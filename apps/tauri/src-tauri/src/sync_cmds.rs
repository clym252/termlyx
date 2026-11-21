//! Sync commands: turning sync on and off, joining a synced vault, the
//! device list, add-device codes, the live status, and the Conflicts view.
//! (Password change and the recovery-key unlock live with the other vault
//! commands in `vault_cmds`; they use the engine held here.)
//!
//! The app holds one [`SyncHandle`] (one `core_sync::Client` per device: a
//! second client on the same refresh token would get the device revoked).
//! It is started when the app starts and again after a vault is created,
//! unlocked, or joined, whenever sync is on; every vault write command
//! nudges it ([`AppState::with_vault_write`]); the app stops it on quit.
//!
//! DTOs are camelCase like the rest. Nothing here returns a password, key or
//! token. The deliberate exceptions, each shown to the user once, are a
//! *new* recovery key ([`EnableOutcome`]), an add-device code
//! ([`LinkCodeDto`], single use, expires), and a revealed conflict field
//! ([`AppState::conflicts_reveal`], one secret at a time, the same kind of
//! deliberate egress as the key export in `vault_cmds`). Their types print
//! `[REDACTED]` for `Debug`.
//!
//! Events: `sync-status` carries a [`SyncStatusDto`] on every change;
//! `vault-locked` carries `{reason: "passwordChanged"}` when the engine
//! locked the vault after installing a password changed on another device;
//! `vault-changed` (no payload) says sync changed records, so open lists
//! should read again.

use std::sync::Arc;

use core_api::vault::{ConflictSide, ConflictView, Resolution, VaultError};
use core_model::Id;
use core_proto::{CodeKind, DeviceView, ErrorCode};
use core_sync::flows::{self, RecoveryChoice};
use core_sync::{
    Pin, SyncConfig, SyncError, SyncErrorKind, SyncHandle, SyncNotice, SyncStatus, parse_join_code,
};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::State;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;

use crate::state::{AppState, CmdError, events};
use crate::vault_cmds::{LockReason, VaultLocked};

// ---------------------------------------------------------------------------
// DTOs

/// Where this device syncs to and as whom. The server's address and the
/// account name are not secrets; the device id is the server's name for this
/// installation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncAccountDto {
    pub server: String,
    pub username: String,
    pub device_id: Id,
}

/// What kind of problem stopped sync.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SyncErrorKindDto {
    CertificateChanged,
    Revoked,
    UpdateClient,
    UpdateServer,
    ClockSkew,
    /// The session is gone and could not be renewed.
    SignInAgain,
    RateLimited,
    Storage,
    Vault,
    Server,
    /// Synced, but some records were skipped.
    Partial,
}

impl From<SyncErrorKind> for SyncErrorKindDto {
    fn from(k: SyncErrorKind) -> Self {
        match k {
            SyncErrorKind::CertificateChanged => Self::CertificateChanged,
            SyncErrorKind::Revoked => Self::Revoked,
            SyncErrorKind::UpdateClient => Self::UpdateClient,
            SyncErrorKind::UpdateServer => Self::UpdateServer,
            SyncErrorKind::ClockSkew => Self::ClockSkew,
            SyncErrorKind::SignInAgain => Self::SignInAgain,
            SyncErrorKind::RateLimited => Self::RateLimited,
            SyncErrorKind::Storage => Self::Storage,
            SyncErrorKind::Vault => Self::Vault,
            SyncErrorKind::Server => Self::Server,
            SyncErrorKind::Partial => Self::Partial,
        }
    }
}

/// The engine's state. `message` is a plain sentence ("server certificate
/// changed", "device revoked", ...), never a URL, token or response body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(
    tag = "state",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum SyncStateDto {
    Off,
    /// Up to date as of `last_sync` (Unix ms; 0 before the first sync).
    Idle {
        last_sync: i64,
    },
    Syncing {
        done: usize,
        total: Option<usize>,
    },
    /// The server cannot be reached; retrying with backoff.
    Offline {
        reason: String,
    },
    Error {
        kind: SyncErrorKindDto,
        message: String,
    },
}

impl From<&SyncStatus> for SyncStateDto {
    fn from(s: &SyncStatus) -> Self {
        match s {
            SyncStatus::Off => Self::Off,
            SyncStatus::Idle { last_sync } => Self::Idle {
                last_sync: *last_sync,
            },
            SyncStatus::Syncing { done, total } => Self::Syncing {
                done: *done,
                total: *total,
            },
            SyncStatus::Offline { reason } => Self::Offline {
                reason: reason.clone(),
            },
            SyncStatus::Error { kind, message } => Self::Error {
                kind: (*kind).into(),
                message: message.clone(),
            },
        }
    }
}

/// `sync_status` and the `sync-status` event: the account (when sync is on)
/// and the engine's state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncStatusDto {
    pub account: Option<SyncAccountDto>,
    pub status: SyncStateDto,
}

/// What a pasted code is: shown before the user confirms it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeInfoDto {
    /// `"invite"` (creates an account), `"link"` (add-device code) or
    /// `"short"` (a setup, invite or add-device short code: it names no
    /// server, so `server` is empty and `fingerprint` `None` until
    /// `sync_check_server` has verified the typed address).
    pub kind: String,
    /// The server's address.
    pub server: String,
    /// The server certificate's fingerprint, character for character what
    /// `termlyx-server fingerprint` prints (`sha256:` and 64 lowercase hex
    /// digits, no spaces), built by the same function
    /// (`core_proto::fingerprint_text`); `None` when the code carries no pin
    /// (plain-HTTP loopback).
    pub fingerprint: Option<String>,
}

/// The recovery key step of `sync_enable`: `{existing: "<key text>"}` or
/// `{new: true}`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RecoveryInput {
    /// The vault's current recovery key, typed by the user.
    Existing(SecretString),
    /// "I lost it": make a new one (always `true`).
    New(bool),
}

impl std::fmt::Debug for RecoveryInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Existing(_) => f.write_str("Existing([REDACTED])"),
            Self::New(n) => write!(f, "New({n})"),
        }
    }
}

/// Result of `sync_enable`. The recovery key text is shown once.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnableOutcome {
    /// The new recovery key, when "I lost it" made one.
    pub new_recovery_key: Option<String>,
}

impl std::fmt::Debug for EnableOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnableOutcome")
            .field(
                "new_recovery_key",
                &self.new_recovery_key.as_ref().map(|_| "[REDACTED]"),
            )
            .finish()
    }
}

/// Why `sync_enable` failed: the usual command error (`code`, `message`,
/// `refs`) plus, when the failure came after the server already registered a
/// new recovery key ("I lost it"), that key: it is the only one that works
/// now, so the UI shows it exactly like a success's. `Debug` redacts it.
#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnableFailure {
    pub code: String,
    pub message: String,
    pub refs: Vec<String>,
    /// The new recovery key the server already holds, if any.
    pub new_recovery_key: Option<String>,
}

impl std::fmt::Debug for EnableFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnableFailure")
            .field("code", &self.code)
            .field("message", &self.message)
            .field(
                "new_recovery_key",
                &self.new_recovery_key.as_ref().map(|_| "[REDACTED]"),
            )
            .finish()
    }
}

impl From<CmdError> for EnableFailure {
    fn from(e: CmdError) -> Self {
        EnableFailure {
            code: e.code,
            message: e.message,
            refs: e.refs,
            new_recovery_key: None,
        }
    }
}

impl From<SyncError> for EnableFailure {
    fn from(e: SyncError) -> Self {
        cmd_error(e).into()
    }
}

impl From<flows::EnableError> for EnableFailure {
    fn from(e: flows::EnableError) -> Self {
        let flows::EnableError {
            error,
            new_recovery_text,
        } = e;
        EnableFailure {
            new_recovery_key: new_recovery_text.map(|k| k.expose_secret().to_owned()),
            ..cmd_error(error).into()
        }
    }
}

/// The plain command error, without the recovery key.
impl From<EnableFailure> for CmdError {
    fn from(e: EnableFailure) -> Self {
        CmdError {
            code: e.code,
            message: e.message,
            refs: e.refs,
        }
    }
}

/// Result of `sync_join`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JoinOutcome {
    /// Where the replaced vault was kept, when one was replaced.
    pub backup: Option<String>,
    /// This device's own vault was reconnected to sync: its records stayed.
    pub reconnected: bool,
}

/// Result of `sync_recover`: the account has a new password, nothing was
/// written here, and the device still needs an add-device code (from
/// another device, or `termlyx-server link USER` on the server) to join.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoverOutcome {
    pub needs_device_code: bool,
    pub server: String,
}

/// An add-device code and when it stops working (Unix ms).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkCodeDto {
    pub code: String,
    /// The server address the code carries (what the new device connects to).
    pub server: String,
    pub expires_at: i64,
}

impl std::fmt::Debug for LinkCodeDto {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LinkCodeDto")
            .field("code", &"[REDACTED]")
            .field("server", &self.server)
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// A device of the account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceDto {
    pub id: String,
    pub name: String,
    pub platform: String,
    /// Unix ms.
    pub created_at: i64,
    /// Unix ms.
    pub last_seen: i64,
    /// This device.
    pub current: bool,
}

impl From<DeviceView> for DeviceDto {
    fn from(d: DeviceView) -> Self {
        Self {
            id: d.id,
            name: d.name,
            platform: d.platform,
            created_at: d.created_at,
            last_seen: d.last_seen,
            current: d.current,
        }
    }
}

/// A conflict: two versions of one record. `current` and `other` are the
/// record in the vault's own JSON field names (snake_case: `group_id`,
/// `key_id`, ...), the names `changedFields` and a `{fields}` resolution
/// use. Secret fields read `{"hasSecret": true}` (or `null` when unset); a
/// side is `null` when it is a deletion (or the record is gone).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConflictDto {
    pub id: Id,
    pub record_id: Id,
    /// The record kind, as `list_entities` names it (`identity`, `host`, ...).
    pub kind: String,
    pub current: Option<Value>,
    pub other: Option<Value>,
    pub changed_fields: Vec<String>,
    /// Unix seconds.
    pub detected_at: i64,
}

impl From<ConflictView> for ConflictDto {
    fn from(c: ConflictView) -> Self {
        Self {
            id: c.id,
            record_id: c.record_id,
            kind: c.kind.as_str().to_owned(),
            current: c.current,
            other: c.other,
            changed_fields: c.changed_fields,
            detected_at: c.detected_at,
        }
    }
}

/// Which version of a conflict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ConflictSideDto {
    Current,
    Other,
}

impl From<ConflictSideDto> for ConflictSide {
    fn from(s: ConflictSideDto) -> Self {
        match s {
            ConflictSideDto::Current => ConflictSide::Current,
            ConflictSideDto::Other => ConflictSide::Other,
        }
    }
}

/// How to settle a conflict: `{keepCurrent: true}`, `{useOther: true}` or
/// `{fields: ["label", ...]}` (take those fields from the other version).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(try_from = "ResolutionWire")]
pub enum ResolutionDto {
    KeepCurrent,
    UseOther,
    Fields(Vec<String>),
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
enum ResolutionWire {
    KeepCurrent(bool),
    UseOther(bool),
    Fields(Vec<String>),
}

impl TryFrom<ResolutionWire> for ResolutionDto {
    type Error = String;

    fn try_from(w: ResolutionWire) -> Result<Self, String> {
        match w {
            ResolutionWire::KeepCurrent(true) => Ok(Self::KeepCurrent),
            ResolutionWire::UseOther(true) => Ok(Self::UseOther),
            ResolutionWire::KeepCurrent(false) | ResolutionWire::UseOther(false) => {
                Err("a resolution flag must be true".into())
            }
            ResolutionWire::Fields(names) => Ok(Self::Fields(names)),
        }
    }
}

impl From<ResolutionDto> for Resolution {
    fn from(r: ResolutionDto) -> Self {
        match r {
            ResolutionDto::KeepCurrent => Resolution::KeepCurrent,
            ResolutionDto::UseOther => Resolution::UseOther,
            ResolutionDto::Fields(names) => Resolution::Fields(names),
        }
    }
}

// ---------------------------------------------------------------------------
// Errors

/// Maps a sync failure to the command error the UI shows. The message is the
/// error's own plain text (it never holds a URL, token, code, password or
/// key); the code lets the UI pick an action.
pub fn cmd_error(e: SyncError) -> CmdError {
    let code = match &e {
        SyncError::CertificateChanged => "certificateChanged",
        SyncError::ServerUnverified => "serverUnverified",
        SyncError::WeakKdf => "weakKdf",
        SyncError::Revoked => "revoked",
        SyncError::UpdateClient => "updateRequired",
        SyncError::UpdateServer => "updateServer",
        SyncError::Unreachable(_) => "unreachable",
        SyncError::Unauthorized => "unauthorized",
        SyncError::ClockSkew => "clockSkew",
        SyncError::RateLimited => "rateLimited",
        SyncError::Storage(_) | SyncError::StorageLocked => "storage",
        SyncError::WrongPassword => "wrongPassword",
        SyncError::WrongRecoveryKey => "wrongRecoveryKey",
        SyncError::InvalidCode(_) => "invalidCode",
        SyncError::Locked => "locked",
        SyncError::VaultExists => "vaultExists",
        SyncError::NeedsDeviceCode => "needsDeviceCode",
        // An address that answers but is not a Termlyx server.
        SyncError::Protocol(_) => "protocol",
        // The local vault's own failure; the flows keep only its plain
        // message (the vault's missing/locked answers are checked up front,
        // see `sync_enable`).
        SyncError::Vault(_) => "storage",
        SyncError::Server { code, .. } => match code {
            ErrorCode::InviteInvalid => "invalidCode",
            ErrorCode::UsernameTaken => "usernameTaken",
            ErrorCode::Conflict => "conflict",
            ErrorCode::NotFound => "notFound",
            ErrorCode::BadRequest | ErrorCode::PayloadTooLarge => "invalid",
            ErrorCode::Forbidden => "forbidden",
            _ => "server",
        },
        // Gone and whatever is added later.
        _ => "server",
    };
    CmdError::new(code, e.to_string())
}

impl From<SyncError> for CmdError {
    fn from(e: SyncError) -> Self {
        cmd_error(e)
    }
}

fn sync_off() -> CmdError {
    CmdError::new("syncOff", "sync is off")
}

fn sync_unavailable() -> CmdError {
    CmdError::new(
        "syncUnavailable",
        "sync is on but could not start; restart Termlyx or turn sync off",
    )
}

// ---------------------------------------------------------------------------
// Code helpers

fn parse_code(text: &str) -> Result<core_sync::JoinCode, CmdError> {
    Ok(parse_join_code(text)?)
}

/// A typed server address as `https://host:port`; a bad one is `invalidAddress`.
fn parse_server_address(text: &str) -> Result<String, CmdError> {
    core_proto::parse_address(text).map_err(|_| {
        CmdError::new(
            "invalidAddress",
            "that server address is not valid; use an address like 192.168.1.20, nas.local or nas.local:8443",
        )
    })
}

fn parse_short_code(text: &str) -> Result<core_proto::ShortCode, CmdError> {
    core_proto::ShortCode::parse(text).map_err(|e| CmdError::from(SyncError::from(e)))
}

/// What `sync_enable` was given: a long invite code, or a short code with the
/// address typed next to it.
enum EnableCode {
    Invite(core_sync::JoinCode),
    Short(core_proto::ShortCode, String),
}

// ---------------------------------------------------------------------------
// The engine slot

/// The running engine, the account it syncs as, and the tasks that turn its
/// status and notices into events.
struct Running {
    handle: SyncHandle,
    account: SyncAccountDto,
    tasks: Vec<JoinHandle<()>>,
}

impl Running {
    /// Stops the event tasks, then the engine (bounded: a request in flight
    /// must not hold up quitting).
    async fn stop(self) {
        let Running { handle, tasks, .. } = self;
        for t in &tasks {
            t.abort();
        }
        if tokio::time::timeout(std::time::Duration::from_secs(5), handle.stop())
            .await
            .is_err()
        {
            tracing::warn!("sync engine did not stop in time; abandoning it");
        }
    }
}

/// The app's sync engine, if sync is on and it started.
#[derive(Default)]
pub(crate) struct SyncSlot {
    running: RwLock<Option<Running>>,
    /// An add-device code already redeemed whose join was refused because
    /// it would replace another vault: kept while the UI shows the replace
    /// warning, so the confirmed retry does not need a new (single-use) code.
    pending_join: tokio::sync::Mutex<Option<flows::PendingJoin>>,
}

impl SyncSlot {
    /// Nudges the engine after a local write. Never waits: if the slot is
    /// being started or stopped the engine syncs anyway at its start.
    pub(crate) fn local_change(&self) {
        if let Ok(slot) = self.running.try_read()
            && let Some(r) = slot.as_ref()
        {
            r.handle.notify_local_change();
        }
    }

    /// Tells the engine the vault was unlocked.
    pub(crate) fn unlocked(&self) {
        if let Ok(slot) = self.running.try_read()
            && let Some(r) = slot.as_ref()
        {
            r.handle.notify_unlocked();
        }
    }

    /// Tells the engine Tether came to the front (so the device is
    /// unlocked): ends a sync pause for a keychain locked with the device.
    pub(crate) fn foreground(&self) {
        if let Ok(slot) = self.running.try_read()
            && let Some(r) = slot.as_ref()
        {
            r.handle.notify_foreground();
        }
    }
}

impl AppState {
    /// Starts the sync engine if sync is on and none is running (a no-op
    /// otherwise, also for an engine that stopped on its own, such as a
    /// revoked device: its final status stays visible). The engine also runs
    /// while the vault is locked, so a locked device still picks up a new
    /// password; merging waits for the unlock.
    pub async fn start_sync(&self) {
        let mut slot = self.0.sync.running.write().await;
        self.start_in(&mut slot).await;
    }

    /// [`AppState::start_sync`] for a caller that already holds the slot, so
    /// a stop/start sequence (turn off, replacing join) is one hold.
    async fn start_in(&self, slot: &mut Option<Running>) {
        let Ok(vault) = self.shared_vault() else {
            return;
        };
        if slot.is_some() {
            return;
        }
        let cfg = match self
            .with_vault(|v| SyncConfig::load(v).map_err(cmd_error))
            .await
        {
            Ok(Some(cfg)) => cfg,
            Ok(None) => return,
            Err(e) => {
                tracing::warn!(error = %e.message, "sync settings could not be read");
                return;
            }
        };
        let Some(handle) = core_sync::start(vault, self.0.tokens.clone()) else {
            return;
        };
        let account = SyncAccountDto {
            server: cfg.url,
            username: cfg.username,
            device_id: cfg.device_id,
        };
        let tasks = vec![
            self.forward_status(handle.status(), account.clone()),
            self.forward_notices(handle.notices()),
        ];
        *slot = Some(Running {
            handle,
            account,
            tasks,
        });
    }

    /// Stops the sync engine (the app calls this on quit). Sync stays on:
    /// the engine starts again with the app. The slot stays held until the
    /// engine is gone, so nothing starts a second one meanwhile.
    pub async fn stop_sync(&self) {
        let mut slot = self.0.sync.running.write().await;
        if let Some(r) = slot.take() {
            r.stop().await;
        }
    }

    /// One `sync-status` event per status change, starting with the current
    /// one. Holds the state weakly, so it never keeps the app alive.
    fn forward_status(
        &self,
        mut status: tokio::sync::watch::Receiver<SyncStatus>,
        account: SyncAccountDto,
    ) -> JoinHandle<()> {
        let weak = Arc::downgrade(&self.0);
        tokio::spawn(async move {
            loop {
                let dto = SyncStatusDto {
                    account: Some(account.clone()),
                    status: SyncStateDto::from(&*status.borrow_and_update()),
                };
                let Some(inner) = weak.upgrade() else { return };
                AppState(inner).emit(events::SYNC_STATUS, &dto);
                if status.changed().await.is_err() {
                    return;
                }
            }
        })
    }

    /// Tells the UI when the engine locked the vault because another device
    /// changed the password, and when sync changed records.
    fn forward_notices(
        &self,
        mut notices: tokio::sync::broadcast::Receiver<SyncNotice>,
    ) -> JoinHandle<()> {
        use tokio::sync::broadcast::error::RecvError;
        let weak = Arc::downgrade(&self.0);
        tokio::spawn(async move {
            loop {
                match notices.recv().await {
                    Ok(SyncNotice::PasswordChangedElsewhere) => {
                        let Some(inner) = weak.upgrade() else { return };
                        AppState(inner).emit(
                            events::VAULT_LOCKED,
                            &VaultLocked {
                                reason: LockReason::PasswordChanged,
                            },
                        );
                    }
                    Ok(SyncNotice::RecordsChanged) => {
                        let Some(inner) = weak.upgrade() else { return };
                        AppState(inner).emit(events::VAULT_CHANGED, &serde_json::json!({}));
                    }
                    // Missed notices may have been this one: read again.
                    Err(RecvError::Lagged(_)) => {
                        let Some(inner) = weak.upgrade() else { return };
                        AppState(inner).emit(events::VAULT_CHANGED, &serde_json::json!({}));
                    }
                    Err(RecvError::Closed) => return,
                }
            }
        })
    }

    /// Whether sync is turned on in the vault's settings (the engine may
    /// still have failed to start).
    async fn sync_enabled(&self) -> bool {
        self.with_vault(|v| Ok(core_sync::is_enabled(v).unwrap_or(false)))
            .await
            .unwrap_or(false)
    }

    // -- commands ------------------------------------------------------------

    /// The account and the engine's state.
    pub async fn sync_status(&self) -> SyncStatusDto {
        {
            let slot = self.0.sync.running.read().await;
            if let Some(r) = slot.as_ref() {
                return SyncStatusDto {
                    account: Some(r.account.clone()),
                    status: SyncStateDto::from(&*r.handle.status().borrow()),
                };
            }
        }
        if self.sync_enabled().await {
            return SyncStatusDto {
                account: None,
                status: SyncStateDto::Error {
                    kind: SyncErrorKindDto::Vault,
                    message: sync_unavailable().message,
                },
            };
        }
        SyncStatusDto {
            account: None,
            status: SyncStateDto::Off,
        }
    }

    /// What a pasted code is and which server it names, to show before the
    /// user confirms.
    pub fn sync_parse_code(&self, code: SecretString) -> Result<CodeInfoDto, CmdError> {
        let text = code.expose_secret();
        let code = match parse_code(text) {
            Ok(code) => code,
            // Not a long code: a short one names no server yet.
            Err(_) if core_proto::ShortCode::parse(text).is_ok() => {
                return Ok(CodeInfoDto {
                    kind: "short".to_owned(),
                    server: String::new(),
                    fingerprint: None,
                });
            }
            Err(e) => return Err(e),
        };
        Ok(CodeInfoDto {
            kind: match code.kind {
                CodeKind::Invite => "invite",
                CodeKind::Link => "link",
            }
            .to_owned(),
            server: code.url,
            fingerprint: code.pin.as_ref().map(core_proto::fingerprint_text),
        })
    }

    /// Checks that the server at the typed `address` knows the short `code`
    /// before anything secret is sent to it (`flows::check_server`); returns
    /// the normalized address and the fingerprint to compare.
    pub async fn sync_check_server(
        &self,
        address: String,
        code: SecretString,
    ) -> Result<CodeInfoDto, CmdError> {
        parse_server_address(&address)?;
        let code = parse_short_code(code.expose_secret())?;
        let check = flows::check_server(&address, &code).await?;
        Ok(CodeInfoDto {
            kind: "invite".to_owned(),
            server: check.url,
            fingerprint: check.pin.as_ref().map(|p| core_proto::fingerprint_text(p.as_bytes())),
        })
    }

    /// Turns sync on for this (unlocked) vault with an invite code, then
    /// starts the engine, which uploads the records with progress. `password`
    /// is the master password; `recovery` is the existing recovery key or "I
    /// lost it" (a new key, returned once: with the result, or with the
    /// error when something failed after the server took it).
    pub async fn sync_enable(
        &self,
        code: SecretString,
        username: String,
        password: SecretString,
        recovery: RecoveryInput,
    ) -> Result<EnableOutcome, EnableFailure> {
        self.sync_enable_at(None, code, username, password, recovery)
            .await
    }

    /// `sync_enable`; with an `address` the code is a short code for the
    /// server there (checked first, `flows::enable_with_short_code`).
    pub async fn sync_enable_at(
        &self,
        address: Option<String>,
        code: SecretString,
        username: String,
        password: SecretString,
        recovery: RecoveryInput,
    ) -> Result<EnableOutcome, EnableFailure> {
        let code = match address {
            Some(address) => {
                let address = parse_server_address(&address)?;
                EnableCode::Short(parse_short_code(code.expose_secret())?, address)
            }
            None => EnableCode::Invite(parse_code(code.expose_secret())?),
        };
        let recovery = match recovery {
            RecoveryInput::Existing(key) => RecoveryChoice::Existing(key),
            RecoveryInput::New(true) => RecoveryChoice::New,
            RecoveryInput::New(false) => {
                return Err(CmdError::invalid("`new` must be true").into());
            }
        };
        // The vault's own answers for a missing or locked vault
        // (`notInitialized`, `locked`), before anything else is checked.
        self.with_vault(|v| {
            if !v.is_initialized() {
                return Err(VaultError::NotInitialized.into());
            }
            if !v.is_unlocked() {
                return Err(VaultError::Locked.into());
            }
            Ok(())
        })
        .await?;
        let vault = self.shared_vault()?;
        // Held until the engine starts: sync is on (in the vault) a moment
        // before the credentials are saved, and nothing may start an engine
        // in between.
        let mut slot = self.0.sync.running.write().await;
        if slot.is_some() || self.sync_enabled().await {
            return Err(CmdError::new("syncOn", "sync is already on").into());
        }
        let tokens = self.0.tokens.clone();
        let enabled = match &code {
            EnableCode::Invite(code) => {
                flows::enable_with_invite(&vault, tokens, code, &username, password, recovery).await
            }
            EnableCode::Short(code, address) => {
                flows::enable_with_short_code(
                    &vault, tokens, address, code, &username, password, recovery,
                )
                .await
            }
        };
        self.start_in(&mut slot).await;
        let result = enabled?;
        Ok(EnableOutcome {
            new_recovery_key: result
                .new_recovery_text
                .map(|k| k.expose_secret().to_owned()),
        })
    }

    /// Joins the synced vault of `username` with an add-device code and
    /// starts the engine, which downloads the records. When this device's
    /// vault is the synced one (sync was turned off, or the device was
    /// revoked) it is reconnected and keeps its records. A different vault is
    /// refused (`vaultExists`) unless `replace_existing`: the old vault is
    /// then kept as a backup file next to it. The code is spent by the first
    /// try, so its sign-in is kept for the confirmed retry with the same code.
    pub async fn sync_join(
        &self,
        code: SecretString,
        username: String,
        password: SecretString,
        replace_existing: bool,
    ) -> Result<JoinOutcome, CmdError> {
        let code = parse_code(code.expose_secret())?;
        let vault = self.shared_vault()?;
        // One hold of the slot from stopping the old engine to starting the
        // new one: nothing can restart the old vault's engine in between.
        let mut slot = self.0.sync.running.write().await;
        let mut kept = self.0.sync.pending_join.lock().await;
        let pending = match kept.take() {
            Some(p) if p.is_for(&code) => p,
            stale => {
                if let Some(stale) = stale {
                    stale.abandon().await;
                }
                flows::redeem_code(&vault, &code, &username, &password).await?
            }
        };
        if let Some(old) = slot.take() {
            // The engine belongs to the vault being joined or replaced.
            old.stop().await;
        }
        let joined = flows::finish_join(
            &vault,
            self.0.tokens.clone(),
            &pending,
            password,
            replace_existing,
        )
        .await;
        match &joined {
            // Nothing changed: wait for the user's answer to the warning.
            Err(SyncError::VaultExists) => *kept = Some(pending),
            Err(_) => pending.abandon().await,
            Ok(_) => {}
        }
        drop(kept);
        // Also after a failed join: the old vault's engine comes back.
        self.start_in(&mut slot).await;
        let joined = joined?;
        Ok(JoinOutcome {
            backup: joined.backup.map(|p| p.display().to_string()),
            reconnected: joined.reconnected,
        })
    }

    /// "Forgot password" on a device without the vault: the recovery key sets
    /// a new password for the account. `code` is any code from that server
    /// (it names the server and is not used up). The device cannot sign in
    /// yet: the result says an add-device code is needed next, for
    /// `sync_join` with the new password.
    pub async fn sync_recover(
        &self,
        code: SecretString,
        username: String,
        recovery_key: SecretString,
        new_password: SecretString,
    ) -> Result<RecoverOutcome, CmdError> {
        let code = parse_code(code.expose_secret())?;
        // No vault is needed here (the database may have failed to open):
        // then the floor is the production default cost.
        let vault = self.shared_vault().ok();
        flows::recover_on_new_device(
            vault.as_ref(),
            &code.url,
            code.pin.map(Pin::new),
            &username,
            recovery_key.expose_secret(),
            new_password,
        )
        .await?;
        Ok(RecoverOutcome {
            needs_device_code: true,
            server: code.url,
        })
    }

    /// Signs this device in again with the master password after sync
    /// stopped with "sign in again" (its session is gone, e.g. the server was
    /// restored from an older backup). The engine then syncs at once. The
    /// password is used for this call only.
    pub async fn sync_sign_in(&self, password: SecretString) -> Result<(), CmdError> {
        let vault = self.shared_vault()?;
        let slot = self.0.sync.running.read().await;
        let Some(r) = slot.as_ref() else {
            drop(slot);
            return Err(if self.sync_enabled().await {
                sync_unavailable()
            } else {
                sync_off()
            });
        };
        Ok(flows::sign_in_again(&vault, &r.handle, password).await?)
    }

    /// After a successful unlock: when sync is waiting to sign in again, do
    /// it with the password the user just typed (then drop it). A failure
    /// leaves the "sign in again" status for the user to act on.
    pub(crate) async fn sign_in_if_waiting(&self, password: SecretString) {
        let Ok(vault) = self.shared_vault() else {
            return;
        };
        let slot = self.0.sync.running.read().await;
        let Some(r) = slot.as_ref() else { return };
        let waiting = matches!(
            &*r.handle.status().borrow(),
            SyncStatus::Error {
                kind: SyncErrorKind::SignInAgain,
                ..
            }
        );
        if waiting && let Err(e) = flows::sign_in_again(&vault, &r.handle, password).await {
            tracing::info!(error = %e, "signing in again after the unlock failed");
        }
    }

    /// Asks the engine to sync now (also retries after a certificate or
    /// sign-in error). Returns at once; progress comes as `sync-status`.
    pub async fn sync_now(&self) -> Result<(), CmdError> {
        let slot = self.0.sync.running.read().await;
        slot.as_ref().ok_or_else(sync_off)?.handle.sync_now();
        Ok(())
    }

    /// Turns sync off on this device: stops the engine, ends the session on
    /// the server (best effort), forgets the credentials. Every record
    /// stays. Safe to call when sync is already off.
    pub async fn sync_turn_off(&self) -> Result<(), CmdError> {
        let vault = self.shared_vault()?;
        // Held until the credentials are gone: an unlock racing this must
        // not start a second engine on them.
        let mut slot = self.0.sync.running.write().await;
        let handle = slot.take().map(|r| {
            for t in &r.tasks {
                t.abort();
            }
            r.handle
        });
        flows::turn_off(&vault, handle, self.0.tokens.clone()).await?;
        drop(slot);
        self.emit(
            events::SYNC_STATUS,
            &SyncStatusDto {
                account: None,
                status: SyncStateDto::Off,
            },
        );
        Ok(())
    }

    /// An add-device code for another device.
    pub async fn sync_link_code(&self) -> Result<LinkCodeDto, CmdError> {
        self.sync_link_code_at(None).await
    }

    /// `sync_link_code`; with an `address` the code names that address
    /// (for devices that reach the server by another name).
    pub async fn sync_link_code_at(&self, address: Option<String>) -> Result<LinkCodeDto, CmdError> {
        let address = address.map(|a| parse_server_address(&a)).transpose()?;
        let slot = self.0.sync.running.read().await;
        let r = slot.as_ref().ok_or_else(sync_off)?;
        let (code, server, expires_at) =
            flows::create_link_code(&r.handle, address.as_deref()).await?;
        Ok(LinkCodeDto {
            code,
            server,
            expires_at,
        })
    }

    /// The account's devices.
    pub async fn sync_devices(&self) -> Result<Vec<DeviceDto>, CmdError> {
        let slot = self.0.sync.running.read().await;
        let r = slot.as_ref().ok_or_else(sync_off)?;
        let devices = r.handle.client().devices().await?;
        Ok(devices.into_iter().map(DeviceDto::from).collect())
    }

    pub async fn sync_rename_device(&self, id: Id, name: String) -> Result<(), CmdError> {
        let slot = self.0.sync.running.read().await;
        let r = slot.as_ref().ok_or_else(sync_off)?;
        Ok(r.handle.client().rename_device(id, &name).await?)
    }

    pub async fn sync_revoke_device(&self, id: Id) -> Result<(), CmdError> {
        let slot = self.0.sync.running.read().await;
        let r = slot.as_ref().ok_or_else(sync_off)?;
        Ok(r.handle.client().revoke_device(id).await?)
    }

    /// Changes the master password. With sync on this goes to the server
    /// first (offline: `unreachable`) and the other devices lock and ask for
    /// the new one; with sync off it is local only. Keeps this session open.
    pub async fn vault_change_password(
        &self,
        old: SecretString,
        new: SecretString,
    ) -> Result<(), CmdError> {
        let vault = self.shared_vault()?;
        {
            let slot = self.0.sync.running.read().await;
            if let Some(r) = slot.as_ref() {
                return Ok(flows::change_password_synced(&vault, &r.handle, old, new).await?);
            }
        }
        if self.sync_enabled().await {
            // A local-only change would leave the account on the old one.
            return Err(sync_unavailable());
        }
        self.with_vault(move |v| {
            v.change_password(old, new)?;
            Ok(())
        })
        .await
    }

    /// The recovery-key reset with sync on: an account-wide password change.
    /// `None` when sync is off (the caller resets locally). With sync on but
    /// no engine running it is refused (`syncUnavailable`), as a password
    /// change is: a local-only reset would leave the account on the old
    /// password.
    pub(crate) async fn recover_synced(
        &self,
        recovery_key: &SecretString,
        new_password: &SecretString,
    ) -> Option<Result<(), CmdError>> {
        let vault = match self.shared_vault() {
            Ok(v) => v,
            Err(e) => return Some(Err(e)),
        };
        let slot = self.0.sync.running.read().await;
        let Some(r) = slot.as_ref() else {
            drop(slot);
            return self.sync_enabled().await.then(|| Err(sync_unavailable()));
        };
        let new_password = SecretString::from(new_password.expose_secret().to_owned());
        let done = flows::recover_local_synced(
            &vault,
            &r.handle,
            recovery_key.expose_secret(),
            new_password,
        )
        .await;
        Some(
            done.map_err(cmd_error)
                .inspect(|()| r.handle.notify_unlocked()),
        )
    }

    /// After a wrong password with sync on: fetches the current account key
    /// material and retries the unlock once. `None` when no engine is
    /// running.
    pub(crate) async fn unlock_refreshed(
        &self,
        password: SecretString,
    ) -> Option<Result<(), CmdError>> {
        let vault = match self.shared_vault() {
            Ok(v) => v,
            Err(e) => return Some(Err(e)),
        };
        let slot = self.0.sync.running.read().await;
        let r = slot.as_ref()?;
        Some(
            flows::refresh_account_then_retry(&vault, &r.handle, password)
                .await
                .map_err(cmd_error),
        )
    }

    /// Whether an engine is running (the unlock path keeps a spare copy of
    /// the password for the refresh retry only then).
    pub(crate) fn sync_running(&self) -> bool {
        self.0
            .sync
            .running
            .try_read()
            .map(|s| s.is_some())
            .unwrap_or(true)
    }

    // -- conflicts ---------------------------------------------------------

    /// Every open conflict, oldest first. Secret fields are markers.
    pub async fn conflicts_list(&self) -> Result<Vec<ConflictDto>, CmdError> {
        self.with_vault(|v| {
            Ok(v.list_conflicts()?
                .into_iter()
                .map(ConflictDto::from)
                .collect())
        })
        .await
    }

    /// Settles conflict `id` and dismisses it. The result is an ordinary
    /// edit and syncs.
    pub async fn conflicts_resolve(
        &self,
        id: Id,
        resolution: ResolutionDto,
    ) -> Result<(), CmdError> {
        self.with_vault_write(move |v| Ok(v.resolve_conflict(id, resolution.into())?))
            .await
    }

    /// The value of one secret field of one version of a conflict, for the
    /// Conflicts view's reveal button. Together with the key export this is
    /// where a secret leaves the vault on purpose: one field at a time, only
    /// fields the vault lists as secret (others are `invalid`).
    pub async fn conflicts_reveal(
        &self,
        id: Id,
        side: ConflictSideDto,
        field: &str,
    ) -> Result<SecretString, CmdError> {
        let field = field.to_owned();
        self.with_vault(move |v| Ok(v.reveal_conflict_field(id, side.into(), &field)?))
            .await
    }
}

// ---------------------------------------------------------------------------
// Commands

#[tauri::command]
pub async fn sync_status(state: State<'_, AppState>) -> Result<SyncStatusDto, CmdError> {
    Ok(state.sync_status().await)
}

#[tauri::command]
pub async fn sync_parse_code(
    state: State<'_, AppState>,
    code: SecretString,
) -> Result<CodeInfoDto, CmdError> {
    state.sync_parse_code(code)
}

#[tauri::command]
pub async fn sync_check_server(
    state: State<'_, AppState>,
    address: String,
    code: SecretString,
) -> Result<CodeInfoDto, CmdError> {
    state.sync_check_server(address, code).await
}

#[tauri::command]
pub async fn sync_enable(
    state: State<'_, AppState>,
    code: SecretString,
    username: String,
    password: SecretString,
    recovery: RecoveryInput,
    address: Option<String>,
) -> Result<EnableOutcome, EnableFailure> {
    state
        .sync_enable_at(address, code, username, password, recovery)
        .await
}

#[tauri::command]
pub async fn sync_join(
    state: State<'_, AppState>,
    code: SecretString,
    username: String,
    password: SecretString,
    replace_existing: bool,
) -> Result<JoinOutcome, CmdError> {
    state
        .sync_join(code, username, password, replace_existing)
        .await
}

#[tauri::command]
pub async fn sync_recover(
    state: State<'_, AppState>,
    code: SecretString,
    username: String,
    recovery_key: SecretString,
    new_password: SecretString,
) -> Result<RecoverOutcome, CmdError> {
    state
        .sync_recover(code, username, recovery_key, new_password)
        .await
}

/// What an iOS device calls itself in the sync device list when its host name
/// says nothing: "iPhone" or "iPad" (the webview knows which). Anything else
/// is refused.
fn device_fallback_name(kind: &str) -> Result<&'static str, CmdError> {
    match kind {
        "iPhone" => Ok("iPhone"),
        "iPad" => Ok("iPad"),
        _ => Err(CmdError::new(
            "invalid",
            "the device is an iPhone or an iPad",
        )),
    }
}

/// Sent once at start-up on iOS; the device list name follows it (B6).
#[tauri::command]
pub fn set_device_fallback(kind: String) -> Result<(), CmdError> {
    flows::set_device_fallback(device_fallback_name(&kind)?);
    Ok(())
}

#[tauri::command]
pub async fn sync_now(state: State<'_, AppState>) -> Result<(), CmdError> {
    state.sync_now().await
}

/// Stops the engine before an update's installer ends the process (on
/// Windows it exits without `RunEvent::Exit`), or starts it again when that
/// install failed.
#[tauri::command]
pub async fn sync_pause(state: State<'_, AppState>, paused: bool) -> Result<(), CmdError> {
    if paused {
        state.stop_sync().await;
    } else {
        state.start_sync().await;
    }
    Ok(())
}

#[tauri::command]
pub async fn sync_sign_in(
    state: State<'_, AppState>,
    password: SecretString,
) -> Result<(), CmdError> {
    state.sync_sign_in(password).await
}

#[tauri::command]
pub async fn sync_turn_off(state: State<'_, AppState>) -> Result<(), CmdError> {
    state.sync_turn_off().await
}

#[tauri::command]
pub async fn sync_link_code(
    state: State<'_, AppState>,
    address: Option<String>,
) -> Result<LinkCodeDto, CmdError> {
    state.sync_link_code_at(address).await
}

#[tauri::command]
pub async fn sync_devices(state: State<'_, AppState>) -> Result<Vec<DeviceDto>, CmdError> {
    state.sync_devices().await
}

#[tauri::command]
pub async fn sync_rename_device(
    state: State<'_, AppState>,
    id: Id,
    name: String,
) -> Result<(), CmdError> {
    state.sync_rename_device(id, name).await
}

#[tauri::command]
pub async fn sync_revoke_device(state: State<'_, AppState>, id: Id) -> Result<(), CmdError> {
    state.sync_revoke_device(id).await
}

#[tauri::command]
pub async fn vault_change_password(
    state: State<'_, AppState>,
    old: SecretString,
    new: SecretString,
) -> Result<(), CmdError> {
    state.vault_change_password(old, new).await
}

#[tauri::command]
pub async fn conflicts_list(state: State<'_, AppState>) -> Result<Vec<ConflictDto>, CmdError> {
    state.conflicts_list().await
}

#[tauri::command]
pub async fn conflicts_resolve(
    state: State<'_, AppState>,
    id: Id,
    resolution: ResolutionDto,
) -> Result<(), CmdError> {
    state.conflicts_resolve(id, resolution).await
}

/// Returns the secret as plain text: see [`AppState::conflicts_reveal`].
#[tauri::command]
pub async fn conflicts_reveal(
    state: State<'_, AppState>,
    id: Id,
    side: ConflictSideDto,
    field: String,
) -> Result<String, CmdError> {
    let secret = state.conflicts_reveal(id, side, &field).await?;
    Ok(secret.expose_secret().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_device_fallback_is_an_iphone_or_an_ipad_only() {
        assert_eq!(device_fallback_name("iPhone").unwrap(), "iPhone");
        assert_eq!(device_fallback_name("iPad").unwrap(), "iPad");
        for other in ["", "Mac", "iphone", "iPhone\n"] {
            assert_eq!(
                device_fallback_name(other).unwrap_err().code,
                "invalid",
                "{other:?}"
            );
        }
    }

    #[test]
    fn fingerprint_is_the_servers_text() {
        let mut pin = [0u8; 32];
        pin[0] = 0xab;
        pin[31] = 0xff;
        let text = core_proto::fingerprint_text(&pin);
        assert_eq!(text, format!("sha256:ab{}ff", "00".repeat(30)));
        assert!(!text.contains(' ') && text == text.to_lowercase());
    }

    #[test]
    fn resolution_wire_shapes() {
        let de = |v: serde_json::Value| serde_json::from_value::<ResolutionDto>(v);
        assert_eq!(
            de(serde_json::json!({"keepCurrent": true})).unwrap(),
            ResolutionDto::KeepCurrent
        );
        assert_eq!(
            de(serde_json::json!({"useOther": true})).unwrap(),
            ResolutionDto::UseOther
        );
        assert_eq!(
            de(serde_json::json!({"fields": ["a", "b"]})).unwrap(),
            ResolutionDto::Fields(vec!["a".into(), "b".into()])
        );
        assert!(de(serde_json::json!({"keepCurrent": false})).is_err());
        assert!(de(serde_json::json!({"nope": true})).is_err());
        assert!(de(serde_json::json!({"keepCurrent": true, "useOther": true})).is_err());
    }

    #[test]
    fn recovery_input_wire_shapes_and_debug() {
        let de = |v: serde_json::Value| serde_json::from_value::<RecoveryInput>(v);
        let existing = de(serde_json::json!({"existing": "hunter2-key"})).unwrap();
        assert!(!format!("{existing:?}").contains("hunter2"));
        assert!(matches!(
            de(serde_json::json!({"new": true})).unwrap(),
            RecoveryInput::New(true)
        ));
        assert!(de(serde_json::json!({"existing": "k", "new": true})).is_err());
        assert!(de(serde_json::json!("new")).is_err());
    }

    #[test]
    fn secret_bearing_dtos_do_not_debug_print_them() {
        let enable = EnableOutcome {
            new_recovery_key: Some("RECOVERY-TEXT".into()),
        };
        let link = LinkCodeDto {
            code: "tether1-CODE".into(),
            server: "https://s".into(),
            expires_at: 1,
        };
        let failed = EnableFailure {
            code: "storage".into(),
            message: "m".into(),
            refs: vec![],
            new_recovery_key: Some("RECOVERY-TEXT".into()),
        };
        let shown = format!("{enable:?} {link:?} {failed:?}");
        assert!(!shown.contains("RECOVERY-TEXT") && !shown.contains("tether1-CODE"));
        // The plain command error drops the key.
        let plain = CmdError::from(failed);
        assert!(!format!("{plain:?}").contains("RECOVERY-TEXT"));
    }
}
