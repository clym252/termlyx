//! Session commands: open a shell (on a vault host's shared connection, or
//! a local pty), write/resize/ack/close it, close every session of the
//! calling webview, and list the calling webview's live work.
//!
//! Terminal output goes to the webview as raw binary frames over a
//! `tauri::ipc::Channel` (batched ~16 ms frames, ack-based backpressure in
//! `core_api::SessionManager`). Keystrokes come back the same way: the
//! `write_shell` command takes the bytes as the raw IPC body (a
//! `Uint8Array` passed to `invoke`) and the session id in the
//! [`SESSION_ID_HEADER`] header, never as a JSON number array.
//!
//! Every session is tagged with the label of the webview that opened it, so
//! `close_all_sessions` (called by the UI on load) can close what a reload
//! orphaned.

use std::path::PathBuf;
use std::sync::Arc;

#[cfg(desktop)]
use core_api::LocalShell;
use core_api::{Direction, OutputSink, ShellTarget};
use core_model::Id;
use core_ssh::{
    AuthMethod, ConnectParams, ConnectProgress, ConnectStep, TAB_ENV, tmux_reattach_command,
};
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use tauri::http::HeaderMap;
use tauri::ipc::{Channel, InvokeBody, InvokeResponseBody, Request};
use tauri::{State, Webview};

use crate::forward_cmds::ForwardStatusEntry;
use crate::sink::{ChannelProgress, ChannelSink, SessionClosed};
use crate::state::{AppState, CONNECT_TIMEOUT, CmdError, DEFAULT_TERM, Reaper};

pub use crate::state::SessionMeta;

/// Header carrying the session id of a `write_shell` call.
pub const SESSION_ID_HEADER: &str = "tether-session-id";

/// What to open a session on.
#[derive(Debug, Clone, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum SessionTarget {
    /// A shell on a vault host, over the host's shared connection. With
    /// `reattach_tmux` (a reconnecting tab that was in that tmux session),
    /// the shell attaches to the session, or is the login shell if it is
    /// gone or the host has no tmux.
    Host {
        host_id: Id,
        #[serde(default)]
        reattach_tmux: Option<String>,
    },
    /// A local shell; `cwd` defaults to the app's working directory and
    /// `shell_id` (an id from `local_shells`) to the default shell. Refused
    /// on mobile.
    Local {
        #[serde(default)]
        cwd: Option<PathBuf>,
        #[serde(default)]
        shell_id: Option<String>,
    },
}

/// A shell offered for local terminals. The program and its arguments stay
/// in the backend: the UI only picks by id.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalShellDto {
    pub id: String,
    pub label: String,
}

#[cfg(desktop)]
impl From<&LocalShell> for LocalShellDto {
    fn from(s: &LocalShell) -> Self {
        Self {
            id: s.id.clone(),
            label: s.label.clone(),
        }
    }
}

/// The program and arguments for `shell_id`, looked up with `resolve`
/// ([`core_api::local_shell`] in the app); `None` is the default shell. A
/// shell that has since gone is an error, not a fallback.
#[cfg(desktop)]
fn pick_shell(
    shell_id: Option<&str>,
    resolve: impl FnOnce(&str) -> Option<LocalShell>,
) -> Result<(Option<PathBuf>, Vec<String>), CmdError> {
    let Some(id) = shell_id else {
        return Ok((None, Vec::new()));
    };
    resolve(id)
        .map(|s| (Some(s.program), s.args))
        .ok_or_else(|| CmdError::new("shell", "That shell is no longer available."))
}

/// `local_shells()` off the async runtime: finding WSL distros can block.
#[cfg(desktop)]
async fn list_local_shells() -> Result<Vec<LocalShell>, CmdError> {
    tauri::async_runtime::spawn_blocking(core_api::local_shells)
        .await
        .map_err(|_| CmdError::new("shell", "Could not list the available shells."))
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenedSession {
    pub session_id: String,
}

/// The calling webview's live sessions and running transfers, and the app's
/// running port forwards (for the quit confirmation).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveWork {
    pub sessions: Vec<ActiveSession>,
    pub transfers: Vec<ActiveTransfer>,
    /// Starting or active forwards. App-wide: they are not tied to a window.
    pub forwards: Vec<ForwardStatusEntry>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveSession {
    pub id: String,
    pub title: String,
    pub host_id: Option<Id>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveTransfer {
    pub job_id: String,
    pub host_id: Id,
    pub direction: Direction,
    pub local: String,
    pub remote: String,
}

/// Direct connection parameters (the DEV-only connect form). Secrets
/// deserialize straight into `SecretString`; `Debug` omits them.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConnectParamsDto {
    pub host: String,
    pub port: u16,
    pub username: String,
    #[serde(default)]
    pub password: Option<SecretString>,
    #[serde(default)]
    pub private_key_pem: Option<SecretString>,
    #[serde(default)]
    pub passphrase: Option<SecretString>,
    pub cols: u16,
    pub rows: u16,
}

impl std::fmt::Debug for ConnectParamsDto {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectParamsDto")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("cols", &self.cols)
            .field("rows", &self.rows)
            .finish_non_exhaustive()
    }
}

impl TryFrom<ConnectParamsDto> for ConnectParams {
    type Error = CmdError;

    fn try_from(dto: ConnectParamsDto) -> Result<Self, CmdError> {
        let auth = match (dto.password, dto.private_key_pem) {
            (Some(pw), None) => AuthMethod::Password(pw),
            (None, Some(pem)) => AuthMethod::PrivateKey {
                pem,
                passphrase: dto.passphrase,
            },
            (None, None) => AuthMethod::KeyboardInteractive,
            (Some(_), Some(_)) => {
                return Err(CmdError::invalid(
                    "supply either a password or a private key, not both",
                ));
            }
        };
        Ok(ConnectParams {
            host: dto.host,
            port: dto.port,
            username: dto.username,
            auth,
            term: DEFAULT_TERM.into(),
            cols: dto.cols,
            rows: dto.rows,
            connect_timeout: CONNECT_TIMEOUT,
            keepalive: None,
            env: Vec::new(),
        })
    }
}

/// The session id and keystroke bytes of a `write_shell` call: the raw
/// body, with the id in [`SESSION_ID_HEADER`]. A JSON body is refused, so
/// keystrokes can never silently fall back to number arrays, except on
/// Android, whose IPC cannot carry a raw body (see [`write_args_for`]).
pub fn write_args(headers: &HeaderMap, body: &InvokeBody) -> Result<(String, Vec<u8>), CmdError> {
    write_args_for(headers, body, cfg!(target_os = "android"))
}

/// [`write_args`], with `json_bytes` saying whether a JSON array of bytes
/// counts as the body: Tauri sends every Android `invoke` over postMessage
/// as JSON, so a `Uint8Array` arrives as a number array there.
pub fn write_args_for(
    headers: &HeaderMap,
    body: &InvokeBody,
    json_bytes: bool,
) -> Result<(String, Vec<u8>), CmdError> {
    let id = headers
        .get(SESSION_ID_HEADER)
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty())
        .ok_or_else(|| CmdError::invalid(format!("missing {SESSION_ID_HEADER} header")))?;
    match body {
        InvokeBody::Raw(bytes) => Ok((id.to_owned(), bytes.clone())),
        InvokeBody::Json(serde_json::Value::Array(items)) if json_bytes => items
            .iter()
            .map(|v| v.as_u64().and_then(|n| u8::try_from(n).ok()))
            .collect::<Option<Vec<u8>>>()
            .map(|bytes| (id.to_owned(), bytes))
            .ok_or_else(|| CmdError::invalid("write_shell takes an array of bytes")),
        InvokeBody::Json(_) => Err(CmdError::invalid(
            "write_shell takes the bytes as a raw binary body",
        )),
    }
}

fn channel_sink(
    output: &Channel<InvokeResponseBody>,
    closed: &Channel<SessionClosed>,
) -> impl FnOnce(Reaper) -> Arc<dyn OutputSink> {
    let (output, closed) = (output.clone(), closed.clone());
    move |reaper| Arc::new(ChannelSink::new(output, closed).on_dead(reaper))
}

impl AppState {
    /// Opens a session for webview `owner`.
    #[allow(clippy::too_many_arguments)] // the three channels are the command's arguments
    pub async fn open_session(
        &self,
        owner: &str,
        target: SessionTarget,
        cols: u16,
        rows: u16,
        on_output: Channel<InvokeResponseBody>,
        on_closed: Channel<SessionClosed>,
        on_progress: Channel<ConnectStep>,
    ) -> Result<OpenedSession, CmdError> {
        let progress = ChannelProgress::shared(&on_progress);
        let session_id = match target {
            #[cfg(mobile)]
            SessionTarget::Local { .. } => {
                return Err(CmdError::new(
                    "shell",
                    "Local terminals are not available on mobile.",
                ));
            }
            #[cfg(desktop)]
            SessionTarget::Local { cwd, shell_id } => {
                // By id, without listing the shells: only Settings probes WSL.
                let (shell, args) = pick_shell(shell_id.as_deref(), core_api::local_shell)?;
                let title = match &cwd {
                    Some(dir) => format!("Local: {}", dir.display()),
                    None => "Local shell".to_owned(),
                };
                let target = ShellTarget::Local {
                    shell,
                    args,
                    cwd,
                    cols,
                    rows,
                };
                let meta = SessionMeta {
                    title,
                    host_id: None,
                    tab: None,
                };
                progress.step(ConnectStep::Shell);
                self.open_target(owner, meta, target, channel_sink(&on_output, &on_closed))
                    .await?
            }
            SessionTarget::Host {
                host_id,
                reattach_tmux,
            } => {
                let (title, term) = self.host_label_and_term(host_id).await?;
                // A fresh id per shell, so a tmux client started from it
                // maps back to this session (`session_tmux`).
                let tab = uuid::Uuid::now_v7().simple().to_string();
                let env = vec![(TAB_ENV.to_owned(), tab.clone())];
                let startup = reattach_tmux.as_deref().map(tmux_reattach_command);
                let meta = SessionMeta {
                    title,
                    host_id: Some(host_id),
                    tab: Some(tab),
                };
                // Held until the session is registered (see state.rs).
                // A pooled connection reports nothing; only a new one does.
                let (mut guard, conn) = self.host_connection(host_id, progress.clone()).await?;
                progress.step(ConnectStep::Shell);
                let target = |conn| ShellTarget::Connection {
                    conn,
                    term: term.clone(),
                    cols,
                    rows,
                    env: env.clone(),
                    startup: startup.clone(),
                };
                let first = self
                    .open_target(
                        owner,
                        meta.clone(),
                        target(conn),
                        channel_sink(&on_output, &on_closed),
                    )
                    .await;
                let opened = match first {
                    // The stored connection died since it was last used:
                    // reconnect once.
                    Err(e) if e.code == "closed" => {
                        match self.reconnect(host_id, &mut guard, progress.clone()).await {
                            Ok(conn) => {
                                progress.step(ConnectStep::Shell);
                                self.open_target(
                                    owner,
                                    meta,
                                    target(conn),
                                    channel_sink(&on_output, &on_closed),
                                )
                                .await
                            }
                            Err(e) => Err(e),
                        }
                    }
                    other => other,
                };
                drop(guard);
                // The host's tabs changed: the next `session_tmux` asks again.
                self.forget_tmux_tabs(host_id);
                // The channel failed (refused pty, too many sessions, ...) or
                // the session already ended before it was registered: do not
                // keep a connection nothing uses. A no-op while it is in use.
                self.release_if_idle(host_id).await;
                opened?
            }
        };
        Ok(OpenedSession { session_id })
    }

    /// Opens an SSH shell from direct parameters (the DEV connect form), on
    /// its own connection. Host keys are checked against the vault's known
    /// hosts; missing secrets are prompted for.
    pub async fn open_shell(
        &self,
        owner: &str,
        params: ConnectParamsDto,
        on_output: Channel<InvokeResponseBody>,
        on_closed: Channel<SessionClosed>,
        on_progress: Channel<ConnectStep>,
    ) -> Result<OpenedSession, CmdError> {
        let params = ConnectParams::try_from(params)?;
        let meta = SessionMeta {
            title: format!("{}@{}", params.username, params.host),
            host_id: None,
            tab: None,
        };
        let target = ShellTarget::Ssh {
            params,
            prompter: self.prompts(),
        };
        ChannelProgress(on_progress).step(ConnectStep::Shell);
        let session_id = self
            .open_target(owner, meta, target, channel_sink(&on_output, &on_closed))
            .await?;
        Ok(OpenedSession { session_id })
    }

    /// Live sessions and running transfers of webview `owner`, and every
    /// running port forward.
    pub fn active_work(&self, owner: &str) -> ActiveWork {
        let mut sessions: Vec<ActiveSession> = self
            .sessions_of(owner)
            .into_iter()
            .map(|(id, meta)| ActiveSession {
                id,
                title: meta.title,
                host_id: meta.host_id,
            })
            .collect();
        sessions.sort_by(|a, b| a.id.cmp(&b.id));
        let mut transfers: Vec<ActiveTransfer> = self
            .transfers_of(owner)
            .into_iter()
            .map(|(job_id, t)| ActiveTransfer {
                job_id,
                host_id: t.host_id,
                direction: t.direction,
                local: t.local,
                remote: t.remote,
            })
            .collect();
        transfers.sort_by(|a, b| a.job_id.cmp(&b.job_id));
        let forwards = self
            .forward_statuses()
            .into_iter()
            .filter(|e| e.status.is_running())
            .collect();
        ActiveWork {
            sessions,
            transfers,
            forwards,
        }
    }
}

#[tauri::command]
#[allow(clippy::too_many_arguments)] // three channels, one per stream
pub async fn open_session(
    state: State<'_, AppState>,
    webview: Webview,
    target: SessionTarget,
    cols: u16,
    rows: u16,
    on_output: Channel<InvokeResponseBody>,
    on_closed: Channel<SessionClosed>,
    on_progress: Channel<ConnectStep>,
) -> Result<OpenedSession, CmdError> {
    state
        .open_session(
            webview.label(),
            target,
            cols,
            rows,
            on_output,
            on_closed,
            on_progress,
        )
        .await
}

/// The tmux session the shell of session `id` is attached to, if any (see
/// `AppState::session_tmux`).
#[tauri::command]
pub async fn session_tmux(
    state: State<'_, AppState>,
    webview: Webview,
    id: String,
) -> Result<Option<String>, CmdError> {
    state.session_tmux(webview.label(), &id).await
}

/// DEV connect form: a shell from direct parameters. Only in builds with
/// the `dev-connect` cargo feature (`pnpm tauri dev --features dev-connect`).
#[cfg(feature = "dev-connect")]
#[tauri::command]
pub async fn open_shell(
    state: State<'_, AppState>,
    webview: Webview,
    params: ConnectParamsDto,
    on_output: Channel<InvokeResponseBody>,
    on_closed: Channel<SessionClosed>,
    on_progress: Channel<ConnectStep>,
) -> Result<OpenedSession, CmdError> {
    state
        .open_shell(webview.label(), params, on_output, on_closed, on_progress)
        .await
}

/// Keystrokes: the raw IPC body, session id in [`SESSION_ID_HEADER`].
#[tauri::command]
pub async fn write_shell(
    state: State<'_, AppState>,
    webview: Webview,
    request: Request<'_>,
) -> Result<(), CmdError> {
    let (id, bytes) = write_args(request.headers(), request.body())?;
    state.write_session(webview.label(), &id, bytes).await
}

#[tauri::command]
pub async fn resize_shell(
    state: State<'_, AppState>,
    webview: Webview,
    id: String,
    cols: u16,
    rows: u16,
) -> Result<(), CmdError> {
    state.resize_session(webview.label(), &id, cols, rows).await
}

#[tauri::command]
pub async fn ack_shell(
    state: State<'_, AppState>,
    webview: Webview,
    id: String,
    bytes: usize,
) -> Result<(), CmdError> {
    state.ack_session(webview.label(), &id, bytes)
}

#[tauri::command]
pub async fn close_shell(
    state: State<'_, AppState>,
    webview: Webview,
    id: String,
) -> Result<(), CmdError> {
    state.close_session(webview.label(), &id).await
}

/// Closes every session the calling webview opened; returns how many.
#[tauri::command]
pub async fn close_all_sessions(
    state: State<'_, AppState>,
    webview: Webview,
) -> Result<usize, CmdError> {
    Ok(state.close_all_sessions(webview.label()).await)
}

/// The shells a local terminal can run, default first. Not registered on mobile.
#[cfg(desktop)]
#[tauri::command]
pub async fn local_shells() -> Result<Vec<LocalShellDto>, CmdError> {
    Ok(list_local_shells()
        .await?
        .iter()
        .map(LocalShellDto::from)
        .collect())
}

/// The calling webview's live sessions and running transfers.
#[tauri::command]
pub async fn active_work(
    state: State<'_, AppState>,
    webview: Webview,
) -> Result<ActiveWork, CmdError> {
    Ok(state.active_work(webview.label()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dto(json: &str) -> ConnectParamsDto {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn dto_deserializes_camel_case_and_converts_to_password_auth() {
        let p: ConnectParams =
            dto(r#"{"host":"h","port":22,"username":"u","password":"pw","cols":80,"rows":24}"#)
                .try_into()
                .unwrap();
        assert_eq!((p.host.as_str(), p.port, p.cols, p.rows), ("h", 22, 80, 24));
        assert!(matches!(p.auth, AuthMethod::Password(_)));
        assert_eq!(p.term, "xterm-256color");
    }

    #[test]
    fn dto_private_key_variant() {
        let p: ConnectParams = dto(
            r#"{"host":"h","port":22,"username":"u","privateKeyPem":"PEM","passphrase":"pp","cols":1,"rows":1}"#,
        )
        .try_into()
        .unwrap();
        assert!(matches!(
            p.auth,
            AuthMethod::PrivateKey {
                passphrase: Some(_),
                ..
            }
        ));
    }

    #[test]
    fn dto_without_secret_asks_and_refuses_both() {
        let none = r#"{"host":"h","port":22,"username":"u","cols":1,"rows":1}"#;
        let p = ConnectParams::try_from(dto(none)).unwrap();
        assert!(matches!(p.auth, AuthMethod::KeyboardInteractive));
        let both = r#"{"host":"h","port":22,"username":"u","password":"a","privateKeyPem":"b","cols":1,"rows":1}"#;
        assert!(ConnectParams::try_from(dto(both)).is_err());
    }

    #[test]
    fn debug_never_prints_secrets() {
        let d = dto(
            r#"{"host":"h","port":22,"username":"u","password":"hunter2","privateKeyPem":"PEMDATA","passphrase":"pp","cols":1,"rows":1}"#,
        );
        let shown = format!("{d:?}");
        assert!(
            !shown.contains("hunter2") && !shown.contains("PEMDATA") && !shown.contains("pp\"")
        );
    }

    #[test]
    fn session_target_shapes() {
        let host: SessionTarget = serde_json::from_str(
            r#"{"kind":"host","hostId":"0190b6f6-7d3c-7cc0-8000-000000000001"}"#,
        )
        .unwrap();
        assert!(matches!(host, SessionTarget::Host { .. }));
        let local: SessionTarget = serde_json::from_str(r#"{"kind":"local"}"#).unwrap();
        assert!(matches!(
            local,
            SessionTarget::Local {
                cwd: None,
                shell_id: None
            }
        ));
        let picked: SessionTarget =
            serde_json::from_str(r#"{"kind":"local","shellId":"cmd"}"#).unwrap();
        assert!(matches!(
            picked,
            SessionTarget::Local { shell_id: Some(ref id), .. } if id == "cmd"
        ));
        assert!(serde_json::from_str::<SessionTarget>(r#"{"kind":"local","cdw":"/"}"#).is_err());
    }

    #[cfg(desktop)]
    fn shell(id: &str, program: &str, args: &[&str]) -> LocalShell {
        LocalShell {
            id: id.into(),
            label: id.into(),
            program: program.into(),
            args: args.iter().map(|a| (*a).to_owned()).collect(),
        }
    }

    #[cfg(desktop)]
    #[test]
    fn pick_shell_resolves_by_id() {
        let shells = [
            shell("cmd", "cmd.exe", &[]),
            shell("wsl:U", "wsl.exe", &["-d", "U"]),
        ];
        let resolve = |id: &str| shells.iter().find(|s| s.id == id).cloned();
        assert_eq!(pick_shell(None, resolve).unwrap(), (None, vec![]));
        assert_eq!(
            pick_shell(Some("wsl:U"), resolve).unwrap(),
            (Some("wsl.exe".into()), vec!["-d".into(), "U".into()])
        );
        let gone = pick_shell(Some("pwsh"), resolve).unwrap_err();
        assert_eq!(gone.code, "shell");
        assert_eq!(gone.message, "That shell is no longer available.");
    }

    #[cfg(desktop)]
    #[test]
    fn local_shell_dto_carries_only_id_and_label() {
        let v = serde_json::to_value(LocalShellDto::from(&shell(
            "cmd",
            "C:\\x\\cmd.exe",
            &["-secret"],
        )))
        .unwrap();
        assert_eq!(v, serde_json::json!({"id": "cmd", "label": "cmd"}));
    }
}
