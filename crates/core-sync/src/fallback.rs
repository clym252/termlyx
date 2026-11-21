//! The credential store fallback (spec "Local storage changes"): where no
//! keychain works (Linux without a secret service, a locked-down keychain),
//! the refresh token and device secret live in the vault's `sync_state`
//! table instead. They are not behind the vault key, as in the keychain, so
//! a locked device still syncs; they only reach ciphertext, and revoking the
//! device kills them.
//!
//! Which store is in use is remembered in `sync_state` (`token_store`), so a
//! restart picks the same one.

use std::sync::Arc;

use core_api::vault::{SharedVault, VaultService};
use core_model::Id;
use secrecy::{ExposeSecret, SecretString};
use zeroize::Zeroizing;

use crate::client::Client;
use crate::error::SyncError;
use crate::state::keys;
use crate::tokens::{DeviceCredentials, TokenStore};

const KEY_DEVICE: &str = "token_device";
const KEY_REFRESH: &str = "refresh_token";
const KEY_SECRET: &str = "device_secret";

const STORE_KEYCHAIN: &[u8] = b"keychain";
const STORE_FILE: &[u8] = b"file";

/// [`TokenStore`] over the vault's `sync_state` table (one device per vault).
#[derive(Clone)]
pub struct SyncStateTokenStore {
    vault: SharedVault,
}

impl SyncStateTokenStore {
    /// A store writing into `vault`'s `sync_state`.
    pub fn new(vault: SharedVault) -> Self {
        Self { vault }
    }

    fn with<T>(
        &self,
        f: impl FnOnce(&mut VaultService) -> Result<T, core_api::vault::VaultError>,
    ) -> Result<T, SyncError> {
        let mut v = self.vault.lock().unwrap_or_else(|p| p.into_inner());
        f(&mut v).map_err(|e| SyncError::Storage(e.to_string()))
    }
}

fn text(b: Vec<u8>) -> Result<SecretString, SyncError> {
    let b = Zeroizing::new(b);
    std::str::from_utf8(&b)
        .map(|s| SecretString::from(s.to_owned()))
        .map_err(|_| SyncError::Storage("stored credentials are unreadable".into()))
}

impl TokenStore for SyncStateTokenStore {
    fn load(&self, device: Id) -> Result<Option<DeviceCredentials>, SyncError> {
        let (owner, refresh, secret) = self.with(|v| {
            Ok((
                v.sync_get(KEY_DEVICE)?,
                v.sync_get(KEY_REFRESH)?,
                v.sync_get(KEY_SECRET)?,
            ))
        })?;
        match (owner, refresh, secret) {
            (Some(owner), Some(refresh), Some(secret))
                if owner == device.to_string().as_bytes() =>
            {
                Ok(Some(DeviceCredentials {
                    refresh: text(refresh)?,
                    device_secret: text(secret)?,
                }))
            }
            _ => Ok(None),
        }
    }

    fn save(&self, device: Id, creds: &DeviceCredentials) -> Result<(), SyncError> {
        let device = device.to_string();
        self.with(|v| {
            v.sync_put_all(&[
                (KEY_DEVICE, device.as_bytes()),
                (KEY_REFRESH, creds.refresh.expose_secret().as_bytes()),
                (KEY_SECRET, creds.device_secret.expose_secret().as_bytes()),
            ])
        })
    }

    fn delete(&self, _device: Id) -> Result<(), SyncError> {
        self.with(|v| {
            v.sync_delete(KEY_DEVICE)?;
            v.sync_delete(KEY_REFRESH)?;
            v.sync_delete(KEY_SECRET)
        })
    }
}

/// Whether the fallback holds this vault's credentials.
pub(crate) fn uses_fallback(v: &VaultService) -> Result<bool, SyncError> {
    Ok(v.sync_get(keys::TOKEN_STORE)?.as_deref() == Some(STORE_FILE))
}

/// The store holding this vault's credentials: the fallback if it was
/// switched to, else `primary` (the keychain in the app).
pub(crate) fn credential_store(
    v: &VaultService,
    vault: &SharedVault,
    primary: Arc<dyn TokenStore>,
) -> Result<Arc<dyn TokenStore>, SyncError> {
    Ok(if uses_fallback(v)? {
        Arc::new(SyncStateTokenStore::new(vault.clone()))
    } else {
        primary
    })
}

/// Moves the client's current credentials into the fallback store and
/// remembers that choice.
pub(crate) async fn switch_to_fallback(
    client: &Client,
    vault: &SharedVault,
) -> Result<(), SyncError> {
    tracing::warn!("keychain unavailable; keeping sync credentials in the vault database");
    client.set_store(Arc::new(SyncStateTokenStore::new(vault.clone())));
    client.persist().await?;
    let mut v = vault.lock().unwrap_or_else(|p| p.into_inner());
    v.sync_put(keys::TOKEN_STORE, STORE_FILE)?;
    Ok(())
}

/// Saves the client's credentials (held in memory since the sign-in) to
/// `primary`, or to the fallback if `primary` fails with
/// [`SyncError::Storage`], and records which one holds them.
pub(crate) async fn save_credentials(
    client: &Client,
    vault: &SharedVault,
    primary: Arc<dyn TokenStore>,
) -> Result<(), SyncError> {
    client.set_store(primary);
    match client.persist().await {
        Ok(()) => {
            let mut v = vault.lock().unwrap_or_else(|p| p.into_inner());
            v.sync_put(keys::TOKEN_STORE, STORE_KEYCHAIN)?;
            Ok(())
        }
        Err(SyncError::Storage(_)) => switch_to_fallback(client, vault).await,
        Err(e) => Err(e),
    }
}
