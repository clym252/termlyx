//! The encrypted local vault: account setup and unlock ([`VaultService`]),
//! entity CRUD with referential integrity ([`Entity`]), and keychain-backed
//! quick unlock ([`DeviceKeyStore`]).
//!
//! Every entity is serialized, sealed under the vault key and stored as an
//! envelope by `core-store`; nothing readable reaches the database file. The
//! account record layout is documented in `docs/crypto.md` ("Local account
//! record").
//!
//! Entities carry secrets (identity passwords, private keys) and their
//! serialized form is plaintext: never log or `Debug`-print entities, their
//! JSON, or anything this module decrypts.

mod account_sync;
mod cascade;
mod conflicts;
mod crud;
mod import;
mod keychain;
mod merge;
mod own_login;
mod presence;
mod quick_unlock;
mod saved_password;
mod service;
mod sync_local;

#[cfg(test)]
mod tests;

use std::sync::{Arc, Mutex};

use core_crypto::CryptoError;
use core_model::{ConnSettings, Id};
use core_store::StoreError;

pub use account_sync::{
    NewRecovery, PasswordChanged, RecoveryReset, SYNC_ACCOUNT_REV, kdf_to_wire, recovery_reset_for,
    validate_kdf, validate_material,
};
pub use conflicts::{ConflictSide, ConflictView, Resolution};
pub use crud::{ChildPolicy, Entity};
pub use keychain::{is_device_locked, keychain_entry};
pub use own_login::PasswordChange;
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub use presence::BiometricGate;
#[cfg(any(test, feature = "test-util"))]
pub use presence::FakeGate;
pub use presence::{
    GateKind, KeystoreBackend, KeystoreError, KeystoreGate, NoGate, PresenceGate,
    UnreachableKeystore, default_gate,
};
#[cfg(windows)]
pub use presence::{WindowsHelloGate, set_prompt_owner};
pub use quick_unlock::{DeviceKeyStore, KeyringStore, MemoryKeyStore};
pub use service::VaultService;
pub use sync_local::MergeReport;

/// The vault as shared by async callers (the Tauri command layer).
///
/// The lock is synchronous and must never be held across an `.await`: take
/// it, do the call, drop the guard. Calls that run Argon2 (`create`,
/// `unlock`, `unlock_with_recovery`) take about a second and belong inside
/// `tokio::task::spawn_blocking`.
pub type SharedVault = Arc<Mutex<VaultService>>;

/// Built-in app defaults, the lowest layer of
/// [`effective_settings`](VaultService::effective_settings): port 22,
/// `xterm-256color`, a 30 s keepalive and UTF-8; everything else unset.
pub fn app_defaults() -> ConnSettings {
    ConnSettings {
        port: Some(22),
        identity_id: None,
        keepalive_secs: Some(30),
        term_type: Some("xterm-256color".to_owned()),
        charset: Some("UTF-8".to_owned()),
        color_scheme: None,
        font_family: None,
        font_size: None,
        env: None,
    }
}

/// The words in the quick unlock errors, which the app shows. They name
/// the device's check ([`presence::device_kind`]): one per platform, except
/// on iOS, where it is Face ID or Touch ID by device. `unavailable` prefixes
/// the reason; Windows Hello's reasons are whole sentences that name it
/// already.
mod copy {
    use super::GateKind;

    #[cfg(windows)]
    pub(super) const KEYCHAIN: &str = "Credential Manager";
    #[cfg(not(windows))]
    pub(super) const KEYCHAIN: &str = "keychain";

    pub(super) fn unavailable(kind: GateKind) -> &'static str {
        match kind {
            GateKind::TouchId => "Touch ID is not available: ",
            GateKind::FaceId => "Face ID is not available: ",
            GateKind::WindowsHello | GateKind::AndroidBiometric => "",
            GateKind::None => "quick unlock is not available: ",
        }
    }

    pub(super) fn changed(kind: GateKind) -> &'static str {
        match kind {
            GateKind::TouchId => "Touch ID fingers changed; quick unlock was turned off",
            GateKind::FaceId => "Face ID changed; quick unlock was turned off",
            GateKind::WindowsHello => "Windows Hello changed; quick unlock was turned off",
            GateKind::AndroidBiometric => "Biometrics changed; quick unlock was turned off",
            GateKind::None => "quick unlock was turned off: this device's unlock check changed",
        }
    }

    pub(super) fn denied(kind: GateKind) -> &'static str {
        match kind {
            GateKind::TouchId => "Touch ID was not confirmed",
            GateKind::FaceId => "Face ID was not confirmed",
            GateKind::WindowsHello => "Windows Hello was not confirmed",
            GateKind::AndroidBiometric => "Biometric unlock was not confirmed",
            GateKind::None => "quick unlock was not confirmed",
        }
    }
}

/// Errors returned by the vault. None carries a password, key or decrypted
/// field.
#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    /// The vault is locked (or not created yet); unlock it first.
    #[error("vault is locked")]
    Locked,
    /// No vault has been created in this database yet.
    #[error("vault has not been created")]
    NotInitialized,
    /// `create` was called on a database that already holds a vault.
    #[error("vault already exists")]
    AlreadyInitialized,
    /// The master password is wrong. Nothing was written.
    #[error("wrong master password")]
    WrongPassword,
    /// The recovery key is malformed or does not belong to this vault.
    #[error("wrong recovery key")]
    WrongRecoveryKey,
    /// The record is still referenced by these ids; nothing was deleted.
    #[error("still referenced by {} record(s)", .0.len())]
    Referenced(Vec<Id>),
    /// The value refers to an id that does not exist (or is another kind).
    #[error("referenced record {0} does not exist")]
    MissingRef(Id),
    /// The id is already used by a record of another kind.
    #[error("id {0} already belongs to a record of another kind")]
    IdConflict(Id),
    /// The identity is another host's or group's own login: inherit it
    /// instead of linking it.
    #[error("identity {0} is another record's own login")]
    ForeignOwnLogin(Id),
    /// The request does not make sense for this record (e.g. picking fields
    /// from a deleted version, revealing a field that is not a secret).
    #[error("invalid request: {0}")]
    Invalid(&'static str),
    /// The change would make a group its own ancestor.
    #[error("group hierarchy would contain a cycle")]
    Cycle,
    /// Quick unlock has not been enabled for this vault on this device.
    #[error("quick unlock is not enabled")]
    QuickUnlockNotEnabled,
    /// The OS keychain (Credential Manager on Windows) is unavailable,
    /// failed, or lost the device key.
    #[error("{}: {}", copy::KEYCHAIN, .0)]
    Keychain(String),
    /// The presence check (Touch ID, Face ID, Windows Hello) cannot run
    /// now: no sensor, nothing enrolled, lid closed or locked out. Use the
    /// master password.
    #[error("{}{}", copy::unavailable(presence::device_kind()), .0)]
    BiometryUnavailable(String),
    /// The enrolled fingers or faces changed (Touch ID, Face ID), or the
    /// Windows Hello credential was reset, since quick unlock was enabled; it
    /// has been turned off. Unlock with the master password and enable it
    /// again.
    #[error("{}", copy::changed(presence::device_kind()))]
    BiometryChanged,
    /// The user cancelled or failed the presence check, or chose the master
    /// password.
    #[error("{}", copy::denied(presence::device_kind()))]
    PresenceDenied,
    /// A record decrypted but does not decode as the requested entity.
    #[error("record {0} does not decode")]
    BadRecord(Id),
    /// The account data in the database is missing or malformed.
    #[error("vault account data is malformed: {0}")]
    BadAccount(&'static str),
    /// A sync server asked for cheaper Argon2 parameters than this device
    /// accepts ([`VaultService::kdf_floor`]): nothing was derived with them
    /// and nothing was sent or written.
    #[error("the server asked for weaker password protection than this app allows")]
    WeakKdf,
    /// Account material from a sync server is malformed, out of bounds, or
    /// belongs to another vault. Nothing was written.
    #[error("account data from the server is not valid: {0}")]
    BadMaterial(&'static str),
    /// Storage failure, including `Corrupt { id }` for a record that fails to
    /// decrypt and `NotFound { id }`.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// Cryptographic failure other than a wrong password or recovery key.
    #[error(transparent)]
    Crypto(#[from] CryptoError),
}
