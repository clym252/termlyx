//! Windows checks that need an interactive session: Credential Manager
//! refuses SSH key sessions. Run by hand from an RDP or console session on
//! the Windows machine, together with core-api's file of the same name:
//!
//! ```text
//! cargo test -p core-api -p core-sync --test windows_interactive -- --ignored --nocapture
//! ```
#![cfg(windows)]

use core_sync::{DeviceCredentials, KeyringTokenStore, TokenStore};
use secrecy::{ExposeSecret, SecretString};

/// Sync credentials survive a Credential Manager round trip and are gone
/// after delete. Uses a throwaway device id and cleans up after itself.
#[test]
#[ignore = "needs an interactive Windows session (Credential Manager)"]
fn keyring_round_trip_sync_token() {
    let store = KeyringTokenStore::new();
    let device = uuid::Uuid::now_v7();
    let creds = DeviceCredentials {
        refresh: SecretString::from("test-refresh-token".to_owned()),
        device_secret: SecretString::from("test-device-secret".to_owned()),
    };

    store.save(device, &creds).expect("save");
    let got = store
        .load(device)
        .expect("load")
        .expect("stored credentials");
    let equal = got.refresh.expose_secret() == "test-refresh-token"
        && got.device_secret.expose_secret() == "test-device-secret";
    store.delete(device).expect("delete");
    assert!(equal, "read back different credentials");
    assert!(store.load(device).expect("load after delete").is_none());
    // Deleting a missing entry is not an error.
    store.delete(device).expect("second delete");
}
