//! The one way Tether opens an OS credential store entry: the quick unlock
//! device key ([`KeyringStore`](super::KeyringStore)) and the sync tokens
//! (`core_sync::KeyringTokenStore`) both go through [`keychain_entry`].
//!
//! Desktop: `keyring`'s own platform store (macOS Keychain, Windows
//! Credential Manager, Secret Service).
//!
//! iOS: `keyring` refuses iOS, so [`ensure_store`] sets the data-protection
//! keychain (`apple-native-keyring-store`, `protected`) as `keyring-core`'s
//! default store once, before the first entry. That store keeps items in
//! the local (not iCloud) keychain but defaults to `WhenUnlocked`, which
//! still moves to a new phone in an encrypted backup; every entry is built
//! with `WhenUnlockedThisDeviceOnly` instead: readable only while the device
//! is unlocked, never synced, never restored to another device.
//!
//! Android: [`ensure_store`] sets `android-native-keyring-store` as the default
//! store once. Each entry is AES-GCM encrypted with a non-exportable Android
//! Keystore key that needs no user authentication (sync runs while the screen
//! is locked) and kept in an app-private SharedPreferences file, which the
//! manifest excludes from backup and device transfer.
//!
//! So on iOS the keychain is out of reach while the device is locked (an app
//! kept alive in the background still runs then): [`is_device_locked`] tells
//! that transient refusal apart from a keychain that does not work at all.

use keyring_core::{Entry, Result};

/// The entry for `service` / `account` in this platform's credential store.
pub fn keychain_entry(service: &str, account: &str) -> Result<Entry> {
    #[cfg(target_os = "android")]
    {
        ensure_store();
        Entry::new(service, account)
    }
    #[cfg(not(any(target_os = "ios", target_os = "android")))]
    {
        keyring::Entry::new(service, account).map(|e| e.inner)
    }
    #[cfg(target_os = "ios")]
    {
        ensure_store();
        let modifiers = std::collections::HashMap::from([("access-policy", IOS_ACCESS_POLICY)]);
        Entry::new_with_modifiers(service, account, &modifiers)
    }
}

/// `kSecAttrAccessibleWhenUnlockedThisDeviceOnly`, as the protected store
/// spells it.
#[cfg(target_os = "ios")]
const IOS_ACCESS_POLICY: &str = "WhenUnlockedThisDeviceOnly";

/// Sets the data-protection keychain as `keyring-core`'s default store, once.
#[cfg(target_os = "ios")]
fn ensure_store() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(
        || match apple_native_keyring_store::protected::Store::new() {
            Ok(store) => keyring_core::set_default_store(store),
            // Entries then fail with NoDefaultStore, which callers report.
            Err(e) => tracing::error!(error = %e, "no iOS keychain store"),
        },
    );
}

/// Sets the Android Keystore store as `keyring-core`'s default store, once.
/// It reads the app context from `ndk-context`, which tao sets up before any
/// Rust code of the app runs.
#[cfg(target_os = "android")]
fn ensure_store() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| match android_native_keyring_store::Store::new() {
        Ok(store) => keyring_core::set_default_store(store),
        // Entries then fail with NoDefaultStore, which callers report.
        Err(e) => tracing::error!(error = %e, "no Android credential store"),
    });
}

/// `errSecInteractionNotAllowed`: the item's data-protection class is not
/// available (the device is locked).
#[cfg(any(target_os = "macos", target_os = "ios"))]
const ERR_SEC_INTERACTION_NOT_ALLOWED: i32 = -25308;

/// Whether `e` only says the device is locked: iOS refuses
/// `WhenUnlockedThisDeviceOnly` items until it is unlocked again. Transient:
/// the same entry works after the unlock. Always `false` on other platforms
/// (a locked macOS login keychain prompts, it does not refuse this way).
pub fn is_device_locked(e: &keyring_core::Error) -> bool {
    cfg!(target_os = "ios") && is_interaction_not_allowed(e)
}

/// `e` carries `errSecInteractionNotAllowed` (the protected store reports it
/// as a platform failure holding the `security-framework` error).
#[cfg(any(target_os = "macos", target_os = "ios"))]
fn is_interaction_not_allowed(e: &keyring_core::Error) -> bool {
    match e {
        keyring_core::Error::PlatformFailure(inner)
        | keyring_core::Error::NoStorageAccess(inner) => inner
            .downcast_ref::<security_framework::base::Error>()
            .is_some_and(|err| err.code() == ERR_SEC_INTERACTION_NOT_ALLOWED),
        _ => false,
    }
}

#[cfg(not(any(target_os = "macos", target_os = "ios")))]
fn is_interaction_not_allowed(_: &keyring_core::Error) -> bool {
    false
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    fn platform(code: i32) -> keyring_core::Error {
        keyring_core::Error::PlatformFailure(Box::new(security_framework::base::Error::from_code(
            code,
        )))
    }

    #[test]
    fn interaction_not_allowed_is_recognised() {
        assert!(is_interaction_not_allowed(&platform(-25308)));
        assert!(!is_interaction_not_allowed(&platform(-25291)));
        assert!(!is_interaction_not_allowed(&keyring_core::Error::NoEntry));
        assert!(!is_interaction_not_allowed(
            &keyring_core::Error::PlatformFailure("other".into())
        ));
        // Only iOS treats it as a locked device; desktop is unchanged.
        assert!(!is_device_locked(&platform(-25308)));
    }
}
