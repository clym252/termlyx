use std::borrow::Cow;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use russh::client::{self, Handle, KeyboardInteractiveAuthResponse, Msg};
use russh::keys::{
    Algorithm, HashAlg, PrivateKey, PrivateKeyWithHashAlg, PublicKeyOrCertificate,
    decode_secret_key,
};
use russh::{Channel, MethodKind};
use secrecy::{ExposeSecret, SecretString};
use tokio::net::TcpStream;
use tokio::sync::watch;
use tracing::{debug, warn};

use crate::connection::{
    CHANNEL_WINDOW, CloseOnDrop, DisconnectSlot, ForwardRoutes, SshConnection,
};
use crate::deadline::HumanDeadline;
use crate::error::SshError;
use crate::hostkey::{HostKeyDecision, HostKeyInfo, HostKeyVerifier};
use crate::progress::{ConnectProgress, ConnectStep};
use crate::prompter::{AuthPrompter, NoPrompter};
use crate::transport::{AuthMethod, ConnectParams, ShellHandle, SshTransport};

/// Upper bound on keyboard-interactive challenge rounds from one server.
const MAX_KBD_ROUNDS: usize = 16;

/// [`SshTransport`] backed by russh.
#[derive(Debug, Clone, Copy, Default)]
pub struct RusshTransport;

/// Shared between the russh handler and the connect logic so the verifier's
/// decision survives russh flattening it into a generic handshake error.
#[derive(Default)]
struct HandshakeState {
    rejected: AtomicBool,
    /// The verifier could not check the key at all.
    unverifiable: AtomicBool,
    /// Why the server's host key could not be described, if it could not.
    encoding_error: Mutex<Option<String>>,
}

/// The russh session's handler. It lives inside the session task, so it
/// must never hold a reference to the [`SshConnection`] (that would keep the
/// connection alive forever), and its callbacks must never wait on a
/// consumer (that would stall every channel).
pub(crate) struct ClientHandler {
    host: String,
    port: u16,
    verifier: Arc<dyn HostKeyVerifier>,
    state: Arc<HandshakeState>,
    /// The key-exchange deadline, paused while the verifier runs.
    deadline: Arc<HumanDeadline>,
    /// Routes `forwarded-tcpip` channels to their `RemoteListener`.
    forwards: Arc<ForwardRoutes>,
    /// Records why the session ended (read by the shell pumps).
    disconnect: DisconnectSlot,
    /// Fires the connection's close signal when the session ends.
    _closed: CloseOnDrop,
}

impl client::Handler for ClientHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let info = match HostKeyInfo::from_server_key(key) {
            Ok(info) => info,
            Err(e) => {
                // Never show the verifier a key we could not describe.
                warn!(error = %e, "server host key could not be encoded");
                *self
                    .state
                    .encoding_error
                    .lock()
                    .unwrap_or_else(|p| p.into_inner()) = Some(e.to_string());
                return Err(russh::Error::UnknownKey);
            }
        };
        let decision = {
            let _human = self.deadline.pause();
            self.verifier.verify(&self.host, self.port, &info).await
        };
        match decision {
            HostKeyDecision::Accept => Ok(true),
            HostKeyDecision::Reject => {
                self.state.rejected.store(true, Ordering::SeqCst);
                Ok(false)
            }
            HostKeyDecision::Unverifiable => {
                self.state.unverifiable.store(true, Ordering::SeqCst);
                Ok(false)
            }
        }
    }

    /// Never waits: queues the channel for its listener or rejects it.
    async fn server_channel_open_forwarded_tcpip(
        &mut self,
        channel: Channel<Msg>,
        connected_address: &str,
        connected_port: u32,
        originator_address: &str,
        originator_port: u32,
        reply: client::ChannelOpenHandle,
        _session: &mut client::Session,
    ) -> Result<(), Self::Error> {
        self.forwards
            .route(
                channel,
                (connected_address, connected_port),
                (originator_address, originator_port),
                reply,
            )
            .await;
        Ok(())
    }

    /// Records why the session ended, then behaves like the default (an
    /// error is returned so the session's join reports it too).
    async fn disconnected(
        &mut self,
        reason: client::DisconnectReason<Self::Error>,
    ) -> Result<(), Self::Error> {
        let (why, result) = match reason {
            client::DisconnectReason::ReceivedDisconnect(info) => {
                let why = if info.message.is_empty() {
                    format!("the server disconnected ({:?})", info.reason_code)
                } else {
                    format!("the server disconnected: {}", info.message)
                };
                (why, Ok(()))
            }
            client::DisconnectReason::Error(e) => (e.to_string(), Err(e)),
        };
        debug!(reason = %why, "ssh session ended");
        self.disconnect
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get_or_insert(why);
        result
    }
}

fn connect_err(e: impl std::fmt::Display) -> SshError {
    SshError::Connect(e.to_string())
}

fn timed_out() -> SshError {
    SshError::Connect("timed out".into())
}

#[async_trait]
impl SshTransport for RusshTransport {
    async fn open_shell(
        &self,
        p: ConnectParams,
        v: Arc<dyn HostKeyVerifier>,
    ) -> Result<ShellHandle, SshError> {
        self.open_shell_with_prompter(p, v, Arc::new(NoPrompter))
            .await
    }

    /// A one-channel connection: the shell keeps it alive, and closing the
    /// shell disconnects it.
    async fn open_shell_with_prompter(
        &self,
        p: ConnectParams,
        v: Arc<dyn HostKeyVerifier>,
        prompter: Arc<dyn AuthPrompter>,
    ) -> Result<ShellHandle, SshError> {
        let conn = self.connect(&p, v, prompter).await?;
        conn.open_shell(&p.term, p.cols, p.rows).await
    }

    async fn connect(
        &self,
        p: &ConnectParams,
        v: Arc<dyn HostKeyVerifier>,
        prompter: Arc<dyn AuthPrompter>,
    ) -> Result<SshConnection, SshError> {
        SshConnection::connect(p, v, prompter).await
    }

    async fn connect_with_progress(
        &self,
        p: &ConnectParams,
        v: Arc<dyn HostKeyVerifier>,
        prompter: Arc<dyn AuthPrompter>,
        progress: Arc<dyn ConnectProgress>,
    ) -> Result<SshConnection, SshError> {
        SshConnection::connect_with_progress(p, v, prompter, progress).await
    }
}

/// TCP connect, key exchange and authentication, reporting each phase to
/// `progress` as it starts. Returns the session handle and the receiver of
/// the close signal (true once the session has ended).
pub(crate) async fn establish(
    p: &ConnectParams,
    v: Arc<dyn HostKeyVerifier>,
    prompter: Arc<dyn AuthPrompter>,
    forwards: Arc<ForwardRoutes>,
    disconnect: DisconnectSlot,
    progress: &dyn ConnectProgress,
) -> Result<(Handle<ClientHandler>, watch::Receiver<bool>), SshError> {
    debug!(host = %p.host, port = p.port, user = %p.username, "connecting");

    // Each phase gets its own `connect_timeout` budget for network waits.
    progress.step(ConnectStep::Tcp);
    let tcp = HumanDeadline::new(p.connect_timeout)
        .run(TcpStream::connect((p.host.as_str(), p.port)))
        .await
        .map_err(|_| timed_out())?
        .map_err(connect_err)?;
    let _ = tcp.set_nodelay(true);

    // Ask for the host-key types already trusted for this host first, so
    // a server with several host keys presents the one we know.
    let known = v.known_algorithms(&p.host, p.port).await;
    let mut config = client::Config {
        keepalive_interval: p.keepalive_interval(),
        keepalive_max: 3,
        // Per-channel flow control (see `connection.rs`).
        window_size: CHANNEL_WINDOW,
        manual_window_adjust: true,
        ..Default::default()
    };
    config.preferred.key = Cow::Owned(prefer_known(&config.preferred.key, &known));
    let config = Arc::new(config);

    let state = Arc::new(HandshakeState::default());
    let kex_deadline = HumanDeadline::new(p.connect_timeout);
    let (closed_tx, closed_rx) = watch::channel(false);
    let handler = ClientHandler {
        host: p.host.clone(),
        port: p.port,
        verifier: v,
        state: state.clone(),
        deadline: kex_deadline.clone(),
        forwards,
        disconnect,
        _closed: CloseOnDrop(closed_tx),
    };

    progress.step(ConnectStep::HostKey);
    let handshake = kex_deadline
        .run(client::connect_stream(config, tcp, handler))
        .await;
    let mut session = match handshake {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => return Err(handshake_err(&state, e)),
        Err(_) => return Err(timed_out()),
    };

    // Authentication gets its own budget; prompts pause it.
    progress.step(ConnectStep::Auth);
    let auth_deadline = HumanDeadline::new(p.connect_timeout);
    let ctx = AuthCtx {
        deadline: &auth_deadline,
        prompter: prompter.as_ref(),
    };
    auth_deadline
        .run(authenticate(&mut session, p, &ctx))
        .await
        .map_err(|_| timed_out())??;
    Ok((session, closed_rx))
}

/// The verifier's decision (or an undescribable key) wins over the generic
/// error russh reports for a refused key.
fn handshake_err(state: &HandshakeState, e: russh::Error) -> SshError {
    if state.unverifiable.load(Ordering::SeqCst) {
        return SshError::HostKeyUnverifiable;
    }
    if state.rejected.load(Ordering::SeqCst) {
        return SshError::HostKeyRejected;
    }
    let encoding = state
        .encoding_error
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .take();
    match encoding {
        Some(msg) => SshError::HostKeyEncoding(msg),
        None => connect_err(e),
    }
}

/// `default` (the host-key algorithm preference order) with the algorithms
/// whose key type is in `known` moved to the front, keeping relative order.
fn prefer_known(default: &[Algorithm], known: &[String]) -> Vec<Algorithm> {
    let (mut first, rest): (Vec<Algorithm>, Vec<Algorithm>) = default
        .iter()
        .cloned()
        .partition(|a| known.iter().any(|k| *k == key_type(a)));
    first.extend(rest);
    first
}

/// The OpenSSH key type an algorithm signs with, as stored for known hosts
/// (every RSA signature variant uses an `ssh-rsa` key).
fn key_type(a: &Algorithm) -> String {
    match a {
        Algorithm::Rsa { .. } => "ssh-rsa".to_owned(),
        other => other.to_string(),
    }
}

/// What authentication needs besides the session and parameters.
struct AuthCtx<'a> {
    /// Paused while the prompter waits on a human.
    deadline: &'a Arc<HumanDeadline>,
    prompter: &'a dyn AuthPrompter,
}

impl AuthCtx<'_> {
    async fn password(&self, user: &str, host: &str) -> Option<SecretString> {
        let _human = self.deadline.pause();
        self.prompter.password(user, host).await
    }

    async fn passphrase(&self, key_label: &str) -> Option<SecretString> {
        let _human = self.deadline.pause();
        self.prompter.passphrase(key_label).await
    }

    async fn keyboard_interactive(
        &self,
        name: &str,
        instructions: &str,
        prompts: &[(String, bool)],
    ) -> Option<Vec<SecretString>> {
        let _human = self.deadline.pause();
        self.prompter
            .keyboard_interactive(name, instructions, prompts)
            .await
    }
}

async fn authenticate(
    session: &mut Handle<ClientHandler>,
    p: &ConnectParams,
    ctx: &AuthCtx<'_>,
) -> Result<(), SshError> {
    let ok = match &p.auth {
        AuthMethod::Password(pw) => stored_password_auth(session, p, pw, ctx).await?,
        AuthMethod::PrivateKey { pem, passphrase } => {
            let key = load_private_key(pem, passphrase.as_ref(), ctx).await?;
            let hash = session
                .best_supported_rsa_hash()
                .await
                .map_err(connect_err)?
                .flatten();
            session
                .authenticate_publickey(
                    p.username.clone(),
                    PrivateKeyWithHashAlg::new(Arc::new(key), hash),
                )
                .await
                .map_err(connect_err)?
                .success()
        }
        AuthMethod::KeyboardInteractive => ask_auth(session, p, ctx).await?,
    };
    if ok {
        Ok(())
    } else {
        Err(SshError::AuthFailed)
    }
}

async fn password_auth(
    session: &mut Handle<ClientHandler>,
    user: &str,
    pw: &SecretString,
) -> Result<bool, SshError> {
    Ok(session
        .authenticate_password(user.to_owned(), pw.expose_secret().to_owned())
        .await
        .map_err(connect_err)?
        .success())
}

/// `AuthMethod::Password`: a stored password. Sent as a password if the
/// server takes one, else as the answer to a keyboard-interactive round with
/// a single hidden prompt (servers that only do keyboard-interactive). If
/// the server refuses it before the human saw any prompt, the human is asked
/// as for [`AuthMethod::KeyboardInteractive`] (a stale saved password should
/// not lock the user out).
async fn stored_password_auth(
    session: &mut Handle<ClientHandler>,
    p: &ConnectParams,
    pw: &SecretString,
    ctx: &AuthCtx<'_>,
) -> Result<bool, SshError> {
    let methods = match session
        .authenticate_none(p.username.clone())
        .await
        .map_err(connect_err)?
    {
        russh::client::AuthResult::Success => return Ok(true),
        russh::client::AuthResult::Failure {
            remaining_methods, ..
        } => remaining_methods,
    };
    if methods.contains(&MethodKind::Password) {
        if password_auth(session, &p.username, pw).await? {
            return Ok(true);
        }
    } else if methods.contains(&MethodKind::KeyboardInteractive) {
        match keyboard_interactive(session, &p.username, ctx, Some(pw)).await? {
            Kbd::Success => return Ok(true),
            // The stored password was sent and refused (perhaps along with
            // a code the human typed): ask the human for everything.
            Kbd::Failed {
                used_stored: true, ..
            } => {}
            // Only the human answered and the server said no.
            Kbd::Failed { asked: true, .. } => return Ok(false),
            Kbd::Failed { asked: false, .. } => {}
        }
    } else {
        return Ok(false);
    }
    debug!("stored password refused; asking");
    ask_auth(session, p, ctx).await
}

/// Decodes the key, asking for its passphrase if it is encrypted and none
/// was supplied.
async fn load_private_key(
    pem: &SecretString,
    passphrase: Option<&SecretString>,
    ctx: &AuthCtx<'_>,
) -> Result<PrivateKey, SshError> {
    let pem = pem.expose_secret();
    let asked;
    let passphrase = match passphrase {
        Some(pp) => pp,
        None => match decode_secret_key(pem, None) {
            Ok(key) => return Ok(key),
            Err(russh::keys::Error::KeyIsEncrypted) => {
                asked = ctx
                    .passphrase(&key_label(pem))
                    .await
                    .ok_or(SshError::AuthFailed)?;
                &asked
            }
            Err(e) => {
                debug!(error = %e, "private key could not be decoded");
                return Err(SshError::AuthFailed);
            }
        },
    };
    decode_secret_key(pem, Some(passphrase.expose_secret())).map_err(|e| {
        // Bad key material or wrong passphrase: we cannot authenticate.
        debug!(error = %e, "private key could not be decoded");
        SshError::AuthFailed
    })
}

/// How a passphrase prompt names a key: its SHA-256 fingerprint where the
/// format stores the public half in the clear (OpenSSH), else a generic name.
fn key_label(pem: &str) -> String {
    PrivateKey::from_openssh(pem)
        .map(|k| k.public_key().fingerprint(HashAlg::Sha256).to_string())
        .unwrap_or_else(|_| "private key".to_owned())
}

/// `AuthMethod::KeyboardInteractive`: no stored secret, ask the human.
async fn ask_auth(
    session: &mut Handle<ClientHandler>,
    p: &ConnectParams,
    ctx: &AuthCtx<'_>,
) -> Result<bool, SshError> {
    let methods = match session
        .authenticate_none(p.username.clone())
        .await
        .map_err(connect_err)?
    {
        russh::client::AuthResult::Success => return Ok(true),
        russh::client::AuthResult::Failure {
            remaining_methods, ..
        } => remaining_methods,
    };
    let mut password_offered = methods.contains(&MethodKind::Password);
    if methods.contains(&MethodKind::KeyboardInteractive) {
        match keyboard_interactive(session, &p.username, ctx, None).await? {
            Kbd::Success => return Ok(true),
            // The user answered and the server said no: do not ask again.
            Kbd::Failed { asked: true, .. } => return Ok(false),
            // Failed before asking anything (e.g. sshd without PAM): fall
            // back to a password, if the server still takes one.
            Kbd::Failed {
                asked: false,
                remaining,
                ..
            } => password_offered = remaining.contains(&MethodKind::Password),
        }
    }
    if !password_offered {
        return Ok(false);
    }
    let pw = ctx
        .password(&p.username, &p.host)
        .await
        .ok_or(SshError::AuthFailed)?;
    password_auth(session, &p.username, &pw).await
}

enum Kbd {
    Success,
    Failed {
        /// Whether the user was shown at least one prompt.
        asked: bool,
        /// Whether the stored password answered a round.
        used_stored: bool,
        remaining: russh::MethodSet,
    },
}

/// Runs keyboard-interactive, asking the human each round. `stored` answers
/// the first round that has exactly one hidden prompt instead (once).
async fn keyboard_interactive(
    session: &mut Handle<ClientHandler>,
    user: &str,
    ctx: &AuthCtx<'_>,
    mut stored: Option<&SecretString>,
) -> Result<Kbd, SshError> {
    let mut asked = false;
    let mut used_stored = false;
    let mut reply = session
        .authenticate_keyboard_interactive_start(user.to_owned(), None)
        .await
        .map_err(connect_err)?;
    for _ in 0..MAX_KBD_ROUNDS {
        let (name, instructions, prompts) = match reply {
            KeyboardInteractiveAuthResponse::Success => return Ok(Kbd::Success),
            KeyboardInteractiveAuthResponse::Failure {
                remaining_methods, ..
            } => {
                return Ok(Kbd::Failed {
                    asked,
                    used_stored,
                    remaining: remaining_methods,
                });
            }
            KeyboardInteractiveAuthResponse::InfoRequest {
                name,
                instructions,
                prompts,
            } => (name, instructions, prompts),
        };
        let answers: Vec<String> = if prompts.is_empty() {
            // Servers may send an empty round (e.g. a banner); answer it
            // without bothering the user.
            Vec::new()
        } else if let (Some(pw), [q]) = (stored, prompts.as_slice())
            && !q.echo
        {
            stored = None;
            used_stored = true;
            vec![pw.expose_secret().to_owned()]
        } else {
            asked = true;
            let prompts: Vec<(String, bool)> =
                prompts.into_iter().map(|q| (q.prompt, q.echo)).collect();
            let answers = ctx
                .keyboard_interactive(&name, &instructions, &prompts)
                .await
                .ok_or(SshError::AuthFailed)?;
            if answers.len() != prompts.len() {
                return Err(SshError::AuthFailed);
            }
            answers
                .iter()
                .map(|a| a.expose_secret().to_owned())
                .collect()
        };
        reply = session
            .authenticate_keyboard_interactive_respond(answers)
            .await
            .map_err(connect_err)?;
    }
    warn!("server sent too many keyboard-interactive rounds");
    Err(SshError::AuthFailed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handshake_errors_keep_their_cause() {
        let state = HandshakeState::default();
        assert!(matches!(
            handshake_err(&state, russh::Error::Disconnect),
            SshError::Connect(_)
        ));

        *state.encoding_error.lock().unwrap() = Some("bad key".into());
        assert!(matches!(
            handshake_err(&state, russh::Error::UnknownKey),
            SshError::HostKeyEncoding(m) if m == "bad key"
        ));

        state.rejected.store(true, Ordering::SeqCst);
        assert!(matches!(
            handshake_err(&state, russh::Error::UnknownKey),
            SshError::HostKeyRejected
        ));

        state.unverifiable.store(true, Ordering::SeqCst);
        assert!(matches!(
            handshake_err(&state, russh::Error::UnknownKey),
            SshError::HostKeyUnverifiable
        ));
    }

    #[test]
    fn known_host_key_types_move_to_the_front() {
        let default = client::Config::default().preferred.key.to_vec();
        assert_eq!(prefer_known(&default, &[]), default);
        assert_eq!(prefer_known(&default, &["unheard-of".into()]), default);

        let ecdsa = prefer_known(&default, &["ecdsa-sha2-nistp384".into()]);
        assert_eq!(ecdsa[0].to_string(), "ecdsa-sha2-nistp384");
        assert_eq!(ecdsa.len(), default.len());

        // `ssh-rsa` keys cover every RSA signature algorithm, in order.
        let rsa = prefer_known(&default, &["ssh-rsa".into(), "ssh-ed25519".into()]);
        let names: Vec<String> = rsa.iter().map(ToString::to_string).collect();
        assert_eq!(
            names[..4],
            ["ssh-ed25519", "rsa-sha2-512", "rsa-sha2-256", "ssh-rsa"]
        );
    }
}
