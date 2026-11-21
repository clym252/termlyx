//! Where a device keeps its sync credentials: the refresh token and the
//! device secret (`TokenResp.deviceSecret`). Together they let a device get
//! back in without the user's password, so they live in the OS keychain, not
//! in the database.
//!
//! The file fallback (`sync_state`, used when the keychain errors) is the
//! sync engine's job: it supplies its own [`TokenStore`].

use std::collections::HashMap;
use std::fmt;
use std::sync::Mutex;

use core_model::Id;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

use crate::error::SyncError;

/// A device's long-lived credentials.
pub struct DeviceCredentials {
    /// The current (single-use) refresh token.
    pub refresh: SecretString,
    /// The per-device secret; proves a password re-login comes from this
    /// device. Never changes after the device is created.
    pub device_secret: SecretString,
}

impl Clone for DeviceCredentials {
    fn clone(&self) -> Self {
        Self {
            refresh: SecretString::from(self.refresh.expose_secret().to_owned()),
            device_secret: SecretString::from(self.device_secret.expose_secret().to_owned()),
        }
    }
}

impl fmt::Debug for DeviceCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeviceCredentials")
            .field("refresh", &"[REDACTED]")
            .field("device_secret", &"[REDACTED]")
            .finish()
    }
}

/// Storage for [`DeviceCredentials`], one entry per device id. Blocking: the
/// client calls it from `spawn_blocking`.
pub trait TokenStore: Send + Sync {
    /// The credentials saved for `device`, if any.
    fn load(&self, device: Id) -> Result<Option<DeviceCredentials>, SyncError>;
    /// Saves (replacing) the credentials for `device`.
    fn save(&self, device: Id, creds: &DeviceCredentials) -> Result<(), SyncError>;
    /// Forgets the credentials for `device`; a missing entry is not an error.
    fn delete(&self, device: Id) -> Result<(), SyncError>;
    /// Checked before a refresh rotates the token: fails with
    /// [`SyncError::StorageLocked`] while the store could not save the
    /// rotated token (the iOS keychain while the device is locked), so the
    /// server never rotates into a token this device would then lose. Any
    /// other answer lets the refresh go ahead. Default: always ready.
    fn ready(&self, device: Id) -> Result<(), SyncError> {
        let _ = device;
        Ok(())
    }
}

/// How both secrets sit in one keychain item.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BlobRef<'a> {
    refresh: &'a str,
    device_secret: &'a str,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Blob {
    refresh: String,
    device_secret: String,
}

impl Drop for Blob {
    fn drop(&mut self) {
        self.refresh.zeroize();
        self.device_secret.zeroize();
    }
}

fn encode(creds: &DeviceCredentials) -> Result<Zeroizing<Vec<u8>>, SyncError> {
    serde_json::to_vec(&BlobRef {
        refresh: creds.refresh.expose_secret(),
        device_secret: creds.device_secret.expose_secret(),
    })
    .map(Zeroizing::new)
    .map_err(|_| SyncError::Storage("could not encode credentials".into()))
}

fn decode(bytes: &[u8]) -> Result<DeviceCredentials, SyncError> {
    let mut blob: Blob = serde_json::from_slice(bytes)
        .map_err(|_| SyncError::Storage("stored credentials are unreadable".into()))?;
    Ok(DeviceCredentials {
        refresh: SecretString::from(std::mem::take(&mut blob.refresh)),
        device_secret: SecretString::from(std::mem::take(&mut blob.device_secret)),
    })
}

/// [`TokenStore`] backed by the OS credential store (macOS Keychain, Windows
/// Credential Manager, Secret Service, iOS data-protection keychain: see
/// [`core_api::vault::keychain_entry`]). One item per device: service
/// [`KeyringTokenStore::SERVICE`], account = the device id, secret = a small
/// JSON blob holding the refresh token and the device secret.
#[derive(Debug, Default, Clone, Copy)]
pub struct KeyringTokenStore;

impl KeyringTokenStore {
    /// Keychain service name for sync credentials.
    pub const SERVICE: &'static str = "app.tether.sync-token";

    /// A store using the platform keychain. Nothing is accessed until used.
    pub fn new() -> Self {
        Self
    }

    fn entry(device: Id) -> Result<keyring_core::Entry, SyncError> {
        core_api::vault::keychain_entry(Self::SERVICE, &device.to_string()).map_err(storage_error)
    }
}

/// A keychain failure as a [`SyncError`]: a device that is only locked
/// (iOS) is [`SyncError::StorageLocked`], anything else
/// [`SyncError::Storage`]. keyring's `Display` never includes secret bytes.
fn storage_error(e: keyring_core::Error) -> SyncError {
    if core_api::vault::is_device_locked(&e) {
        SyncError::StorageLocked
    } else {
        SyncError::Storage(e.to_string())
    }
}

impl TokenStore for KeyringTokenStore {
    fn load(&self, device: Id) -> Result<Option<DeviceCredentials>, SyncError> {
        match Self::entry(device)?.get_secret() {
            Ok(bytes) => decode(&Zeroizing::new(bytes)).map(Some),
            Err(keyring_core::Error::NoEntry) => Ok(None),
            Err(e) => Err(storage_error(e)),
        }
    }

    fn save(&self, device: Id, creds: &DeviceCredentials) -> Result<(), SyncError> {
        Self::entry(device)?
            .set_secret(&encode(creds)?)
            .map_err(storage_error)
    }

    fn delete(&self, device: Id) -> Result<(), SyncError> {
        match Self::entry(device)?.delete_credential() {
            Ok(()) | Err(keyring_core::Error::NoEntry) => Ok(()),
            Err(e) => Err(storage_error(e)),
        }
    }

    /// iOS: a read of the item (readable exactly when writable: both need
    /// the device unlocked). Elsewhere no probe, so the desktop keychain
    /// sees no extra access.
    fn ready(&self, device: Id) -> Result<(), SyncError> {
        if !cfg!(target_os = "ios") {
            return Ok(());
        }
        match Self::entry(device)?.get_secret() {
            Ok(bytes) => {
                drop(Zeroizing::new(bytes));
                Ok(())
            }
            Err(e) if core_api::vault::is_device_locked(&e) => Err(SyncError::StorageLocked),
            // Anything else is for the save after the refresh to report.
            Err(_) => Ok(()),
        }
    }
}

/// In-memory [`TokenStore`] for tests.
#[derive(Default)]
pub struct MemoryTokenStore {
    entries: Mutex<HashMap<Id, DeviceCredentials>>,
}

impl MemoryTokenStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<Id, DeviceCredentials>> {
        // A panic while holding the lock cannot leave the map inconsistent.
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl TokenStore for MemoryTokenStore {
    fn load(&self, device: Id) -> Result<Option<DeviceCredentials>, SyncError> {
        Ok(self.lock().get(&device).cloned())
    }

    fn save(&self, device: Id, creds: &DeviceCredentials) -> Result<(), SyncError> {
        self.lock().insert(device, creds.clone());
        Ok(())
    }

    fn delete(&self, device: Id) -> Result<(), SyncError> {
        self.lock().remove(&device);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn creds(r: &str, d: &str) -> DeviceCredentials {
        DeviceCredentials {
            refresh: SecretString::from(r.to_owned()),
            device_secret: SecretString::from(d.to_owned()),
        }
    }

    #[test]
    fn debug_is_redacted() {
        let text = format!("{:?}", creds("CANARY-REFRESH", "CANARY-SECRET"));
        assert!(!text.contains("CANARY"), "{text}");
        assert!(text.contains("[REDACTED]"));
    }

    #[test]
    fn keychain_blob_round_trips() {
        let bytes = encode(&creds("r-1", "s-1")).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"refresh": "r-1", "deviceSecret": "s-1"})
        );
        let back = decode(&bytes).unwrap();
        assert_eq!(back.refresh.expose_secret(), "r-1");
        assert_eq!(back.device_secret.expose_secret(), "s-1");
        assert!(matches!(decode(b"not json"), Err(SyncError::Storage(_))));
        assert!(matches!(
            decode(br#"{"refresh":"x"}"#),
            Err(SyncError::Storage(_))
        ));
    }

    #[test]
    fn memory_store_save_load_delete() {
        let store = MemoryTokenStore::new();
        let id = Id::from_u128(7);
        assert!(store.load(id).unwrap().is_none());
        store.save(id, &creds("a", "b")).unwrap();
        let got = store.load(id).unwrap().unwrap();
        assert_eq!(got.refresh.expose_secret(), "a");
        store.save(id, &creds("c", "b")).unwrap();
        assert_eq!(
            store.load(id).unwrap().unwrap().refresh.expose_secret(),
            "c"
        );
        store.delete(id).unwrap();
        store.delete(id).unwrap();
        assert!(store.load(id).unwrap().is_none());
    }
}
