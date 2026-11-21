//! Application state behind every command: the vault, the prompt bridge,
//! live sessions (tagged with the webview that opened them), one shared
//! `SshConnection` per host, SFTP clients with their transfer queues, the
//! port-forward manager, the sync engine (see `sync_cmds`), and keep-alive,
//! which follows the live sessions and forwards (`keepalive`).
//!
//! Nothing here depends on a running Tauri app: events go out through an
//! [`EventSink`], and session output through whatever [`OutputSink`] the
//! caller builds, so the state is testable on its own (see
//! `tests/command_layer.rs`).
//!
//! Locking rules:
//! * The vault's `std::sync::Mutex` is only taken inside
//!   [`AppState::with_vault`]'s blocking task, never across an `.await`.
//! * The other `std::sync::Mutex`es guard plain maps and are never held
//!   across an `.await` either.
//! * The sync engine's slot is a `tokio::sync::RwLock` (`sync_cmds`): the
//!   flows that talk to the server hold it shared while they run; starting
//!   and stopping the engine takes it exclusively and keeps it for the whole
//!   sequence (turn off, replacing join), so two engines never overlap. The vault lock is still
//!   only taken inside a blocking task (`with_vault`, or the flows' own).
//! * A host's connection slot is a `tokio::sync::Mutex`: it is held while
//!   connecting (so one host gets one connection) and while a caller opens
//!   its channel and registers it (so an idle-release cannot drop the
//!   connection in between).
//! * A host's tmux tab map is a `tokio::sync::Mutex` too, held while the
//!   host is asked for it, so tabs polling together share one `exec`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

use core_api::vault::{SharedVault, VaultError, VaultService};
use core_api::{
    Direction, ForwardEvent, ForwardManager, OutputSink, PromptBridge, SavingPrompter, SessionId,
    SessionManager, ShellTarget, TransferQueue, VaultHostKeyVerifier,
};
use core_model::Id;
use core_ssh::keys::KeyError;
use core_ssh::{
    ClosedSignal, ConnectProgress, HostKeyDecision, HostKeyInfo, HostKeyVerifier, SftpClient,
    SshConnection, SshError, SshTransport, parse_tmux_tabs, tmux_tabs_command,
};
use core_store::StoreError;
use core_sync::{KeyringTokenStore, TokenStore};
use serde::Serialize;
use tokio::sync::OwnedMutexGuard;

/// Event names sent to the UI.
pub mod events {
    pub const PROMPT: &str = "prompt";
    pub const PROMPT_WITHDRAWN: &str = "prompt-withdrawn";
    pub const TRANSFER: &str = "transfer";
    pub const FORWARD: &str = "forward";
    pub const VAULT_LOCKED: &str = "vault-locked";
    pub const VAULT_CHANGED: &str = "vault-changed";
    pub const SYNC_STATUS: &str = "sync-status";
    pub const KEEPALIVE: &str = "keepalive";
}

/// Bounds the network waits of each connect phase (see
/// `ConnectParams::connect_timeout`).
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// How long a host's tab → tmux session map answers `session_tmux` before
/// the host is asked again.
pub const TMUX_TABS_TTL: Duration = Duration::from_secs(10);
/// Cap on the tmux listing's output (one short line per tmux client).
const TMUX_TABS_MAX_BYTES: usize = 64 * 1024;

/// Terminal type when no level of the settings chain sets one.
pub const DEFAULT_TERM: &str = "xterm-256color";

/// Where events for the UI go (the app: `AppHandle::emit`).
pub trait EventSink: Send + Sync + 'static {
    /// Sends `payload` as event `event`. An `Err` means nobody can receive
    /// it (the webview is gone).
    fn emit(&self, event: &str, payload: serde_json::Value) -> Result<(), String>;
}

/// The vault as found at startup. A vault file that does not open leaves the
/// app running with [`VaultSlot::Failed`]: `vault_status` reports the error,
/// vault commands fail with `vaultUnavailable`, and local shells still work.
pub enum VaultSlot {
    Ready(SharedVault),
    /// Why the vault could not be opened (no secrets: a path and an error).
    Failed(String),
}

/// The error every command returns. Serializes as
/// `{"code": "...", "message": "...", "refs": [...]}`; `refs` lists the ids
/// involved (for `referenced`: the records still using the one being
/// deleted). Never carries a secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CmdError {
    pub code: String,
    pub message: String,
    pub refs: Vec<String>,
}

impl std::fmt::Display for CmdError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for CmdError {}

impl CmdError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_owned(),
            message: message.into(),
            refs: Vec::new(),
        }
    }

    /// Bad input from the UI (unknown kind, malformed JSON, ...).
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new("invalid", message)
    }

    pub fn not_found(what: impl std::fmt::Display) -> Self {
        Self::new("notFound", format!("{what} not found"))
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new("internal", message)
    }

    pub fn io(message: impl Into<String>) -> Self {
        Self::new("io", message)
    }

    /// Still in use by `ids`; nothing was changed.
    pub fn referenced(message: impl Into<String>, ids: Vec<Id>) -> Self {
        Self {
            refs: ids.iter().map(Id::to_string).collect(),
            ..Self::new("referenced", message)
        }
    }

    fn with_refs(mut self, ids: &[Id]) -> Self {
        self.refs = ids.iter().map(Id::to_string).collect();
        self
    }
}

impl From<VaultError> for CmdError {
    fn from(e: VaultError) -> Self {
        let message = e.to_string();
        match &e {
            VaultError::Locked => Self::new("locked", message),
            VaultError::NotInitialized => Self::new("notInitialized", message),
            VaultError::AlreadyInitialized => Self::new("alreadyInitialized", message),
            VaultError::WrongPassword => Self::new("wrongPassword", message),
            VaultError::WrongRecoveryKey => Self::new("wrongRecoveryKey", message),
            VaultError::Referenced(ids) => Self::referenced(message, ids.clone()),
            VaultError::MissingRef(id) => Self::new("missingRef", message).with_refs(&[*id]),
            VaultError::IdConflict(id) => Self::new("idConflict", message).with_refs(&[*id]),
            VaultError::Cycle => Self::new("cycle", message),
            VaultError::Invalid(_) => Self::new("invalid", message),
            VaultError::ForeignOwnLogin(id) => Self::new("invalid", message).with_refs(&[*id]),
            VaultError::QuickUnlockNotEnabled => Self::new("quickUnlockNotEnabled", message),
            VaultError::Keychain(_) => Self::new("keychain", message),
            VaultError::BiometryUnavailable(_) => Self::new("biometryUnavailable", message),
            VaultError::BiometryChanged => Self::new("biometryChanged", message),
            VaultError::PresenceDenied => Self::new("presenceDenied", message),
            VaultError::BadRecord(id) => Self::new("corrupt", message).with_refs(&[*id]),
            VaultError::BadAccount(_) | VaultError::BadMaterial(_) => Self::new("corrupt", message),
            VaultError::WeakKdf => Self::new("weakKdf", message),
            VaultError::Store(StoreError::NotFound { id }) => {
                Self::new("notFound", message).with_refs(&[*id])
            }
            VaultError::Store(StoreError::Corrupt { id }) => {
                Self::new("corrupt", message).with_refs(&[*id])
            }
            VaultError::Store(_) => Self::new("storage", message),
            VaultError::Crypto(_) => Self::new("crypto", message),
        }
    }
}

impl From<SshError> for CmdError {
    fn from(e: SshError) -> Self {
        let code = match &e {
            SshError::Connect(_) => "connect",
            SshError::AuthFailed => "authFailed",
            SshError::HostKeyRejected => "hostKeyRejected",
            SshError::HostKeyUnverifiable => "hostKeyUnverifiable",
            SshError::HostKeyEncoding(_) => "hostKeyEncoding",
            SshError::Channel(_) => "channel",
            SshError::Sftp(_) => "sftp",
            SshError::Closed => "closed",
        };
        Self::new(code, e.to_string())
    }
}

impl From<KeyError> for CmdError {
    fn from(e: KeyError) -> Self {
        let code = match &e {
            KeyError::NeedsPassphrase => "needsPassphrase",
            KeyError::BadPassphrase => "badPassphrase",
            KeyError::Unsupported(_) | KeyError::Parse(_) => "key",
        };
        Self::new(code, e.to_string())
    }
}

/// What a session is, for `active_work` and idle-connection release.
#[derive(Debug, Clone)]
pub struct SessionMeta {
    /// Shown in the quit confirmation (host label, "Local shell", ...).
    pub title: String,
    /// The vault host whose shared connection the session uses.
    pub host_id: Option<Id>,
    /// A host shell's tab id (its `LC_TETHER_TAB`), which maps a tmux client
    /// started from it back to the session.
    pub tab: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct SessionInfo {
    pub owner: String,
    pub meta: SessionMeta,
}

/// A queued or running transfer, for `active_work`.
#[derive(Debug, Clone)]
pub(crate) struct TransferInfo {
    pub owner: String,
    pub host_id: Id,
    pub direction: Direction,
    pub local: String,
    pub remote: String,
}

/// An open SFTP client and its transfer queue for one host.
pub(crate) struct SftpHost {
    pub client: SftpClient,
    pub queue: TransferQueue,
    pub closed: ClosedSignal,
}

type ConnSlot = Arc<tokio::sync::Mutex<Option<SshConnection>>>;

/// A host's tab id → tmux session map, and when it was read.
type TmuxSlot = Arc<tokio::sync::Mutex<Option<(Instant, HashMap<String, String>)>>>;

/// Hands-off state between opening a session and its sink: the session id
/// once known, whether the session already ended, and whether a reap (close
/// because the UI is gone) was asked for before the id was known.
#[derive(Default)]
struct SlotState {
    id: Option<SessionId>,
    ended: bool,
    reap: bool,
}

/// Lets an output sink close its own session when the UI side is gone (its
/// channel send failed): the backstop for orphaned sessions. Cheap to clone;
/// only the first `reap` does anything.
#[derive(Clone)]
pub struct Reaper {
    state: Weak<Inner>,
    slot: Arc<Mutex<SlotState>>,
}

impl Reaper {
    /// Closes the session in the background (now, or as soon as its id is
    /// known).
    pub fn reap(&self) {
        let id = {
            let mut slot = lock(&self.slot);
            if slot.reap || slot.ended {
                return;
            }
            slot.reap = true;
            slot.id.clone()
        };
        if let Some(id) = id {
            let state = self.state.clone();
            tauri::async_runtime::spawn(async move {
                if let Some(inner) = state.upgrade() {
                    tracing::debug!(session = %id, "output channel gone; closing session");
                    AppState(inner).close_session_unchecked(&id).await;
                }
            });
        }
    }
}

/// Wraps the caller's sink: forgets the session (and releases an idle host
/// connection) once it has ended.
struct TrackedSink {
    inner: Arc<dyn OutputSink>,
    slot: Arc<Mutex<SlotState>>,
    state: Weak<Inner>,
}

impl OutputSink for TrackedSink {
    fn frame(&self, bytes: Vec<u8>) {
        self.inner.frame(bytes);
    }

    fn closed(&self, reason: Option<String>, exit_status: Option<u32>) {
        let mut released_host = None;
        match self.state.upgrade() {
            Some(inner) => {
                let mut registry = lock(&inner.registry);
                let mut slot = lock(&self.slot);
                slot.ended = true;
                if let Some(id) = &slot.id
                    && let Some(info) = registry.remove(id)
                {
                    released_host = info.meta.host_id;
                }
                drop(slot);
                drop(registry);
                AppState(inner.clone()).refresh_keepalive();
                if let Some(host) = released_host {
                    tauri::async_runtime::spawn(async move {
                        AppState(inner).release_if_idle(host).await;
                    });
                }
            }
            None => lock(&self.slot).ended = true,
        }
        self.inner.closed(reason, exit_status);
    }
}

pub(crate) struct Inner {
    vault: VaultSlot,
    prompts: Arc<PromptBridge>,
    events: Arc<dyn EventSink>,
    transport: Arc<dyn SshTransport>,
    sessions: SessionManager,
    registry: Mutex<HashMap<SessionId, SessionInfo>>,
    connections: Mutex<HashMap<Id, ConnSlot>>,
    tmux: Mutex<HashMap<Id, TmuxSlot>>,
    pub(crate) sftp: Mutex<HashMap<Id, Arc<SftpHost>>>,
    pub(crate) transfers: Mutex<HashMap<String, TransferInfo>>,
    pub(crate) forwards: ForwardManager,
    /// Rule id → host id of every rule started in this run.
    pub(crate) forward_hosts: Mutex<HashMap<Id, Id>>,
    /// Where the sync device credentials live (the OS keychain in the app).
    pub(crate) tokens: Arc<dyn TokenStore>,
    pub(crate) sync: crate::sync_cmds::SyncSlot,
    /// The auto-lock's clock (`autolock`).
    autolock: Arc<crate::autolock::AutoLockSlot>,
    /// Background keep-alive (`keepalive`): follows the live sessions and forwards.
    pub(crate) keepalive: Arc<crate::keepalive::KeepAliveSlot>,
}

/// Shared application state; see the module docs. Cheap to clone.
#[derive(Clone)]
pub struct AppState(pub(crate) Arc<Inner>);

/// Locks a map mutex. Their holders never panic midway, but a poisoned map is
/// still consistent, so poison is ignored.
pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl AppState {
    /// Builds the state with the OS keychain as the sync credential store.
    /// Spawns the task that turns forward-rule status changes into `forward`
    /// events.
    pub fn new(
        vault: VaultSlot,
        prompts: Arc<PromptBridge>,
        events: Arc<dyn EventSink>,
        transport: Arc<dyn SshTransport>,
    ) -> Self {
        Self::with_token_store(
            vault,
            prompts,
            events,
            transport,
            Arc::new(KeyringTokenStore::new()),
        )
    }

    /// [`AppState::new`] with an explicit sync credential store (tests use an
    /// in-memory one so they never touch the keychain).
    pub fn with_token_store(
        vault: VaultSlot,
        prompts: Arc<PromptBridge>,
        events: Arc<dyn EventSink>,
        transport: Arc<dyn SshTransport>,
        tokens: Arc<dyn TokenStore>,
    ) -> Self {
        let (forwards, mut forward_events) = ForwardManager::new();
        let inner = Arc::new(Inner {
            vault,
            prompts,
            events,
            sessions: SessionManager::new(transport.clone()),
            transport,
            registry: Mutex::default(),
            connections: Mutex::default(),
            tmux: Mutex::default(),
            sftp: Mutex::default(),
            transfers: Mutex::default(),
            forwards,
            forward_hosts: Mutex::default(),
            tokens,
            sync: Default::default(),
            autolock: Default::default(),
            keepalive: Default::default(),
        });
        inner.keepalive.set_events(inner.events.clone());
        let weak = Arc::downgrade(&inner);
        tauri::async_runtime::spawn(async move {
            // Ends when the manager (inside the state) is dropped.
            while let Some(event) = forward_events.recv().await {
                let Some(inner) = weak.upgrade() else { break };
                let state = AppState(inner);
                state.emit(events::FORWARD, &event);
                state.refresh_keepalive();
                // Spawned: a release can wait on the host's slot while a
                // connect there sits on a prompt, and must not hold up the
                // events of every other rule.
                if !event.status.is_running() {
                    tauri::async_runtime::spawn(async move {
                        state.after_forward_event(&event).await;
                    });
                }
            }
        });
        Self(inner)
    }

    /// Tether came to the front (focused, or its page shown): resumes a sync
    /// paused while the device was locked (iOS keychain).
    pub(crate) fn came_to_front(&self) {
        self.0.sync.foreground();
    }

    /// The auto-lock (window events, the webview's `app_front`, its timer).
    pub fn autolock(&self) -> Arc<crate::autolock::AutoLockSlot> {
        self.0.autolock.clone()
    }

    pub(crate) fn prompts(&self) -> Arc<PromptBridge> {
        self.0.prompts.clone()
    }

    /// Sends an event to the UI; a failure is logged (nobody is listening).
    pub(crate) fn emit<T: Serialize>(&self, event: &str, payload: &T) {
        let payload = match serde_json::to_value(payload) {
            Ok(p) => p,
            Err(e) => {
                tracing::error!(event, error = %e, "event payload does not serialize");
                return;
            }
        };
        if let Err(e) = self.0.events.emit(event, payload) {
            tracing::debug!(event, error = %e, "event not delivered");
        }
    }

    // -- vault -------------------------------------------------------------

    pub(crate) fn vault_slot(&self) -> &VaultSlot {
        &self.0.vault
    }

    pub(crate) fn shared_vault(&self) -> Result<SharedVault, CmdError> {
        match &self.0.vault {
            VaultSlot::Ready(v) => Ok(v.clone()),
            VaultSlot::Failed(why) => Err(CmdError::new(
                "vaultUnavailable",
                format!("the vault could not be opened: {why}"),
            )),
        }
    }

    /// Runs `f` on the vault in a blocking task (Argon2 and SQLite never
    /// run on an async worker). The vault lock is held only inside `f`.
    pub async fn with_vault<T, F>(&self, f: F) -> Result<T, CmdError>
    where
        T: Send + 'static,
        F: FnOnce(&mut VaultService) -> Result<T, CmdError> + Send + 'static,
    {
        let vault = self.shared_vault()?;
        tauri::async_runtime::spawn_blocking(move || {
            let mut guard = lock(&vault);
            f(&mut guard)
        })
        .await
        .map_err(|e| CmdError::internal(format!("vault task failed: {e}")))?
    }

    /// [`AppState::with_vault`] for commands that change records: afterwards
    /// the sync engine is nudged to push (also after a failure, which may
    /// have written part of its work).
    pub async fn with_vault_write<T, F>(&self, f: F) -> Result<T, CmdError>
    where
        T: Send + 'static,
        F: FnOnce(&mut VaultService) -> Result<T, CmdError> + Send + 'static,
    {
        let result = self.with_vault(f).await;
        self.0.sync.local_change();
        result
    }

    /// The verifier for SSH host keys: the vault's known hosts, asking the
    /// user through the prompt bridge.
    fn host_key_verifier(&self) -> Result<Arc<dyn HostKeyVerifier>, CmdError> {
        Ok(Arc::new(VaultHostKeyVerifier::new(
            self.shared_vault()?,
            self.0.prompts.clone(),
        )))
    }

    // -- sessions ----------------------------------------------------------

    /// Opens a session on `target` for webview `owner` and registers it.
    /// `make_sink` builds the output sink; the [`Reaper`] it gets closes the
    /// session if the sink finds the UI gone.
    pub async fn open_target(
        &self,
        owner: &str,
        meta: SessionMeta,
        target: ShellTarget,
        make_sink: impl FnOnce(Reaper) -> Arc<dyn OutputSink>,
    ) -> Result<SessionId, CmdError> {
        let verifier: Arc<dyn HostKeyVerifier> = match &target {
            ShellTarget::Ssh { .. } => self.host_key_verifier()?,
            // Local shells and existing connections never check a host key.
            ShellTarget::Connection { .. } => Arc::new(RejectAll),
            #[cfg(desktop)]
            ShellTarget::Local { .. } => Arc::new(RejectAll),
        };
        let slot = Arc::new(Mutex::new(SlotState::default()));
        let reaper = Reaper {
            state: Arc::downgrade(&self.0),
            slot: slot.clone(),
        };
        let sink = Arc::new(TrackedSink {
            inner: make_sink(reaper),
            slot: slot.clone(),
            state: Arc::downgrade(&self.0),
        });
        let verifies_host_key = matches!(&target, ShellTarget::Ssh { .. });
        let opened = self.0.sessions.open(target, verifier, sink).await;
        if verifies_host_key {
            // Accepting an unknown host key stores it in the vault.
            self.0.sync.local_change();
        }
        let id = opened?;
        let reap_now = {
            let mut registry = lock(&self.0.registry);
            let mut slot = lock(&slot);
            slot.id = Some(id.clone());
            if !slot.ended {
                registry.insert(
                    id.clone(),
                    SessionInfo {
                        owner: owner.to_owned(),
                        meta,
                    },
                );
            }
            slot.reap && !slot.ended
        };
        self.refresh_keepalive();
        if reap_now {
            self.close_session_unchecked(&id).await;
        }
        Ok(id)
    }

    /// Ids of the live sessions opened by webview `owner`.
    pub fn session_ids(&self, owner: &str) -> Vec<SessionId> {
        lock(&self.0.registry)
            .iter()
            .filter(|(_, info)| info.owner == owner)
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// How many sessions are open, whoever owns them.
    pub(crate) fn session_count(&self) -> usize {
        lock(&self.0.registry).len()
    }

    pub(crate) fn sessions_of(&self, owner: &str) -> Vec<(SessionId, SessionMeta)> {
        lock(&self.0.registry)
            .iter()
            .filter(|(_, info)| info.owner == owner)
            .map(|(id, info)| (id.clone(), info.meta.clone()))
            .collect()
    }

    pub(crate) fn transfers_of(&self, owner: &str) -> Vec<(String, TransferInfo)> {
        lock(&self.0.transfers)
            .iter()
            .filter(|(_, info)| info.owner == owner)
            .map(|(id, info)| (id.clone(), info.clone()))
            .collect()
    }

    /// Refuses a session that another webview opened. An unknown id
    /// passes: the session calls report it as closed (or ignore it).
    fn check_owner(&self, owner: &str, id: &str) -> Result<(), CmdError> {
        match lock(&self.0.registry).get(id) {
            Some(info) if info.owner != owner => Err(CmdError::new(
                "notOwner",
                "the session belongs to another window",
            )),
            _ => Ok(()),
        }
    }

    /// Writes keystrokes to a session of webview `owner`.
    pub async fn write_session(
        &self,
        owner: &str,
        id: &str,
        data: Vec<u8>,
    ) -> Result<(), CmdError> {
        self.check_owner(owner, id)?;
        Ok(self.0.sessions.write(id, data).await?)
    }

    pub async fn resize_session(
        &self,
        owner: &str,
        id: &str,
        cols: u16,
        rows: u16,
    ) -> Result<(), CmdError> {
        self.check_owner(owner, id)?;
        Ok(self.0.sessions.resize(id, cols, rows).await?)
    }

    /// Acknowledges delivered output; unknown ids are ignored.
    pub fn ack_session(&self, owner: &str, id: &str, bytes: usize) -> Result<(), CmdError> {
        self.check_owner(owner, id)?;
        self.0.sessions.ack(id, bytes);
        Ok(())
    }

    /// Closes a session of webview `owner`; its sink has heard `closed` when
    /// this returns. Unknown ids are a no-op.
    pub async fn close_session(&self, owner: &str, id: &str) -> Result<(), CmdError> {
        self.check_owner(owner, id)?;
        self.close_session_unchecked(id).await;
        Ok(())
    }

    /// The tmux session that the shell of session `id` (of webview `owner`)
    /// is attached to: `None` for a plain shell, a local one, or a host
    /// where tmux cannot be asked (no tmux, no `/proc`). The host's map is
    /// read with one `exec` on its connection, kept for [`TMUX_TABS_TTL`]
    /// and dropped whenever a shell opens there. An error (the session or
    /// its connection gone, the connection busy connecting) means "not
    /// known now", not "no tmux".
    pub async fn session_tmux(&self, owner: &str, id: &str) -> Result<Option<String>, CmdError> {
        let meta = match lock(&self.0.registry).get(id) {
            Some(info) if info.owner != owner => {
                return Err(CmdError::new(
                    "notOwner",
                    "the session belongs to another window",
                ));
            }
            Some(info) => info.meta.clone(),
            None => return Err(SshError::Closed.into()),
        };
        let (Some(host_id), Some(tab)) = (meta.host_id, meta.tab) else {
            return Ok(None);
        };
        let slot = lock(&self.0.tmux).entry(host_id).or_default().clone();
        let mut cached = slot.lock().await;
        if let Some((read_at, tabs)) = cached.as_ref()
            && read_at.elapsed() < TMUX_TABS_TTL
        {
            return Ok(tabs.get(&tab).cloned());
        }
        let conn = self.live_connection(host_id)?;
        let out = conn
            .exec_capture(tmux_tabs_command(), TMUX_TABS_MAX_BYTES)
            .await
            .inspect_err(|e| tracing::debug!(host = %host_id, error = %e, "tmux tabs not read"))?;
        let tabs = parse_tmux_tabs(&out.stdout);
        let found = tabs.get(&tab).cloned();
        *cached = Some((Instant::now(), tabs));
        Ok(found)
    }

    /// Drops the host's tmux tab map, so the next `session_tmux` asks again.
    pub(crate) fn forget_tmux_tabs(&self, host_id: Id) {
        lock(&self.0.tmux).remove(&host_id);
    }

    /// Closes a session whoever owns it (internal: reaping, close-all).
    pub(crate) async fn close_session_unchecked(&self, id: &str) {
        self.0.sessions.close(id).await;
    }

    /// Closes every session opened by webview `owner` (the UI calls this on
    /// load, so a reload does not leave its old sessions running unseen).
    /// Returns how many were closed.
    pub async fn close_all_sessions(&self, owner: &str) -> usize {
        let ids = self.session_ids(owner);
        for id in &ids {
            self.close_session_unchecked(id).await;
        }
        ids.len()
    }

    // -- shared connections --------------------------------------------------

    fn conn_slot(&self, host_id: Id) -> ConnSlot {
        lock(&self.0.connections)
            .entry(host_id)
            .or_default()
            .clone()
    }

    /// The host's shared connection, connecting if there is none (or it
    /// closed). Returns the slot guard too: keep it until the channel being
    /// opened is registered, then drop it.
    pub(crate) async fn host_connection(
        &self,
        host_id: Id,
        progress: Arc<dyn ConnectProgress>,
    ) -> Result<(OwnedMutexGuard<Option<SshConnection>>, SshConnection), CmdError> {
        let mut guard = self.conn_slot(host_id).lock_owned().await;
        if let Some(conn) = guard.as_ref().filter(|c| !c.is_closed()).cloned() {
            return Ok((guard, conn));
        }
        *guard = None;
        let conn = self.connect_host(host_id, progress).await?;
        *guard = Some(conn.clone());
        self.forget_when_closed(host_id, &conn);
        Ok((guard, conn))
    }

    /// The host's stored connection, if it is live and nobody is connecting
    /// or opening a channel on it right now (never waits for that).
    fn live_connection(&self, host_id: Id) -> Result<SshConnection, CmdError> {
        let slot = lock(&self.0.connections)
            .get(&host_id)
            .cloned()
            .ok_or(SshError::Closed)?;
        let stored = slot
            .try_lock()
            .map_err(|_| CmdError::new("busy", "the host's connection is busy"))?;
        Ok(stored
            .as_ref()
            .filter(|c| !c.is_closed())
            .cloned()
            .ok_or(SshError::Closed)?)
    }

    /// Drops a stored connection that turned out to be dead and connects
    /// again (one retry for a channel open that found it closed).
    pub(crate) async fn reconnect(
        &self,
        host_id: Id,
        guard: &mut OwnedMutexGuard<Option<SshConnection>>,
        progress: Arc<dyn ConnectProgress>,
    ) -> Result<SshConnection, CmdError> {
        **guard = None;
        let conn = self.connect_host(host_id, progress).await?;
        **guard = Some(conn.clone());
        self.forget_when_closed(host_id, &conn);
        Ok(conn)
    }

    /// Connects to vault host `host_id`. A password typed at a prompt with
    /// "save" checked is stored on the host's identity once the login
    /// succeeded (see `SavingPrompter`); failing to store it is logged and
    /// does not fail the connection. `progress` hears each connect step.
    async fn connect_host(
        &self,
        host_id: Id,
        progress: Arc<dyn ConnectProgress>,
    ) -> Result<SshConnection, CmdError> {
        let params = self.connect_params(host_id, 80, 24).await?;
        let verifier = self.host_key_verifier()?;
        let username = params.username.clone();
        let offer = {
            self.with_vault(move |v| Ok(v.password_save_target(host_id)?))
                .await
                .unwrap_or_else(|e| {
                    tracing::debug!(host = %host_id, error = %e.message, "no password save offer");
                    None
                })
        };
        let prompter = SavingPrompter::new(self.0.prompts.clone(), offer);
        tracing::info!(host = %host_id, "connecting");
        let connected = self
            .0
            .transport
            .connect_with_progress(&params, verifier, prompter.clone(), progress)
            .await;
        // Accepting an unknown host key stores it in the vault.
        self.0.sync.local_change();
        let conn = connected?;
        if let Some(password) = prompter.take_saved() {
            let saved = self
                .with_vault_write(move |v| Ok(v.save_host_password(host_id, &username, password)?))
                .await;
            match saved {
                Ok(_) => tracing::info!(host = %host_id, "saved the typed password to the vault"),
                Err(e) => {
                    tracing::warn!(host = %host_id, error = %e.message, "could not save the typed password")
                }
            }
        }
        Ok(conn)
    }

    /// Clears the host's slot once its connection ends, so the next use
    /// reconnects instead of failing on a dead connection.
    fn forget_when_closed(&self, host_id: Id, conn: &SshConnection) {
        let signal = conn.close_signal();
        let slot = Arc::downgrade(&self.conn_slot(host_id));
        tauri::async_runtime::spawn(async move {
            signal.wait().await;
            if let Some(slot) = slot.upgrade() {
                let mut stored = slot.lock().await;
                if stored.as_ref().is_some_and(SshConnection::is_closed) {
                    *stored = None;
                    tracing::info!(host = %host_id, "connection closed");
                }
            }
        });
    }

    /// Whether any session, SFTP client or running forward still uses the
    /// host's connection.
    fn host_in_use(&self, host_id: Id) -> bool {
        let sessions = lock(&self.0.registry)
            .values()
            .any(|i| i.meta.host_id == Some(host_id));
        let sftp = lock(&self.0.sftp).contains_key(&host_id);
        let forwards = lock(&self.0.forward_hosts)
            .iter()
            .filter(|(_, host)| **host == host_id)
            .any(|(rule, _)| {
                self.0
                    .forwards
                    .status(*rule)
                    .is_some_and(|s| s.is_running())
            });
        sessions || sftp || forwards
    }

    /// Lets go of the host's connection if nothing uses it any more (it
    /// disconnects once the last channel is gone). Callers that opened (or
    /// failed to open) a channel call this after dropping the slot guard, so
    /// a connection whose first channel failed, or whose session ended
    /// before it was registered, is not kept idle forever.
    pub(crate) async fn release_if_idle(&self, host_id: Id) {
        let Some(slot) = lock(&self.0.connections).get(&host_id).cloned() else {
            return;
        };
        let mut stored = slot.lock().await;
        if stored.is_some() && !self.host_in_use(host_id) {
            *stored = None;
            self.forget_tmux_tabs(host_id);
            tracing::info!(host = %host_id, "releasing idle connection");
        }
    }

    async fn after_forward_event(&self, event: &ForwardEvent) {
        if event.status.is_running() {
            return;
        }
        let host = lock(&self.0.forward_hosts).get(&event.rule_id).copied();
        if let Some(host) = host {
            self.release_if_idle(host).await;
        }
    }
}

/// The verifier handed to targets that never check a host key (local shells,
/// channels on an already verified connection). Rejects, should it ever be
/// asked.
struct RejectAll;

#[async_trait::async_trait]
impl HostKeyVerifier for RejectAll {
    async fn verify(&self, _host: &str, _port: u16, _key: &HostKeyInfo) -> HostKeyDecision {
        HostKeyDecision::Reject
    }
}
