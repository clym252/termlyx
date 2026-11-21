//! Windows checks that need an interactive session: Credential Manager and
//! Windows Hello refuse SSH key sessions. Run by hand from an RDP or console
//! session on the Windows machine:
//!
//! ```text
//! cargo test -p core-api -p core-sync --test windows_interactive -- --ignored --nocapture
//! ```
//!
//! The sync-token keyring check lives in `crates/core-sync/tests/` under the
//! same file name (core-sync depends on core-api, not the other way round).
#![cfg(windows)]

use core_api::vault::{DeviceKeyStore, KeyringStore, PresenceGate, WindowsHelloGate};
use secrecy::ExposeSecret;

/// A device key survives a Credential Manager round trip and is gone after
/// delete. Uses a throwaway vault id and cleans up after itself.
#[test]
#[ignore = "needs an interactive Windows session (Credential Manager)"]
fn keyring_round_trip_device_key() {
    let store = KeyringStore::new();
    let vault_id = uuid::Uuid::now_v7();
    let key = [0x5au8; 32];

    store.set(vault_id, &key).expect("set");
    let got = store.get(vault_id).expect("get").expect("stored key");
    let equal = got.expose_secret() == &key;
    store.delete(vault_id).expect("delete");
    assert!(equal, "read back a different key");
    assert!(store.get(vault_id).expect("get after delete").is_none());
    // Deleting a missing entry is not an error.
    store.delete(vault_id).expect("second delete");
}

/// Two Windows Hello signatures of the same challenge give the same key
/// (RSA PKCS#1 v1.5 is deterministic). Prompts twice; three times when the
/// Tether credential is created first. Prints only whether the keys match.
#[test]
#[ignore = "needs an interactive Windows session and Windows Hello (prompts)"]
fn hello_derive_key_is_stable() {
    let gate = WindowsHelloGate;
    gate.state().expect("Windows Hello available");
    let challenge = [0x42u8; 32];
    let first = gate
        .derive_key(&challenge, "Tether test: first signature", true)
        .expect("first derive");
    let second = gate
        .derive_key(&challenge, "Tether test: second signature", false)
        .expect("second derive");
    let equal = first.expose_secret() == second.expose_secret();
    println!("equal: {equal}");
    assert!(equal);
}
