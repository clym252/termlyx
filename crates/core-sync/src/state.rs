//! What sync remembers between runs, kept in the vault's `sync_state` table
//! (plain key/value bookkeeping, no secrets except the documented credential
//! fallback in [`crate::fallback`]), and the status the engine reports.
//!
//! Numbers are stored as decimal text, ids and URLs as UTF-8, the pin as its
//! 32 bytes.

use std::fmt;

use core_api::vault::{SYNC_ACCOUNT_REV, SharedVault, VaultService};
use core_model::{Id, KIND_SET_VERSION};

use crate::error::SyncError;
use crate::tls::Pin;

/// `sync_state` keys.
pub(crate) mod keys {
    pub const ENABLED: &str = "enabled";
    pub const URL: &str = "url";
    pub const PIN: &str = "pin";
    pub const USERNAME: &str = "username";
    pub const DEVICE_ID: &str = "device_id";
    /// Last pulled `seq`.
    pub const CURSOR: &str = "cursor";
    /// The server database identity the cursor belongs to.
    pub const EPOCH: &str = "epoch";
    /// Written by `core-api` with the material it describes.
    pub const ACCOUNT_REV: &str = super::SYNC_ACCOUNT_REV;
    /// `KIND_SET_VERSION` of the build that last completed a full pull.
    pub const KINDS_VERSION: &str = "kinds_version";
    /// Set while a full resync is under way (survives a restart).
    pub const RESYNC: &str = "resync";
    /// The epoch seen on the resync's first page.
    pub const RESYNC_EPOCH: &str = "resync_epoch";
    /// Unix ms of the last completed sync.
    pub const LAST_SYNC: &str = "last_sync";
    /// Which credential store holds the tokens: `keychain` or `file`.
    pub const TOKEN_STORE: &str = "token_store";
}

/// Where this device syncs to and as whom.
#[derive(Clone, PartialEq, Eq)]
pub struct SyncConfig {
    /// Server base URL.
    pub url: String,
    /// SPKI pin of the server certificate (self-signed servers).
    pub pin: Option<Pin>,
    /// Account username.
    pub username: String,
    /// This device's id on the server.
    pub device_id: Id,
}

impl fmt::Debug for SyncConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SyncConfig")
            .field("url", &self.url)
            .field("pin", &self.pin.is_some())
            .field("username", &self.username)
            .field("device_id", &self.device_id)
            .finish()
    }
}

fn bad(what: &str) -> SyncError {
    SyncError::Vault(format!("sync settings are damaged ({what})"))
}

pub(crate) fn get_text(v: &VaultService, k: &str) -> Result<Option<String>, SyncError> {
    v.sync_get(k)?
        .map(|b| String::from_utf8(b).map_err(|_| bad(k)))
        .transpose()
}

pub(crate) fn get_i64(v: &VaultService, k: &str) -> Result<Option<i64>, SyncError> {
    get_text(v, k)?
        .map(|t| t.parse().map_err(|_| bad(k)))
        .transpose()
}

pub(crate) fn put_i64(v: &mut VaultService, k: &str, n: i64) -> Result<(), SyncError> {
    Ok(v.sync_put(k, n.to_string().as_bytes())?)
}

impl SyncConfig {
    /// The saved configuration, or `None` when sync is off on this device.
    pub fn load(v: &VaultService) -> Result<Option<SyncConfig>, SyncError> {
        if !is_enabled(v)? {
            return Ok(None);
        }
        let need = |k: &str| get_text(v, k)?.ok_or_else(|| bad(k));
        let pin = match v.sync_get(keys::PIN)? {
            None => None,
            Some(b) => Some(Pin::new(b.try_into().map_err(|_| bad(keys::PIN))?)),
        };
        Ok(Some(SyncConfig {
            url: need(keys::URL)?,
            pin,
            username: need(keys::USERNAME)?,
            device_id: need(keys::DEVICE_ID)?
                .parse()
                .map_err(|_| bad(keys::DEVICE_ID))?,
        }))
    }

    /// Saves the configuration (it does not turn sync on by itself).
    pub fn save(&self, v: &mut VaultService) -> Result<(), SyncError> {
        let device = self.device_id.to_string();
        v.sync_put_all(&[
            (keys::URL, self.url.as_bytes()),
            (keys::USERNAME, self.username.as_bytes()),
            (keys::DEVICE_ID, device.as_bytes()),
        ])?;
        match self.pin {
            Some(pin) => v.sync_put(keys::PIN, pin.as_bytes())?,
            None => v.sync_delete(keys::PIN)?,
        }
        Ok(())
    }
}

/// Whether sync is on for this vault.
pub fn is_enabled(v: &VaultService) -> Result<bool, SyncError> {
    Ok(v.sync_get(keys::ENABLED)?.as_deref() == Some(b"1"))
}

pub(crate) fn set_enabled(v: &mut VaultService) -> Result<(), SyncError> {
    Ok(v.sync_put(keys::ENABLED, b"1")?)
}

/// Fresh bookkeeping for a vault that just started syncing with a server:
/// cursor 0, the server's epoch, `accountRev`, and this build's kind set.
pub(crate) fn init(v: &mut VaultService, epoch: &str, account_rev: i64) -> Result<(), SyncError> {
    let kinds = KIND_SET_VERSION.to_string();
    let rev = account_rev.to_string();
    v.sync_put_all(&[
        (keys::CURSOR, b"0"),
        (keys::EPOCH, epoch.as_bytes()),
        (keys::ACCOUNT_REV, rev.as_bytes()),
        (keys::KINDS_VERSION, kinds.as_bytes()),
    ])?;
    v.sync_delete(keys::RESYNC)?;
    v.sync_delete(keys::RESYNC_EPOCH)?;
    Ok(())
}

/// The `sync_state` of a vault that just turned sync on with a new account:
/// the configuration, cursor 0, the server's epoch, `accountRev`, this
/// build's kind set, and the enabled flag (written in one transaction with
/// the rest of the enable, see `VaultService::commit_enable`).
pub(crate) fn enable_state(
    cfg: &SyncConfig,
    epoch: &str,
    account_rev: i64,
) -> Vec<(&'static str, Vec<u8>)> {
    let mut kv = vec![
        (keys::URL, cfg.url.as_bytes().to_vec()),
        (keys::USERNAME, cfg.username.as_bytes().to_vec()),
        (keys::DEVICE_ID, cfg.device_id.to_string().into_bytes()),
        (keys::CURSOR, b"0".to_vec()),
        (keys::EPOCH, epoch.as_bytes().to_vec()),
        (keys::ACCOUNT_REV, account_rev.to_string().into_bytes()),
        (
            keys::KINDS_VERSION,
            KIND_SET_VERSION.to_string().into_bytes(),
        ),
        (keys::ENABLED, b"1".to_vec()),
    ];
    if let Some(pin) = cfg.pin {
        kv.push((keys::PIN, pin.as_bytes().to_vec()));
    }
    kv
}

/// The `sync_state` of a vault that reconnects to the account it already
/// belongs to: the configuration, `accountRev`, cursor 0 and a pending full
/// resync (the epoch is learned from the first page, as in any resync).
pub(crate) fn rejoin_state(cfg: &SyncConfig, account_rev: i64) -> Vec<(&'static str, Vec<u8>)> {
    let mut kv = vec![
        (keys::URL, cfg.url.as_bytes().to_vec()),
        (keys::USERNAME, cfg.username.as_bytes().to_vec()),
        (keys::DEVICE_ID, cfg.device_id.to_string().into_bytes()),
        (keys::CURSOR, b"0".to_vec()),
        (keys::ACCOUNT_REV, account_rev.to_string().into_bytes()),
        (keys::RESYNC, b"1".to_vec()),
    ];
    if let Some(pin) = cfg.pin {
        kv.push((keys::PIN, pin.as_bytes().to_vec()));
    }
    kv
}

/// Runs `f` on the vault on the blocking pool (vault calls are synchronous
/// SQLite, and some run Argon2). The lock is held only inside `f`.
pub(crate) async fn with_vault<T, F>(vault: &SharedVault, f: F) -> Result<T, SyncError>
where
    T: Send + 'static,
    F: FnOnce(&mut VaultService) -> Result<T, SyncError> + Send + 'static,
{
    let vault = vault.clone();
    tokio::task::spawn_blocking(move || {
        // Recovering a poisoned lock is safe: writes that must land
        // together are single store transactions (`begin_resync`,
        // `finish_resync`, `apply_inbound` batches, account installs), and
        // the other multi-call sequences are safe to replay (a re-pulled
        // page is ignored, a re-pushed row is acknowledged).
        let mut v = vault.lock().unwrap_or_else(|p| p.into_inner());
        f(&mut v)
    })
    .await
    .map_err(|_| SyncError::Vault("the vault task failed".into()))?
}

/// What kind of problem stops sync (for the UI to pick an icon or action).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncErrorKind {
    /// The server's certificate no longer matches the pin.
    CertificateChanged,
    /// This device was revoked; sync stopped for good.
    Revoked,
    /// The server needs a newer Tether.
    UpdateClient,
    /// The server is older than this app.
    UpdateServer,
    /// This device's clock is too far off.
    ClockSkew,
    /// The session is gone; the device must sign in again.
    SignInAgain,
    /// Too many requests for now.
    RateLimited,
    /// The credential store failed.
    Storage,
    /// The local vault failed.
    Vault,
    /// Anything else the server or the connection reported.
    Server,
    /// The sync finished, but some records were skipped (unreadable rows
    /// from the server, local records the server refuses); the rest synced.
    Partial,
}

/// The engine's state as the UI shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncStatus {
    /// Sync is not enabled on this device.
    Off,
    /// Up to date as of `last_sync` (Unix ms; 0 before the first sync).
    Idle { last_sync: i64 },
    /// Working: `done` rows of `total` (when known).
    Syncing { done: usize, total: Option<usize> },
    /// The server cannot be reached; retrying with backoff.
    Offline { reason: String },
    /// Sync stopped on an error (plain `message`).
    Error {
        kind: SyncErrorKind,
        message: String,
    },
}

impl SyncStatus {
    /// The status shown for `e` (not used for `Unreachable`, which is
    /// [`SyncStatus::Offline`]).
    pub(crate) fn error(e: &SyncError) -> SyncStatus {
        let kind = match e {
            SyncError::CertificateChanged => SyncErrorKind::CertificateChanged,
            SyncError::Revoked => SyncErrorKind::Revoked,
            SyncError::UpdateClient => SyncErrorKind::UpdateClient,
            SyncError::UpdateServer => SyncErrorKind::UpdateServer,
            SyncError::ClockSkew => SyncErrorKind::ClockSkew,
            SyncError::Unauthorized => SyncErrorKind::SignInAgain,
            SyncError::RateLimited => SyncErrorKind::RateLimited,
            SyncError::Storage(_) | SyncError::StorageLocked => SyncErrorKind::Storage,
            SyncError::Vault(_) | SyncError::Locked => SyncErrorKind::Vault,
            _ => SyncErrorKind::Server,
        };
        let message = match e {
            SyncError::ClockSkew => "this device's clock is wrong".to_owned(),
            other => other.to_string(),
        };
        SyncStatus::Error { kind, message }
    }
}

/// One-off events for the UI, besides the status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncNotice {
    /// Another device changed the master password (or reset it with the
    /// recovery key); this device locked itself.
    PasswordChangedElsewhere,
    /// Pulled changes, a push conflict or a repair changed records this
    /// device shows (lists should reload). At most one per engine step; none
    /// for a pull that only echoed this device's own pushes.
    RecordsChanged,
}

impl fmt::Display for SyncNotice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SyncNotice::PasswordChangedElsewhere => {
                f.write_str("Master password was changed on another device")
            }
            SyncNotice::RecordsChanged => f.write_str("Records changed by sync"),
        }
    }
}
