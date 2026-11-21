//! Quick unlock: a random 32-byte device key kept in the OS keychain wraps
//! the account secret; the wrapped blob lives in the store's account table
//! (`account_secret.device`).
//!
//! Wrapping the account secret (rather than the vault key) mirrors the
//! password and recovery paths: every unlock route yields the account secret
//! and opens the HPKE-sealed vault key the same way, and a password reset
//! does not invalidate quick unlock. Anyone who can read both this device's
//! keychain entry and the vault file can unlock; that is the trade-off the
//! user opts into. On macOS Tether itself only reads the entry after Touch
//! ID ([`PresenceGate`](super::PresenceGate)).
//!
//! Behind a key-binding gate (Windows Hello) there is no keychain entry: the
//! gate's prompt derives the wrapping key from a random challenge stored in
//! `quick_unlock.hello_challenge`, and a reset credential derives a different
//! key, so the unwrap fails and quick unlock turns itself off.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use core_crypto::{CryptoError, random_key, unwrap_secret, wrap_secret};
use core_model::Id;
use secrecy::{ExposeSecret, SecretBox};
use zeroize::Zeroizing;

use super::VaultError;
use super::keychain::keychain_entry;
use super::service::{VaultService, keys, open_account, wrap_label};

/// What the system prompt says when quick unlock is turned on.
const ENABLE_REASON: &str = "turn on quick unlock for your Termlyx vault";
/// What the system prompt says on a quick unlock.
const UNLOCK_REASON: &str = "unlock your Termlyx vault";

/// Per-vault storage for the 32-byte device key. Errors are human-readable
/// descriptions without key material.
pub trait DeviceKeyStore: Send {
    /// The device key for `vault_id`, or `None` if none is stored.
    fn get(&self, vault_id: Id) -> Result<Option<SecretBox<[u8; 32]>>, String>;
    /// Stores (or replaces) the device key for `vault_id`.
    fn set(&self, vault_id: Id, key: &[u8; 32]) -> Result<(), String>;
    /// Removes the device key for `vault_id`; a missing entry is not an error.
    fn delete(&self, vault_id: Id) -> Result<(), String>;
}

/// [`DeviceKeyStore`] backed by the OS credential store via `keyring`
/// (macOS Keychain, Windows Credential Manager, Secret Service, iOS
/// data-protection keychain: see [`keychain_entry`]). Entries use
/// service [`KeyringStore::SERVICE`] and the vault id as the account name.
#[derive(Debug, Default, Clone, Copy)]
pub struct KeyringStore;

impl KeyringStore {
    /// Keychain service name for Tether device keys.
    pub const SERVICE: &'static str = "app.tether.vault-device-key";

    /// A store using the platform keychain. Nothing is accessed until used.
    pub fn new() -> Self {
        Self
    }

    fn entry(vault_id: Id) -> Result<keyring_core::Entry, String> {
        keychain_entry(Self::SERVICE, &vault_id.to_string()).map_err(|e| e.to_string())
    }
}

impl DeviceKeyStore for KeyringStore {
    fn get(&self, vault_id: Id) -> Result<Option<SecretBox<[u8; 32]>>, String> {
        match Self::entry(vault_id)?.get_secret() {
            Ok(bytes) => {
                let bytes = Zeroizing::new(bytes);
                let key: [u8; 32] = bytes
                    .as_slice()
                    .try_into()
                    .map_err(|_| "stored device key has the wrong length".to_owned())?;
                Ok(Some(SecretBox::new(Box::new(key))))
            }
            Err(keyring_core::Error::NoEntry) => Ok(None),
            // keyring's Display never includes the secret bytes.
            Err(e) => Err(e.to_string()),
        }
    }

    fn set(&self, vault_id: Id, key: &[u8; 32]) -> Result<(), String> {
        Self::entry(vault_id)?
            .set_secret(key)
            .map_err(|e| e.to_string())
    }

    fn delete(&self, vault_id: Id) -> Result<(), String> {
        match Self::entry(vault_id)?.delete_credential() {
            Ok(()) | Err(keyring_core::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }
}

/// In-memory [`DeviceKeyStore`] for tests and for platforms without a
/// keychain. Clones share the same entries.
#[derive(Clone, Default)]
pub struct MemoryKeyStore {
    entries: Arc<Mutex<HashMap<Id, Zeroizing<[u8; 32]>>>>,
}

impl MemoryKeyStore {
    /// Number of stored device keys.
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    /// Whether no device key is stored.
    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    /// Forgets every device key (simulates a wiped keychain).
    pub fn clear(&self) {
        self.lock().clear();
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<Id, Zeroizing<[u8; 32]>>> {
        // A panic while holding this lock cannot leave the map inconsistent.
        self.entries.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl DeviceKeyStore for MemoryKeyStore {
    fn get(&self, vault_id: Id) -> Result<Option<SecretBox<[u8; 32]>>, String> {
        Ok(self
            .lock()
            .get(&vault_id)
            .map(|k| SecretBox::new(Box::new(**k))))
    }

    fn set(&self, vault_id: Id, key: &[u8; 32]) -> Result<(), String> {
        self.lock().insert(vault_id, Zeroizing::new(*key));
        Ok(())
    }

    fn delete(&self, vault_id: Id) -> Result<(), String> {
        self.lock().remove(&vault_id);
        Ok(())
    }
}

impl VaultService {
    /// Enables quick unlock on this device: stores the account secret in the
    /// vault wrapped by a device key. Needs the vault unlocked. Replaces any
    /// previous setup.
    ///
    /// Without a gate or behind Touch ID or Face ID the device key is new and
    /// random and kept in the keychain; behind Touch ID or Face ID the user
    /// confirms once and the current enrollment state is stored with it.
    /// Behind a key-binding gate (Windows Hello) the gate's prompt derives
    /// the key, any keychain device key is deleted first, and nothing goes in
    /// the keychain.
    pub fn enable_quick_unlock(&mut self) -> Result<(), VaultError> {
        if self.gate.binds_keys() {
            return self.enable_bound_quick_unlock();
        }
        let unlocked = self.unlocked.as_ref().ok_or(VaultError::Locked)?;
        let vault_id = unlocked.vault_id;
        let gated = self.gate.checks_presence();
        let state = if gated {
            let state = self.gate.state()?;
            self.gate.confirm(ENABLE_REASON)?;
            Some(state)
        } else {
            None
        };
        let device_key = random_key();
        let wrapped = wrap_secret(
            device_key.expose_secret(),
            unlocked.account.secret.expose_secret(),
            &wrap_label("device", vault_id),
        );
        self.keystore
            .set(vault_id, device_key.expose_secret())
            .map_err(VaultError::Keychain)?;
        self.store.account_put(keys::SECRET_BY_DEVICE, &wrapped)?;
        match state {
            Some(state) => self.store.account_put(keys::BIOMETRY_STATE, &state)?,
            None => self.store.account_delete(keys::BIOMETRY_STATE)?,
        }
        self.store.account_delete(keys::HELLO_CHALLENGE)?;
        Ok(())
    }

    /// [`enable_quick_unlock`](Self::enable_quick_unlock) behind a
    /// key-binding gate: a fresh challenge, the gate's key for it wraps the
    /// account secret. Any keychain device key is deleted before anything is
    /// written, so a failure leaves the previous setup as it was and success
    /// leaves no plain key behind.
    fn enable_bound_quick_unlock(&mut self) -> Result<(), VaultError> {
        let unlocked = self.unlocked.as_ref().ok_or(VaultError::Locked)?;
        let vault_id = unlocked.vault_id;
        self.gate.state()?;
        // Not secret: only the gate's credential turns it into a key.
        let challenge = *random_key().expose_secret();
        let key = self.gate.derive_key(&challenge, ENABLE_REASON, true)?;
        let wrapped = wrap_secret(
            key.expose_secret(),
            unlocked.account.secret.expose_secret(),
            &wrap_label("device", vault_id),
        );
        self.keystore
            .delete(vault_id)
            .map_err(VaultError::Keychain)?;
        let store = &self.store;
        store.atomically(|| -> Result<(), VaultError> {
            store.account_put(keys::HELLO_CHALLENGE, &challenge)?;
            store.account_put(keys::SECRET_BY_DEVICE, &wrapped)?;
            store.account_delete(keys::BIOMETRY_STATE)?;
            Ok(())
        })
    }

    /// Turns quick unlock off: removes the wrapped secret and its companion
    /// rows from the vault (in one transaction), then the device key from the
    /// keychain. Works locked or unlocked. A [`VaultError::Keychain`] error
    /// means quick unlock is already off and only the keychain cleanup failed.
    pub fn disable_quick_unlock(&mut self) -> Result<(), VaultError> {
        let vault_id = self.load_account()?.vault_id;
        let store = &self.store;
        store.atomically(|| -> Result<(), VaultError> {
            store.account_delete(keys::SECRET_BY_DEVICE)?;
            store.account_delete(keys::BIOMETRY_STATE)?;
            store.account_delete(keys::HELLO_CHALLENGE)?;
            Ok(())
        })?;
        self.keystore.delete(vault_id).map_err(VaultError::Keychain)
    }

    /// Turns quick unlock off because its key can no longer work, and returns
    /// [`VaultError::BiometryChanged`] so the reason reaches the user even if
    /// the keychain cleanup fails (the vault rows are gone by then).
    fn turn_off_quick_unlock_changed(&mut self) -> VaultError {
        match self.disable_quick_unlock() {
            Ok(()) => VaultError::BiometryChanged,
            Err(VaultError::Keychain(e)) => {
                // keyring's messages carry no key material.
                tracing::warn!("quick unlock turned off; keychain cleanup failed: {e}");
                VaultError::BiometryChanged
            }
            Err(e) => e,
        }
    }

    /// Whether quick unlock has been enabled for this vault.
    pub fn is_quick_unlock_enabled(&self) -> bool {
        matches!(self.store.account_get(keys::SECRET_BY_DEVICE), Ok(Some(_)))
    }

    /// Unlocks with the device key from the keychain, without the password.
    ///
    /// [`VaultError::Keychain`] when the keychain is unavailable, fails, or no
    /// longer holds a matching key; [`VaultError::QuickUnlockNotEnabled`] when
    /// it was never enabled. Password unlock keeps working in every case.
    ///
    /// Behind a presence gate the device key is read only after the user
    /// confirms ([`VaultError::PresenceDenied`] otherwise, or
    /// [`VaultError::BiometryUnavailable`] when the check cannot run). If the
    /// enrolled fingers changed since quick unlock was enabled, it is turned
    /// off and [`VaultError::BiometryChanged`] returned without asking. A
    /// vault enabled without a gate records the state on its first gated
    /// unlock.
    ///
    /// Behind a key-binding gate the prompt derives the wrapping key. A key
    /// that no longer opens the wrap (Windows Hello reset), a platform
    /// credential that is gone (reported by the gate, which then never
    /// prompts or creates one) or a missing challenge turns quick unlock off
    /// and returns
    /// [`VaultError::BiometryChanged`], once; a cancel or an unavailable gate
    /// leaves it on. A vault enabled behind a key-binding gate but opened
    /// behind another gate has no keychain key to read, so it is turned off
    /// the same way, without a prompt.
    pub fn quick_unlock(&mut self) -> Result<(), VaultError> {
        let acct = self.load_account()?;
        let wrapped = acct
            .secret_by_device
            .as_deref()
            .ok_or(VaultError::QuickUnlockNotEnabled)?;
        let challenge = self.store.account_get(keys::HELLO_CHALLENGE)?;
        if self.gate.binds_keys() {
            self.gate.state()?;
            let Some(challenge) = challenge else {
                return Err(self.turn_off_quick_unlock_changed());
            };
            let key = match self.gate.derive_key(&challenge, UNLOCK_REASON, false) {
                // The platform credential is gone: never create one here.
                Err(VaultError::BiometryChanged) => {
                    return Err(self.turn_off_quick_unlock_changed());
                }
                other => other?,
            };
            let secret = match unwrap_secret(
                key.expose_secret(),
                wrapped,
                &wrap_label("device", acct.vault_id),
            ) {
                Ok(secret) => secret,
                Err(CryptoError::Decrypt) => return Err(self.turn_off_quick_unlock_changed()),
                Err(e) => return Err(VaultError::Crypto(e)),
            };
            return self.set_unlocked(open_account(&acct, &secret)?);
        }
        if challenge.is_some() {
            return Err(self.turn_off_quick_unlock_changed());
        }
        let mut record_state = None;
        if self.gate.checks_presence() {
            let state = self.gate.state()?;
            match self.store.account_get(keys::BIOMETRY_STATE)? {
                Some(stored) if stored != state => {
                    self.disable_quick_unlock()?;
                    return Err(VaultError::BiometryChanged);
                }
                Some(_) => {}
                None => record_state = Some(state),
            }
            self.gate.confirm(UNLOCK_REASON)?;
        }
        let device_key = self
            .keystore
            .get(acct.vault_id)
            .map_err(VaultError::Keychain)?
            .ok_or_else(|| VaultError::Keychain("device key missing from keychain".into()))?;
        let secret = unwrap_secret(
            device_key.expose_secret(),
            wrapped,
            &wrap_label("device", acct.vault_id),
        )
        .map_err(|e| match e {
            CryptoError::Decrypt => {
                VaultError::Keychain("keychain device key does not match this vault".into())
            }
            e => VaultError::Crypto(e),
        })?;
        self.set_unlocked(open_account(&acct, &secret)?)?;
        if let Some(state) = record_state {
            self.store.account_put(keys::BIOMETRY_STATE, &state)?;
        }
        Ok(())
    }
}
