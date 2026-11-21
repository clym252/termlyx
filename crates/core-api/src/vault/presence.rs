//! The user-presence check in front of quick unlock: Touch ID on macOS,
//! Face ID or Touch ID on iOS, Windows Hello on Windows, the Android Keystore
//! on Android, nothing elsewhere.
//!
//! The check is enforced by Tether, not by the keychain: a keychain item
//! with a biometric `SecAccessControl` needs the data-protection keychain,
//! which needs a signed build with a keychain-access-groups entitlement.
//! Until builds are signed the device key stays a plain login-keychain item
//! (other processes still hit its ACL prompt) and this gate decides whether
//! Tether reads it.
//!
//! iOS uses the same gate. Its device key is already in the data-protection
//! keychain, this-device-only, where only Tether can read it (the app
//! sandbox), so the gate stays Tether's there too; `docs/crypto.md` ("Quick
//! unlock on iOS") says why no `SecAccessControl` is added.
//!
//! The gate also reports the biometric enrollment state (a hash of the
//! enrolled fingers or faces). Quick unlock stores it when enabled and turns
//! itself off when it changes, as a `biometryCurrentSet` access control
//! would.
//!
//! A key-binding gate ([`PresenceGate::binds_keys`], Windows Hello, Android)
//! does more than say yes: its prompt produces the key that wraps the
//! account secret ([`PresenceGate::derive_key`]), so no keychain entry exists
//! for another program (or anyone holding the unlocked phone) to read.
//! `docs/crypto.md` ("Quick unlock on Windows", "Quick unlock on Android")
//! has the derivations.

use secrecy::SecretBox;
use serde::Serialize;
use zeroize::Zeroizing;

use super::VaultError;

/// What stands in front of quick unlock. Serializes as `"none"`,
/// `"touchId"`, `"faceId"`, `"windowsHello"` or `"androidBiometric"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum GateKind {
    /// No presence check: the keychain alone decides (Phase 1 behaviour).
    None,
    /// Touch ID (macOS, iPad, iPhone with a home button), with the master
    /// password as the fallback.
    TouchId,
    /// Face ID (iPhone, iPad Pro), with the master password as the fallback.
    FaceId,
    /// Windows Hello (PIN, fingerprint or face), with the master password as
    /// the fallback. A key-binding gate.
    WindowsHello,
    /// A strong-class fingerprint or face through the Android Keystore, with
    /// the master password as the fallback. A key-binding gate.
    AndroidBiometric,
}

/// A user-presence check. Implementations block the calling thread while
/// the system asks the user.
pub trait PresenceGate: Send {
    fn kind(&self) -> GateKind;
    /// Whether quick unlock asks this gate before it opens the vault. Only
    /// [`NoGate`] does not; a biometric gate that cannot name its sensor
    /// (kind [`GateKind::None`]) still asks, and fails if it cannot.
    fn checks_presence(&self) -> bool {
        self.kind() != GateKind::None
    }
    /// The current biometric enrollment state. Errors with
    /// [`VaultError::BiometryUnavailable`] when the check cannot run now (no
    /// sensor, nothing enrolled, lid closed, locked out).
    fn state(&self) -> Result<Vec<u8>, VaultError>;
    /// Asks the user to confirm with `reason` shown. Errors with
    /// [`VaultError::PresenceDenied`] when they cancel, fail, or choose the
    /// master password instead.
    fn confirm(&self, reason: &str) -> Result<(), VaultError>;
    /// Whether this gate produces quick unlock's wrapping key itself
    /// ([`derive_key`](Self::derive_key)) instead of guarding a keychain
    /// entry. Such a vault keeps no device key in the keychain.
    fn binds_keys(&self) -> bool {
        false
    }
    /// The 32-byte key that wraps the account secret, derived from
    /// `challenge`. For a key-binding gate this *is* the user prompt, with
    /// `reason` shown: [`VaultError::PresenceDenied`] when the user cancels,
    /// [`VaultError::BiometryUnavailable`] when it cannot run now. The same
    /// challenge gives the same key until the platform credential is reset.
    ///
    /// `create` is true only when enabling: a missing platform credential is
    /// then created (itself a prompt). When unlocking (`create` false) a
    /// missing credential is [`VaultError::BiometryChanged`], without a
    /// prompt, and the core turns quick unlock off. A reset credential that
    /// still exists must yield a *different* key: the unwrap then fails and
    /// the core turns quick unlock off the same way. Never report either as
    /// `BiometryUnavailable`, which leaves quick unlock on and failing on
    /// every attempt.
    fn derive_key(
        &self,
        _challenge: &[u8],
        _reason: &str,
        _create: bool,
    ) -> Result<SecretBox<[u8; 32]>, VaultError> {
        Err(VaultError::BiometryUnavailable(
            "this gate does not bind keys".into(),
        ))
    }
}

/// No presence check (platforms without one, and tests).
#[derive(Debug, Default, Clone, Copy)]
pub struct NoGate;

impl PresenceGate for NoGate {
    fn kind(&self) -> GateKind {
        GateKind::None
    }

    fn state(&self) -> Result<Vec<u8>, VaultError> {
        Ok(Vec::new())
    }

    fn confirm(&self, _reason: &str) -> Result<(), VaultError> {
        Ok(())
    }
}

/// The gate [`VaultService::open`](super::VaultService::open) uses on this
/// platform.
pub fn default_gate() -> Box<dyn PresenceGate> {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    {
        Box::new(biometric::BiometricGate)
    }
    #[cfg(windows)]
    {
        Box::new(windows_hello::WindowsHelloGate)
    }
    // The app swaps in the Keystore plugin's gate; until then (and in
    // anything else that opens the vault) quick unlock is unavailable, never
    // an ungated keychain key.
    #[cfg(target_os = "android")]
    {
        Box::new(KeystoreGate(UnreachableKeystore))
    }
    #[cfg(not(any(target_os = "macos", target_os = "ios", windows, target_os = "android")))]
    {
        Box::new(NoGate)
    }
}

/// What the Android Keystore said instead of a MAC. Carries no secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeystoreError {
    /// The user cancelled the prompt or chose the master password.
    Cancelled,
    /// The prompt cannot run now (no strong biometrics enrolled, too many
    /// attempts, no sensor), as a whole sentence for the user.
    Unavailable(String),
    /// Biometrics were enrolled or removed since the key was made: Android
    /// invalidated it for good.
    Invalidated,
    /// There is no key (and `create` was false).
    Missing,
}

/// The Android Keystore half of [`KeystoreGate`]: the app's plugin, or a
/// fake in tests.
pub trait KeystoreBackend: Send {
    /// HMAC-SHA256 of `challenge` under Tether's Keystore key, after a
    /// strong-biometric prompt showing `reason`. `create` (enabling) makes
    /// the key when it is missing or invalidated, replacing an invalidated
    /// one; otherwise those are [`KeystoreError::Missing`] and
    /// [`KeystoreError::Invalidated`], reported before any prompt.
    fn hmac(
        &self,
        challenge: &[u8],
        reason: &str,
        create: bool,
    ) -> Result<Zeroizing<Vec<u8>>, KeystoreError>;
}

/// Android quick unlock: a key-binding gate whose wrapping key is
/// [`core_crypto::keystore_key`] of a Keystore HMAC (`docs/crypto.md`,
/// "Quick unlock on Android"). Built on every target so its tests run
/// anywhere.
pub struct KeystoreGate<B: KeystoreBackend>(pub B);

impl<B: KeystoreBackend> PresenceGate for KeystoreGate<B> {
    fn kind(&self) -> GateKind {
        GateKind::AndroidBiometric
    }

    fn binds_keys(&self) -> bool {
        true
    }

    /// No enrollment hash to compare: Android invalidates the key instead.
    fn state(&self) -> Result<Vec<u8>, VaultError> {
        Ok(Vec::new())
    }

    /// Never used: quick unlock asks through [`derive_key`](PresenceGate::derive_key).
    fn confirm(&self, _reason: &str) -> Result<(), VaultError> {
        Ok(())
    }

    fn derive_key(
        &self,
        challenge: &[u8],
        reason: &str,
        create: bool,
    ) -> Result<SecretBox<[u8; 32]>, VaultError> {
        match self.0.hmac(challenge, reason, create) {
            Ok(mac) => Ok(core_crypto::keystore_key(&mac)),
            Err(KeystoreError::Cancelled) => Err(VaultError::PresenceDenied),
            Err(KeystoreError::Unavailable(why)) => Err(VaultError::BiometryUnavailable(why)),
            Err(KeystoreError::Invalidated) => Err(VaultError::BiometryChanged),
            Err(KeystoreError::Missing) if !create => Err(VaultError::BiometryChanged),
            Err(KeystoreError::Missing) => Err(VaultError::BiometryUnavailable(
                "The biometric key could not be made. Try again.".into(),
            )),
        }
    }
}

/// The Keystore when the app's plugin cannot be reached: every prompt is
/// [`KeystoreError::Unavailable`].
pub struct UnreachableKeystore;

impl KeystoreBackend for UnreachableKeystore {
    fn hmac(&self, _: &[u8], _: &str, _: bool) -> Result<Zeroizing<Vec<u8>>, KeystoreError> {
        Err(KeystoreError::Unavailable(
            "Biometric unlock is not available in this build.".into(),
        ))
    }
}

/// The kind of [`default_gate`] on this device (on iOS: Face ID or Touch
/// ID). Quick unlock's error words follow it.
pub(super) fn device_kind() -> GateKind {
    default_gate().kind()
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub use biometric::BiometricGate;
#[cfg(windows)]
pub use windows_hello::{WindowsHelloGate, set_prompt_owner};

#[cfg(any(target_os = "macos", target_os = "ios"))]
mod biometric {
    use std::sync::mpsc;

    use block2::RcBlock;
    use objc2::runtime::Bool;
    use objc2_foundation::{NSError, NSString};
    use objc2_local_authentication::{LABiometryType, LAContext, LAPolicy};

    use super::{GateKind, PresenceGate, VaultError};

    /// Touch ID (macOS), or Face ID or Touch ID (iOS), through
    /// LocalAuthentication (biometry only: no device password; the fallback
    /// button means "use the master password").
    #[derive(Debug, Default, Clone, Copy)]
    pub struct BiometricGate;

    const POLICY: LAPolicy = LAPolicy::DeviceOwnerAuthenticationWithBiometrics;

    fn unavailable(e: &NSError) -> VaultError {
        VaultError::BiometryUnavailable(e.localizedDescription().to_string())
    }

    /// The gate kind for a device's biometry. Only iOS asks: a Mac says
    /// Touch ID even without a sensor (the check then reports why it cannot
    /// run), as it always has.
    #[cfg_attr(not(target_os = "ios"), allow(dead_code))]
    pub(super) fn kind_for(biometry: LABiometryType) -> GateKind {
        match biometry {
            LABiometryType::FaceID => GateKind::FaceId,
            LABiometryType::TouchID => GateKind::TouchId,
            _ => GateKind::None,
        }
    }

    impl PresenceGate for BiometricGate {
        #[cfg(target_os = "macos")]
        fn kind(&self) -> GateKind {
            GateKind::TouchId
        }

        #[cfg(target_os = "ios")]
        fn kind(&self) -> GateKind {
            // SAFETY: plain LocalAuthentication calls on a fresh context.
            unsafe {
                let ctx = LAContext::new();
                // Sets `biometryType` to the device's sensor whatever it
                // answers (nothing enrolled, locked out).
                let _ = ctx.canEvaluatePolicy_error(POLICY);
                kind_for(ctx.biometryType())
            }
        }

        /// Asks even when iOS names no sensor: quick unlock never skips the
        /// check behind this gate.
        fn checks_presence(&self) -> bool {
            true
        }

        fn state(&self) -> Result<Vec<u8>, VaultError> {
            // SAFETY: plain LocalAuthentication calls on a fresh context.
            unsafe {
                let ctx = LAContext::new();
                ctx.canEvaluatePolicy_error(POLICY)
                    .map_err(|e| unavailable(&e))?;
                // `domainState` replaces it from macOS 15 and iOS 18; this
                // one covers every supported macOS, and iOS keeps it.
                #[allow(deprecated)]
                let state = ctx.evaluatedPolicyDomainState();
                state
                    .map(|d| d.to_vec())
                    .ok_or_else(|| VaultError::BiometryUnavailable("no enrollment state".into()))
            }
        }

        fn confirm(&self, reason: &str) -> Result<(), VaultError> {
            let (tx, rx) = mpsc::channel::<bool>();
            // The reply runs on a LocalAuthentication queue, never this
            // thread, so blocking here cannot deadlock.
            let reply = RcBlock::new(move |ok: Bool, _err: *mut NSError| {
                let _ = tx.send(ok.as_bool());
            });
            // Kept alive until the reply arrives: a released context is
            // invalidated, which could end the evaluation early.
            // SAFETY: plain LocalAuthentication calls on a fresh context.
            let ctx = unsafe { LAContext::new() };
            // SAFETY: the block only sends on a channel (it is Send).
            unsafe {
                ctx.canEvaluatePolicy_error(POLICY)
                    .map_err(|e| unavailable(&e))?;
                ctx.setLocalizedFallbackTitle(Some(&NSString::from_str("Use Master Password")));
                ctx.evaluatePolicy_localizedReason_reply(
                    POLICY,
                    &NSString::from_str(reason),
                    &reply,
                );
            }
            let answer = rx.recv();
            drop(ctx);
            match answer {
                Ok(true) => Ok(()),
                _ => Err(VaultError::PresenceDenied),
            }
        }
    }
}

#[cfg(all(test, any(target_os = "macos", target_os = "ios")))]
mod biometric_tests {
    use objc2_local_authentication::LABiometryType;

    use super::GateKind;
    use super::biometric::kind_for;

    #[test]
    fn kind_follows_the_biometry_type() {
        assert_eq!(kind_for(LABiometryType::FaceID), GateKind::FaceId);
        assert_eq!(kind_for(LABiometryType::TouchID), GateKind::TouchId);
        assert_eq!(kind_for(LABiometryType::None), GateKind::None);
        // Vision Pro's Optic ID, and anything newer: unknown here.
        assert_eq!(kind_for(LABiometryType::OpticID), GateKind::None);
        assert_eq!(kind_for(LABiometryType(1 << 8)), GateKind::None);
    }
}

#[cfg(windows)]
mod windows_hello {
    //! Windows Hello through WinRT: `UserConsentVerifier` says whether Hello
    //! is set up, and a `KeyCredentialManager` key credential signs quick
    //! unlock's challenge (`docs/crypto.md`, "Quick unlock on Windows").
    //!
    //! The calls block on WinRT async operations (`join`). The core calls
    //! the gate from `spawn_blocking` threads, never the UI thread, so the
    //! window keeps pumping messages while the Hello dialog is up.
    //!
    //! Prompt ownership: `UserConsentVerifier` has an HWND-owned variant
    //! (`IUserConsentVerifierInterop`), used by [`confirm`]. Key credentials
    //! have none, and their dialog can open behind the app window, so every
    //! prompt runs inside a [`PromptFocus`]: it brings the owner window
    //! ([`set_prompt_owner`]) to the front, then watches for the Hello dialog
    //! and brings that to the front once it appears.
    //!
    //! [`confirm`]: PresenceGate::confirm

    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
    use std::thread::JoinHandle;
    use std::time::{Duration, Instant};

    use secrecy::SecretBox;
    use windows::Security::Credentials::UI::{
        UserConsentVerificationResult, UserConsentVerifier, UserConsentVerifierAvailability,
    };
    use windows::Security::Credentials::{
        KeyCredential, KeyCredentialCreationOption, KeyCredentialManager, KeyCredentialStatus,
    };
    use windows::Security::Cryptography::CryptographicBuffer;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::WinRT::{IBufferByteAccess, IUserConsentVerifierInterop};
    use windows::Win32::UI::WindowsAndMessaging::{
        FindWindowW, IsIconic, SW_RESTORE, SetForegroundWindow, ShowWindow,
    };
    use windows::core::{HSTRING, Interface, PCWSTR, w};
    use windows_future::IAsyncOperation;
    use zeroize::Zeroize;

    use super::{GateKind, PresenceGate, VaultError};

    /// The key credential's name; one per Windows user, shared by all vaults.
    pub const CREDENTIAL_NAME: &str = "dev.tether.client.quick-unlock";

    /// The window Hello prompts belong to (an `HWND`); 0 = none.
    static PROMPT_OWNER: AtomicIsize = AtomicIsize::new(0);

    /// Sets the window Windows Hello prompts open over (the app's main
    /// window, as an `HWND`); 0 clears it.
    pub fn set_prompt_owner(hwnd: isize) {
        PROMPT_OWNER.store(hwnd, Ordering::Relaxed);
    }

    fn prompt_owner() -> Option<HWND> {
        match PROMPT_OWNER.load(Ordering::Relaxed) {
            0 => None,
            h => Some(HWND(h as *mut core::ffi::c_void)),
        }
    }

    const NOT_SET_UP: &str = "Windows Hello is not set up on this PC.";
    const NO_PIN: &str = "Set up a Windows Hello PIN in Windows Settings to use quick unlock.";
    const BY_POLICY: &str = "Windows Hello is turned off by policy on this PC.";
    const BUSY: &str = "Windows Hello is busy. Try again.";
    const NOT_NOW: &str = "Windows Hello is not available right now.";

    fn unavailable(msg: &str) -> VaultError {
        VaultError::BiometryUnavailable(msg.into())
    }

    /// A failed WinRT call. Its HRESULT text carries no secret.
    fn winrt(what: &'static str) -> impl FnOnce(windows::core::Error) -> VaultError {
        move |e| VaultError::BiometryUnavailable(format!("Windows Hello could not {what}: {e}"))
    }

    fn status_name(status: KeyCredentialStatus) -> String {
        match status {
            KeyCredentialStatus::Success => "success".into(),
            KeyCredentialStatus::UnknownError => "unknown error".into(),
            KeyCredentialStatus::NotFound => "credential not found".into(),
            KeyCredentialStatus::UserCanceled => "cancelled".into(),
            KeyCredentialStatus::UserPrefersPassword => "user prefers password".into(),
            KeyCredentialStatus::CredentialAlreadyExists => "credential already exists".into(),
            KeyCredentialStatus::SecurityDeviceLocked => "security device locked".into(),
            other => format!("status {}", other.0),
        }
    }

    /// A key credential status other than success, for `what` ("sign").
    fn status_error(what: &str, status: KeyCredentialStatus) -> VaultError {
        match status {
            KeyCredentialStatus::UserCanceled | KeyCredentialStatus::UserPrefersPassword => {
                VaultError::PresenceDenied
            }
            other => VaultError::BiometryUnavailable(format!(
                "Windows Hello could not {what}: {}",
                status_name(other)
            )),
        }
    }

    /// Keeps a Hello prompt in front of the app while it runs: brings the
    /// owner window forward, then (on a helper thread) brings each Hello
    /// dialog that appears forward, until dropped.
    struct PromptFocus {
        done: Arc<AtomicBool>,
        watcher: Option<JoinHandle<()>>,
    }

    impl PromptFocus {
        /// Cap on the watch, should a prompt never return.
        const WATCH: Duration = Duration::from_secs(300);
        const POLL: Duration = Duration::from_millis(50);

        fn start() -> Self {
            if let Some(owner) = prompt_owner() {
                // SAFETY: plain user32 calls on a window handle; a stale or
                // destroyed handle only makes them fail.
                unsafe {
                    if IsIconic(owner).as_bool() {
                        let _ = ShowWindow(owner, SW_RESTORE);
                    }
                    let _ = SetForegroundWindow(owner);
                }
            }
            let done = Arc::new(AtomicBool::new(false));
            let stop = done.clone();
            let watcher = std::thread::Builder::new()
                .name("hello-focus".into())
                .spawn(move || {
                    let start = Instant::now();
                    // The dialog last raised, as an address (HWND is not
                    // Send); a new dialog (a retry, or the previous prompt's
                    // dialog still closing at first) is raised again.
                    let mut raised = 0usize;
                    while !stop.load(Ordering::Relaxed) && start.elapsed() < Self::WATCH {
                        // SAFETY: FindWindowW reads two NUL-terminated
                        // strings (a static literal and null);
                        // SetForegroundWindow only takes a handle.
                        unsafe {
                            if let Ok(dialog) = FindWindowW(
                                // The Windows Hello / credential dialog.
                                w!("Credential Dialog Xaml Host"),
                                PCWSTR::null(),
                            ) && !dialog.is_invalid()
                                && dialog.0 as usize != raised
                                && SetForegroundWindow(dialog).as_bool()
                            {
                                raised = dialog.0 as usize;
                            }
                        }
                        std::thread::sleep(Self::POLL);
                    }
                })
                .ok();
            Self { done, watcher }
        }
    }

    impl Drop for PromptFocus {
        fn drop(&mut self) {
            self.done.store(true, Ordering::Relaxed);
            if let Some(watcher) = self.watcher.take() {
                let _ = watcher.join();
            }
        }
    }

    /// Opens this user's quick unlock key credential. A missing one is
    /// created when `create` (enabling; creating is a Hello prompt), and is
    /// [`VaultError::BiometryChanged`] otherwise (unlocking: a removed or
    /// reset credential, which quick unlock cannot use any more).
    fn credential(create: bool) -> Result<KeyCredential, VaultError> {
        let name = HSTRING::from(CREDENTIAL_NAME);
        let open = || -> Result<_, VaultError> {
            KeyCredentialManager::OpenAsync(&name)
                .and_then(|op| op.join())
                .map_err(winrt("open its key"))
        };
        let opened = open()?;
        match opened.Status().map_err(winrt("open its key"))? {
            KeyCredentialStatus::Success => {
                return opened.Credential().map_err(winrt("open its key"));
            }
            KeyCredentialStatus::NotFound if create => {}
            KeyCredentialStatus::NotFound => return Err(VaultError::BiometryChanged),
            other => return Err(status_error("open its key", other)),
        }
        let created = {
            let _focus = PromptFocus::start();
            KeyCredentialManager::RequestCreateAsync(
                &name,
                KeyCredentialCreationOption::FailIfExists,
            )
            .and_then(|op| op.join())
            .map_err(winrt("create its key"))?
        };
        let result = match created.Status().map_err(winrt("create its key"))? {
            KeyCredentialStatus::Success => created,
            // Created by another Tether window in the meantime.
            KeyCredentialStatus::CredentialAlreadyExists => {
                let again = open()?;
                match again.Status().map_err(winrt("open its key"))? {
                    KeyCredentialStatus::Success => again,
                    other => return Err(status_error("open its key", other)),
                }
            }
            other => return Err(status_error("create its key", other)),
        };
        result.Credential().map_err(winrt("create its key"))
    }

    /// Has `credential` sign `challenge` (a Hello prompt) and derives the
    /// HelloKey from the signature, which is zeroized and never leaves here.
    fn sign_and_derive(
        credential: &KeyCredential,
        challenge: &[u8],
    ) -> Result<SecretBox<[u8; 32]>, VaultError> {
        let data = CryptographicBuffer::CreateFromByteArray(challenge).map_err(winrt("sign"))?;
        let signed = {
            let _focus = PromptFocus::start();
            credential
                .RequestSignAsync(&data)
                .and_then(|op| op.join())
                .map_err(winrt("sign"))?
        };
        match signed.Status().map_err(winrt("sign"))? {
            KeyCredentialStatus::Success => {}
            other => return Err(status_error("sign", other)),
        }
        let signature = signed.Result().map_err(winrt("sign"))?;
        let len = signature.Length().map_err(winrt("sign"))? as usize;
        let access: IBufferByteAccess = signature.cast().map_err(winrt("sign"))?;
        // SAFETY: IBufferByteAccess::Buffer returns the start of the
        // buffer's own storage, valid while `signature` is alive.
        let ptr = unsafe { access.Buffer() }.map_err(winrt("sign"))?;
        if len == 0 || ptr.is_null() {
            return Err(unavailable("Windows Hello could not sign: empty signature"));
        }
        // SAFETY: `ptr` points at `len` initialized bytes owned by
        // `signature`, which outlives this slice; nothing else touches the
        // buffer meanwhile (it was created for this call).
        let bytes = unsafe { std::slice::from_raw_parts_mut(ptr, len) };
        let key = core_crypto::hello_key(bytes);
        bytes.zeroize();
        Ok(key)
    }

    /// Windows Hello (PIN, fingerprint or face). A key-binding gate: quick
    /// unlock's wrapping key comes from a Hello signature, so the keychain
    /// holds nothing for it.
    #[derive(Debug, Default, Clone, Copy)]
    pub struct WindowsHelloGate;

    impl PresenceGate for WindowsHelloGate {
        fn kind(&self) -> GateKind {
            GateKind::WindowsHello
        }

        fn binds_keys(&self) -> bool {
            true
        }

        /// Hello has no enrollment hash to compare; a constant marks it
        /// available (a reset shows up as a different key instead).
        fn state(&self) -> Result<Vec<u8>, VaultError> {
            let availability =
                UserConsentVerifier::CheckAvailabilityAsync().and_then(|op| op.join());
            match availability {
                Ok(UserConsentVerifierAvailability::Available) => Ok(b"windows-hello".to_vec()),
                Ok(UserConsentVerifierAvailability::DeviceNotPresent) => {
                    Err(unavailable(NOT_SET_UP))
                }
                Ok(UserConsentVerifierAvailability::NotConfiguredForUser) => {
                    Err(unavailable(NO_PIN))
                }
                Ok(UserConsentVerifierAvailability::DisabledByPolicy) => {
                    Err(unavailable(BY_POLICY))
                }
                Ok(UserConsentVerifierAvailability::DeviceBusy) => Err(unavailable(BUSY)),
                Ok(_) => Err(unavailable(NOT_NOW)),
                Err(e) => {
                    tracing::debug!("Windows Hello availability check failed: {e}");
                    Err(unavailable(NOT_NOW))
                }
            }
        }

        /// A plain Hello check showing `reason`, owned by the prompt owner
        /// window when one is set. Quick unlock itself uses
        /// [`derive_key`](PresenceGate::derive_key).
        fn confirm(&self, reason: &str) -> Result<(), VaultError> {
            let message = HSTRING::from(reason);
            let answer = {
                let _focus = PromptFocus::start();
                let op: IAsyncOperation<UserConsentVerificationResult> = match prompt_owner() {
                    Some(owner) => {
                        let interop = windows::core::factory::<
                            UserConsentVerifier,
                            IUserConsentVerifierInterop,
                        >()
                        .map_err(winrt("ask"))?;
                        // SAFETY: `owner` is a window handle (a stale one
                        // makes the call fail); `message` outlives the call.
                        unsafe { interop.RequestVerificationForWindowAsync(owner, &message) }
                            .map_err(winrt("ask"))?
                    }
                    None => UserConsentVerifier::RequestVerificationAsync(&message)
                        .map_err(winrt("ask"))?,
                };
                op.join().map_err(winrt("ask"))?
            };
            match answer {
                UserConsentVerificationResult::Verified => Ok(()),
                UserConsentVerificationResult::Canceled
                | UserConsentVerificationResult::RetriesExhausted => {
                    Err(VaultError::PresenceDenied)
                }
                UserConsentVerificationResult::DeviceNotPresent => Err(unavailable(NOT_SET_UP)),
                UserConsentVerificationResult::NotConfiguredForUser => Err(unavailable(NO_PIN)),
                UserConsentVerificationResult::DisabledByPolicy => Err(unavailable(BY_POLICY)),
                UserConsentVerificationResult::DeviceBusy => Err(unavailable(BUSY)),
                _ => Err(unavailable(NOT_NOW)),
            }
        }

        /// Signs `challenge` with this user's Tether key credential (created
        /// when enabling, if missing) and derives the HelloKey from the
        /// signature. The Hello dialog shows its own text: key credentials
        /// take no `reason`.
        fn derive_key(
            &self,
            challenge: &[u8],
            _reason: &str,
            create: bool,
        ) -> Result<SecretBox<[u8; 32]>, VaultError> {
            let credential = credential(create)?;
            sign_and_derive(&credential, challenge)
        }
    }
}

/// Scripted [`PresenceGate`] for tests: a Touch ID gate ([`new`](Self::new))
/// or a Windows Hello-like key-binding gate ([`binding`](Self::binding))
/// whose state and answers are set by the test. Clones share state.
#[cfg(any(test, feature = "test-util"))]
#[derive(Clone)]
pub struct FakeGate {
    inner: std::sync::Arc<std::sync::Mutex<FakeState>>,
}

#[cfg(any(test, feature = "test-util"))]
struct FakeState {
    state: Result<Vec<u8>, String>,
    allow: bool,
    confirms: usize,
    /// Key-binding gates only: stands in for the platform credential.
    /// Test-only, but still never printed.
    seed: Option<Vec<u8>>,
    /// Key-binding gates only: the platform credential was removed.
    missing: bool,
    /// Overrides the reported kind ([`FakeGate::set_kind`]).
    kind: Option<GateKind>,
}

#[cfg(any(test, feature = "test-util"))]
impl FakeGate {
    /// Available, enrolled with `state`, and confirming.
    pub fn new(state: &[u8]) -> Self {
        Self {
            inner: std::sync::Arc::new(std::sync::Mutex::new(FakeState {
                state: Ok(state.to_vec()),
                allow: true,
                confirms: 0,
                seed: None,
                missing: false,
                kind: None,
            })),
        }
    }

    /// A key-binding gate (kind [`GateKind::WindowsHello`]): available,
    /// confirming, and deriving SHA-256(`seed` ‖ challenge).
    pub fn binding(seed: &[u8]) -> Self {
        let gate = Self::new(b"windows-hello");
        gate.lock().seed = Some(seed.to_vec());
        gate
    }

    /// Replaces the binding seed: simulates a reset or re-created Windows
    /// Hello credential, which signs the same challenge to a different key.
    pub fn set_seed(&self, seed: &[u8]) {
        self.lock().seed = Some(seed.to_vec());
    }

    /// Removes the platform credential (a Windows Hello reset that left
    /// none): unlocking reports it, enabling creates a new one, which signs
    /// to a different key.
    pub fn remove_credential(&self) {
        self.lock().missing = true;
    }

    /// Reports `kind` instead (an iOS gate names Face ID, Touch ID, or
    /// nothing for a sensor it does not know); the gate still checks.
    pub fn set_kind(&self, kind: GateKind) {
        self.lock().kind = Some(kind);
    }

    /// The enrollment state, or `Err` for "biometry unavailable".
    pub fn set_state(&self, state: Result<Vec<u8>, String>) {
        self.lock().state = state;
    }

    /// Whether later confirmations succeed.
    pub fn answer(&self, allow: bool) {
        self.lock().allow = allow;
    }

    /// How many times the user was asked (`confirm` and `derive_key`).
    pub fn confirms(&self) -> usize {
        self.lock().confirms
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, FakeState> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(any(test, feature = "test-util"))]
impl PresenceGate for FakeGate {
    fn kind(&self) -> GateKind {
        if let Some(kind) = self.lock().kind {
            kind
        } else if self.binds_keys() {
            GateKind::WindowsHello
        } else {
            GateKind::TouchId
        }
    }

    fn checks_presence(&self) -> bool {
        true
    }

    fn binds_keys(&self) -> bool {
        self.lock().seed.is_some()
    }

    fn state(&self) -> Result<Vec<u8>, VaultError> {
        self.lock()
            .state
            .clone()
            .map_err(VaultError::BiometryUnavailable)
    }

    fn confirm(&self, _reason: &str) -> Result<(), VaultError> {
        self.state()?;
        let mut s = self.lock();
        s.confirms += 1;
        if s.allow {
            Ok(())
        } else {
            Err(VaultError::PresenceDenied)
        }
    }

    fn derive_key(
        &self,
        challenge: &[u8],
        _reason: &str,
        create: bool,
    ) -> Result<SecretBox<[u8; 32]>, VaultError> {
        use sha2::{Digest, Sha256};

        self.state()?;
        let mut s = self.lock();
        let Some(seed) = s.seed.clone() else {
            return Err(VaultError::BiometryUnavailable(
                "this gate does not bind keys".into(),
            ));
        };
        if s.missing {
            if !create {
                return Err(VaultError::BiometryChanged);
            }
            // A new credential (asked for), which signs differently.
            s.confirms += 1;
            s.missing = false;
            s.seed = Some([seed.as_slice(), b"/new"].concat());
        }
        s.confirms += 1;
        if !s.allow {
            return Err(VaultError::PresenceDenied);
        }
        let mut h = Sha256::new();
        h.update(s.seed.as_deref().unwrap_or_default());
        h.update(challenge);
        Ok(SecretBox::new(Box::new(h.finalize().into())))
    }
}
