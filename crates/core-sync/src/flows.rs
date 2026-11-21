//! Account flows (spec "Client", "Password change and recovery"): turn sync
//! on for an existing vault, join a synced vault with an add-device code,
//! sign an existing device in again with the password, make an add-device
//! code, change the password, reset it with the recovery key (on this device
//! or a new one), and turn sync off.
//!
//! Every flow asks the server first and commits locally only after it
//! accepted, so a refusal never leaves this device and the server
//! disagreeing about the password. Argon2 and SQLite run on the blocking
//! pool; the vault lock is never held across a request.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use core_api::vault::{SharedVault, recovery_reset_for, validate_kdf};
use core_crypto::{KdfParams, derive_master, parse_recovery_key, recovery_auth_key};
use core_model::Id;
use core_proto::{
    AccountResp, CodeKind, CodeProof, CodeVerifyReq, DeviceInfo, ErrorCode, JoinCode, LoginReq,
    PasswordChangeReq, PreloginReq, RecoverReq, RecoveryResetReq, RegisterReq, ShortCode,
    TokenResp, client_proof, parse_address, verify_server_proof,
};
use secrecy::{ExposeSecret, SecretString};

use crate::client::{Client, FIRST_CONTACT_PATIENCE};
use crate::engine::SyncHandle;
use crate::error::SyncError;
use crate::fallback;
use crate::state::{self, SyncConfig, with_vault};
use crate::tls::{self, Pin};
use crate::tokens::TokenStore;

/// Account revision of a newly registered account (`docs/protocol.md`).
const FIRST_ACCOUNT_REV: i64 = 1;

/// A client for a server this device may never have contacted, once a
/// connection to it opens (see [`FIRST_CONTACT_PATIENCE`]).
async fn first_contact(url: &str, pin: Option<Pin>) -> Result<Client, SyncError> {
    let client = Client::new(url, pin)?;
    if !FIRST_CONTACT_PATIENCE.is_zero() {
        client.reach(FIRST_CONTACT_PATIENCE).await?;
    }
    Ok(client)
}

/// The recovery key step of turning sync on (spec "Existing vaults and the
/// recovery key").
pub enum RecoveryChoice {
    /// The user typed their existing recovery key.
    Existing(SecretString),
    /// "I lost it": make a new one (shown once, replaces the old one).
    New,
}

/// Outcome of [`enable_with_invite`].
pub struct EnableResult {
    /// The new recovery key's text, when one was made: show it once. Zeroized
    /// on drop, never printed by `Debug`.
    pub new_recovery_text: Option<SecretString>,
}

impl std::fmt::Debug for EnableResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnableResult")
            .field(
                "new_recovery_text",
                &self.new_recovery_text.as_ref().map(|_| "[REDACTED]"),
            )
            .finish()
    }
}

/// Why [`enable_with_invite`] failed. When the failure came after the
/// server registered the account with a new recovery key ("I lost it"), the
/// server (and normally this vault) already trust only that key, so its text
/// travels with the error: show it to the user like a success's. Zeroized on
/// drop, never printed by `Debug`.
pub struct EnableError {
    /// What went wrong.
    pub error: SyncError,
    /// The new recovery key's text, when the server already holds it.
    pub new_recovery_text: Option<SecretString>,
}

impl From<SyncError> for EnableError {
    fn from(error: SyncError) -> Self {
        EnableError {
            error,
            new_recovery_text: None,
        }
    }
}

impl PartialEq<SyncError> for EnableError {
    fn eq(&self, other: &SyncError) -> bool {
        self.error == *other
    }
}

impl std::fmt::Debug for EnableError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnableError")
            .field("error", &self.error)
            .field(
                "new_recovery_text",
                &self.new_recovery_text.as_ref().map(|_| "[REDACTED]"),
            )
            .finish()
    }
}

impl std::fmt::Display for EnableError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(f)
    }
}

impl std::error::Error for EnableError {}

/// Outcome of [`join_with_code`].
#[derive(Debug)]
pub struct JoinResult {
    /// Where the replaced vault was kept (`<file>.before-join-<date>`).
    pub backup: Option<PathBuf>,
    /// This device's own vault was reconnected: its records were kept.
    pub reconnected: bool,
}

/// Outcome of [`recover_on_new_device`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NewDeviceRecovery {
    /// The account revision after the reset.
    pub account_rev: i64,
}

/// What a device is called when its host name says nothing: set once by the
/// app on iOS ("iPhone" / "iPad"), where `gethostname` is generic.
static DEVICE_FALLBACK: OnceLock<String> = OnceLock::new();

/// Names this device for the device list when its host name is no use (iOS:
/// "iPhone" or "iPad"). The first call wins; later ones are ignored.
pub fn set_device_fallback(name: &str) {
    let _ = DEVICE_FALLBACK.set(name.to_owned());
}

/// Host names that name no particular device.
const GENERIC_HOSTNAMES: &[&str] = &[
    "localhost",
    "localhost.localdomain",
    "iphone",
    "ipad",
    "ios",
];

/// A device list name from a host name: `.local` dropped, control characters
/// removed, at most 100 characters; `fallback` when nothing useful is left
/// (empty, or a generic name like "localhost" or "iPhone").
pub fn device_name(hostname: &str, fallback: &str) -> String {
    let name: String = hostname
        .trim()
        .trim_end_matches(".local")
        .chars()
        .filter(|c| !c.is_control())
        .take(100)
        .collect();
    let name = name.trim();
    if name.is_empty() || GENERIC_HOSTNAMES.contains(&name.to_ascii_lowercase().as_str()) {
        fallback.to_owned()
    } else {
        name.to_owned()
    }
}

/// This computer's name and platform for the device list.
pub fn this_device() -> DeviceInfo {
    let fallback = DEVICE_FALLBACK.get().cloned();
    // Android's host name is generic; the phone model names it.
    #[cfg(target_os = "android")]
    let fallback = fallback.or_else(android_model);
    let fallback = fallback.unwrap_or_else(|| "Tether device".to_owned());
    DeviceInfo {
        name: device_name(&hostname().unwrap_or_default(), &fallback),
        platform: std::env::consts::OS.to_owned(),
    }
}

#[cfg(unix)]
fn hostname() -> Option<String> {
    let mut buf = [0u8; 256];
    // SAFETY: the buffer is valid for `buf.len()` bytes; gethostname writes
    // at most that many and we stop at the first NUL.
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) };
    if rc != 0 {
        return None;
    }
    let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    String::from_utf8(buf[..end].to_vec()).ok()
}

/// The phone's model (`ro.product.model`, e.g. "Pixel 8"); `None` when unset.
#[cfg(target_os = "android")]
fn android_model() -> Option<String> {
    let mut buf = [0u8; libc::PROP_VALUE_MAX as usize];
    // SAFETY: the key is NUL-terminated; the buffer is PROP_VALUE_MAX bytes,
    // the most the call writes (including its NUL).
    let len = unsafe {
        libc::__system_property_get(c"ro.product.model".as_ptr(), buf.as_mut_ptr().cast())
    };
    let bytes = buf[..usize::try_from(len).ok()?].to_vec();
    let model = String::from_utf8(bytes).ok()?;
    let model = model.trim();
    (!model.is_empty()).then(|| model.to_owned())
}

#[cfg(not(unix))]
fn hostname() -> Option<String> {
    std::env::var("COMPUTERNAME").ok()
}

fn device_id(resp: &TokenResp) -> Result<Id, SyncError> {
    resp.device_id
        .parse()
        .map_err(|_| SyncError::Protocol("the server sent a malformed device id".into()))
}

/// A sign-in refused for the key: the password (or recovery key) is wrong.
fn refused_as(wrong: SyncError) -> impl FnOnce(SyncError) -> SyncError {
    move |e| match e {
        SyncError::Unauthorized => wrong,
        e => e,
    }
}

/// The cheapest KDF cost `vault` accepts from a server
/// (`VaultService::kdf_floor`).
async fn kdf_floor(vault: &SharedVault) -> Result<KdfParams, SyncError> {
    with_vault(vault, |v| Ok(v.kdf_floor()?)).await
}

/// The AuthKey for `username` on the server `client` talks to: prelogin
/// serves the account's KDF parameters (hostile input, bounded before
/// deriving and refused below `floor`: [`SyncError::WeakKdf`]), Argon2 runs
/// on the blocking pool.
async fn server_auth_key(
    client: &Client,
    username: &str,
    password: &SecretString,
    floor: &KdfParams,
) -> Result<secrecy::SecretBox<[u8; 32]>, SyncError> {
    let pre = client
        .prelogin(&PreloginReq {
            username: username.to_owned(),
        })
        .await?;
    let kdf = validate_kdf(&pre.kdf, floor)?;
    let pw = SecretString::from(password.expose_secret().to_owned());
    Ok(
        tokio::task::spawn_blocking(move || derive_master(&pw, &kdf))
            .await
            .map_err(|_| SyncError::Vault("key derivation failed".into()))?
            .map_err(|e| SyncError::Vault(e.to_string()))?
            .auth_key,
    )
}

/// Turns sync on for this device's existing (unlocked) vault with an invite
/// code: checks the password and the recovery key locally, registers the
/// account with the vault's current key material (`nodeHint` = its HLC
/// node), then, in one local transaction, records the server, installs the
/// new recovery wrap (if any), queues every live record for upload and turns
/// sync on; the credentials are saved last. Nothing is written locally if the
/// server refuses. Start the engine ([`crate::start`]) afterwards: it uploads
/// with progress.
///
/// `tokens` is the primary credential store (the keychain); if it fails the
/// credentials go to the vault database instead. A failure after the server
/// registered a new recovery key returns that key with the error
/// ([`EnableError::new_recovery_text`]).
pub async fn enable_with_invite(
    vault: &SharedVault,
    tokens: Arc<dyn TokenStore>,
    code: &JoinCode,
    username: &str,
    password: SecretString,
    recovery: RecoveryChoice,
) -> Result<EnableResult, EnableError> {
    if code.kind != CodeKind::Invite {
        return Err(SyncError::InvalidCode(
            "this is an add-device code; it joins a synced vault instead".into(),
        )
        .into());
    }
    enable_at(
        vault,
        tokens,
        &code.url,
        code.pin.map(Pin::new),
        Credential::Token(code.token),
        username,
        password,
        recovery,
    )
    .await
}

/// A server at a typed address that proved it knows the short code (see
/// [`check_server`]). No `Debug`: it holds the proof's nonce.
pub struct ServerCheck {
    /// `https://host:port`.
    pub url: String,
    /// The pin to connect with; `None` when the certificate passed WebPKI.
    pub pin: Option<Pin>,
    client_nonce: [u8; 32],
}

/// The proof binding: the pin when pinned, else empty (spec "TLS").
fn binding(pin: &Option<Pin>) -> &[u8] {
    pin.as_ref().map_or(&[], |p| p.as_bytes())
}

/// Checks that the server at `address` (`host`, `host:port`, ...) knows
/// `code` before anything secret is sent to it: probes its certificate
/// ([`tls::probe`]), sends a fresh nonce to `/v1/code/verify` and checks the
/// server's proof. A wrong proof is [`SyncError::ServerUnverified`]; a server
/// without the endpoint is [`SyncError::UpdateServer`].
pub async fn check_server(address: &str, code: &ShortCode) -> Result<ServerCheck, SyncError> {
    let url = parse_address(address)?;
    let pin = tls::probe(&url).await?.pin;
    let client = first_contact(&url, pin).await?;
    let client_nonce = *core_crypto::keys::random_key().expose_secret();
    let resp = client
        .verify_code(&CodeVerifyReq {
            code_id: code.id().to_vec(),
            client_nonce: client_nonce.to_vec(),
            pinned: pin.is_some(),
        })
        .await?;
    if !verify_server_proof(
        &code.key(),
        binding(&pin),
        &client_nonce,
        &resp.server_proof,
    ) {
        return Err(SyncError::ServerUnverified);
    }
    Ok(ServerCheck {
        url,
        pin,
        client_nonce,
    })
}

/// [`enable_with_invite`] with a setup or invite short code and the server
/// address the user typed. The server must pass [`check_server`] first;
/// only then is the account registered (with the code's proof, never the
/// code).
pub async fn enable_with_short_code(
    vault: &SharedVault,
    tokens: Arc<dyn TokenStore>,
    address: &str,
    code: &ShortCode,
    username: &str,
    password: SecretString,
    recovery: RecoveryChoice,
) -> Result<EnableResult, EnableError> {
    let check = check_server(address, code).await?;
    let proof = CodeProof {
        code_id: code.id().to_vec(),
        client_nonce: check.client_nonce.to_vec(),
        pinned: check.pin.is_some(),
        proof: client_proof(&code.key(), binding(&check.pin), &check.client_nonce).to_vec(),
    };
    enable_at(
        vault,
        tokens,
        &check.url,
        check.pin,
        Credential::Proof(proof),
        username,
        password,
        recovery,
    )
    .await
}

/// What redeems the invite in `register`.
enum Credential {
    /// A full `tether1-` invite's token.
    Token([u8; 32]),
    /// A short code's proof.
    Proof(CodeProof),
}

#[allow(clippy::too_many_arguments)]
async fn enable_at(
    vault: &SharedVault,
    tokens: Arc<dyn TokenStore>,
    url: &str,
    pin: Option<Pin>,
    credential: Credential,
    username: &str,
    password: SecretString,
    recovery: RecoveryChoice,
) -> Result<EnableResult, EnableError> {
    let username = username.trim().to_owned();
    let prep = with_vault(vault, move |v| {
        if !v.is_unlocked() {
            return Err(SyncError::Locked);
        }
        if state::is_enabled(v)? {
            return Err(SyncError::Vault("sync is already on".into()));
        }
        let auth = v.auth_key(&password)?;
        let (recovery_auth, new) = match recovery {
            RecoveryChoice::Existing(text) => (v.recovery_auth(text.expose_secret())?, None),
            RecoveryChoice::New => {
                let n = v.new_recovery()?;
                let auth = secrecy::SecretBox::new(Box::new(*n.auth.expose_secret()));
                (auth, Some(n))
            }
        };
        let mut material = v.account_material()?;
        if let Some(n) = &new {
            material.secret_by_recovery = n.wrap.clone();
        }
        Ok((auth, recovery_auth, new, material, v.node()?))
    })
    .await?;
    let (auth, recovery_auth, new, material, node) = prep;

    let (invite_token, code_proof) = match credential {
        Credential::Token(token) => (token.to_vec(), None),
        Credential::Proof(proof) => (Vec::new(), Some(proof)),
    };
    let client = first_contact(url, pin).await?;
    let resp = client
        .register(&RegisterReq {
            invite_token,
            username: username.clone(),
            auth_key: auth.expose_secret().to_vec(),
            device: this_device(),
            material,
            recovery_auth_key: recovery_auth.expose_secret().to_vec(),
            node_hint: node,
            code_proof,
        })
        .await?;

    // From here on the server holds the new recovery key (if one was made):
    // whatever fails, its text goes back to the caller.
    let new_text = new
        .as_ref()
        .map(|n| SecretString::from(n.text.expose_secret().to_owned()));
    let with_key = |error: SyncError| EnableError {
        error,
        new_recovery_text: new_text
            .as_ref()
            .map(|t| SecretString::from(t.expose_secret().to_owned())),
    };
    // Held in memory until the vault is ready for the fallback store.
    client.adopt(&resp).await.map_err(with_key)?;
    let cfg = SyncConfig {
        url: client.base_url().to_owned(),
        pin,
        username,
        device_id: device_id(&resp).map_err(with_key)?,
    };
    let (node, epoch) = (resp.node, resp.epoch.clone());
    with_vault(vault, move |v| {
        // A new server: every live record goes up as new.
        let kv = state::enable_state(&cfg, &epoch, FIRST_ACCOUNT_REV);
        let kv: Vec<(&str, &[u8])> = kv.iter().map(|(k, v)| (*k, v.as_slice())).collect();
        Ok(v.commit_enable(new.as_ref(), node, &kv)?)
    })
    .await
    .map_err(with_key)?;
    fallback::save_credentials(&client, vault, tokens)
        .await
        .map_err(with_key)?;
    Ok(EnableResult {
        new_recovery_text: new_text,
    })
}

/// An add-device code redeemed with the password: this device is signed in
/// to the account as a new device and holds the account's key material, but
/// nothing is written locally yet. [`finish_join`] installs it;
/// [`PendingJoin::abandon`] revokes the new device instead. `Debug` shows no
/// session data.
pub struct PendingJoin {
    client: Client,
    cfg: SyncConfig,
    node: u16,
    epoch: String,
    account: AccountResp,
    /// The code's link token, to recognize the same code again.
    token: [u8; 32],
}

impl std::fmt::Debug for PendingJoin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingJoin")
            .field("cfg", &self.cfg)
            .finish_non_exhaustive()
    }
}

impl PendingJoin {
    /// Whether `code` is the code this sign-in redeemed (it is spent, so a
    /// second attempt with it must reuse this sign-in).
    pub fn is_for(&self, code: &JoinCode) -> bool {
        self.token == code.token
    }

    /// Gives the sign-in up: the new device is revoked on the server (best
    /// effort), so a refused join leaves nothing behind in the device list.
    pub async fn abandon(self) {
        if let Err(e) = self.client.revoke_device(self.cfg.device_id).await {
            tracing::info!(error = %e, "could not revoke the abandoned join's device");
        }
    }
}

/// Redeems an add-device code: signs in with the password and the code's
/// link token (the server adds a new device), then fetches the account
/// material. Writes nothing locally; continue with [`finish_join`]. An
/// invite code is refused ([`SyncError::InvalidCode`]), a wrong password is
/// [`SyncError::WrongPassword`], KDF parameters cheaper than `vault` accepts
/// are [`SyncError::WeakKdf`] (nothing sent).
pub async fn redeem_code(
    vault: &SharedVault,
    code: &JoinCode,
    username: &str,
    password: &SecretString,
) -> Result<PendingJoin, SyncError> {
    if code.kind != CodeKind::Link {
        return Err(SyncError::InvalidCode(
            "this is an invite code; joining a synced vault needs an add-device code".into(),
        ));
    }
    let username = username.trim().to_owned();
    let pin = code.pin.map(Pin::new);
    let floor = kdf_floor(vault).await?;
    let client = first_contact(&code.url, pin).await?;
    let auth = server_auth_key(&client, &username, password, &floor).await?;
    let resp = client
        .login(&LoginReq {
            username: username.clone(),
            auth_key: auth.expose_secret().to_vec(),
            device: this_device(),
            link_token: Some(code.token.to_vec()),
        })
        .await
        .map_err(refused_as(SyncError::WrongPassword))?;
    // The device secret comes once: held by the client (in memory) at once.
    client.adopt(&resp).await?;
    let account = client.account().await?;
    let cfg = SyncConfig {
        url: client.base_url().to_owned(),
        pin,
        username,
        device_id: device_id(&resp)?,
    };
    Ok(PendingJoin {
        client,
        cfg,
        node: resp.node,
        epoch: resp.epoch,
        account,
        token: code.token,
    })
}

/// How [`finish_join`] treats the local vault.
enum JoinKind {
    /// No vault yet: create it from the account.
    New,
    /// This device's own vault (same vault id): keep its records, resync.
    Reconnect,
    /// Another vault: replace it (a backup is kept).
    Replace,
}

/// Installs a redeemed code ([`redeem_code`]) locally and turns sync on:
///
/// * no vault on this device: the vault is created from the account,
///   unlocked; records arrive when the engine starts;
/// * this device's vault is the synced one (same vault id: sync was turned
///   off, the device was revoked or lost its sign-in): every local record is
///   kept, the account's current material is installed and a full resync
///   starts (local edits the server lacks are pushed);
/// * another vault: [`SyncError::VaultExists`] unless `replace_existing`
///   (after the user confirmed the spec's warning); the old vault is then
///   kept as `<file>.before-join-<date>` and replaced.
///
/// Nothing is written when it fails, so the same `pending` can be finished
/// again (with `replace_existing` after the warning). Stop the vault's
/// engine first.
pub async fn finish_join(
    vault: &SharedVault,
    tokens: Arc<dyn TokenStore>,
    pending: &PendingJoin,
    password: SecretString,
    replace_existing: bool,
) -> Result<JoinResult, SyncError> {
    let (cfg, node, epoch) = (pending.cfg.clone(), pending.node, pending.epoch.clone());
    let account = pending.account.clone();
    let result = with_vault(vault, move |v| {
        let kind = match v.vault_id() {
            None => JoinKind::New,
            Some(id) if id.to_string() == account.material.vault_id => JoinKind::Reconnect,
            Some(_) if replace_existing => JoinKind::Replace,
            Some(_) => return Err(SyncError::VaultExists),
        };
        let mut result = JoinResult {
            backup: None,
            reconnected: false,
        };
        match kind {
            JoinKind::Reconnect => {
                let kv = state::rejoin_state(&cfg, account.account_rev);
                let kv: Vec<(&str, &[u8])> = kv.iter().map(|(k, v)| (*k, v.as_slice())).collect();
                v.rejoin(&account.material, password, node, &kv)?;
                result.reconnected = true;
                return Ok(result);
            }
            JoinKind::Replace => {
                result.backup = Some(v.replace_with_join(&account.material, password, node)?);
            }
            JoinKind::New => v.join(&account.material, password, node)?,
        }
        v.sync_clear()?;
        cfg.save(v)?;
        state::init(v, &epoch, account.account_rev)?;
        Ok(result)
    })
    .await?;
    fallback::save_credentials(&pending.client, vault, tokens).await?;
    with_vault(vault, state::set_enabled).await?;
    Ok(result)
}

/// Joins the synced vault of `username` with an add-device code in one go:
/// [`redeem_code`], then [`finish_join`]. When the join is refused because
/// another vault would be replaced, the new device is revoked again
/// ([`PendingJoin::abandon`]) and the code is spent: the app instead keeps
/// the [`PendingJoin`] while it asks the user.
pub async fn join_with_code(
    vault: &SharedVault,
    tokens: Arc<dyn TokenStore>,
    code: &JoinCode,
    username: &str,
    password: SecretString,
    replace_existing: bool,
) -> Result<JoinResult, SyncError> {
    let pending = redeem_code(vault, code, username, &password).await?;
    match finish_join(vault, tokens, &pending, password, replace_existing).await {
        Err(SyncError::VaultExists) => {
            pending.abandon().await;
            Err(SyncError::VaultExists)
        }
        other => other,
    }
}

/// Signs this device back in with the master password when its session is
/// gone (sync stopped with "sign in again": the server was restored from a
/// backup older than the device's refresh token, or the session was ended).
/// Prelogin gives the account's KDF parameters (bounded, and no cheaper than
/// the ones this vault stores: [`SyncError::WeakKdf`]), the AuthKey is
/// derived, and `login` presents this device's id and stored device secret
/// (R12). The engine adopts the new session and syncs at once; a restored
/// server's new epoch then starts a full resync that keeps local edits. No
/// local record is touched and no new device is created.
///
/// A wrong password is [`SyncError::WrongPassword`]; a revoked device is
/// [`SyncError::Revoked`]; missing stored credentials, or a server that no
/// longer knows this device, is [`SyncError::NeedsDeviceCode`].
pub async fn sign_in_again(
    vault: &SharedVault,
    handle: &SyncHandle,
    password: SecretString,
) -> Result<(), SyncError> {
    let (cfg, floor) = with_vault(vault, |v| {
        let cfg = SyncConfig::load(v)?.ok_or_else(|| SyncError::Vault("sync is off".into()))?;
        Ok((cfg, v.kdf_floor()?))
    })
    .await?;
    let client = handle.client();
    // The id and secret prove which device this is; without the secret the
    // password alone cannot sign it in.
    if !client.resume(cfg.device_id).await? {
        return Err(SyncError::NeedsDeviceCode);
    }
    let auth = server_auth_key(client, &cfg.username, &password, &floor).await?;
    let resp = client
        .login(&LoginReq {
            username: cfg.username.clone(),
            auth_key: auth.expose_secret().to_vec(),
            device: this_device(),
            link_token: None,
        })
        .await
        .map_err(|e| match e {
            SyncError::Unauthorized => SyncError::WrongPassword,
            // The server does not know this device (it was added after the
            // backup the server was restored from). A disabled account is
            // also `forbidden` and keeps its own message.
            SyncError::Server {
                code: ErrorCode::Forbidden,
                message,
            } if !message.contains("disabled") => SyncError::NeedsDeviceCode,
            e => e,
        })?;
    if device_id(&resp)? != cfg.device_id {
        return Err(SyncError::Protocol(
            "the server signed in a different device".into(),
        ));
    }
    client.adopt(&resp).await?;
    // Clears the "sign in again" halt and syncs.
    handle.sync_now();
    Ok(())
}

/// An add-device code for another device, the server URL it carries, and
/// when it expires (Unix ms). The server's code is rewritten with this
/// device's own server URL and pin (the server may only know a placeholder
/// address), or with `address` (`host[:port]`) when other devices reach the
/// server by another name.
pub async fn create_link_code(
    handle: &SyncHandle,
    address: Option<&str>,
) -> Result<(String, String, i64), SyncError> {
    let client = handle.client();
    let link = client.create_link().await?;
    let mut code = JoinCode::parse(&link.code)?;
    code.url = match address {
        Some(a) => parse_address(a)?,
        None => client.base_url().to_owned(),
    };
    code.pin = client.pin().map(|p| *p.as_bytes());
    Ok((code.encode(), code.url, link.expires_at))
}

/// Changes the master password with sync on: the server takes the new KDF
/// parameters, wrap and verifier first (based on the stored `accountRev`),
/// then the local vault. Other devices keep their sessions and pick up the
/// change. If the account changed meanwhile, the current material is
/// installed and the user is asked to try again.
pub async fn change_password_synced(
    vault: &SharedVault,
    handle: &SyncHandle,
    old: SecretString,
    new: SecretString,
) -> Result<(), SyncError> {
    let _held = handle.gate().lock().await;
    let (change, base) = with_vault(vault, move |v| {
        let change = v.prepare_password_change(&old, &new)?;
        let base = v
            .account_rev()?
            .ok_or_else(|| SyncError::Vault("sync state lacks the account revision".into()))?;
        Ok((change, base))
    })
    .await?;
    let req = PasswordChangeReq {
        old_auth_key: change.old_auth.expose_secret().to_vec(),
        new_auth_key: change.new_auth.expose_secret().to_vec(),
        kdf: change.material.kdf.clone(),
        secret_by_kek: change.material.secret_by_kek.clone(),
        account_rev_base: base,
    };
    match handle.client().change_password(&req).await {
        Ok(resp) => {
            with_vault(vault, move |v| {
                Ok(v.commit_password_change(&change, Some(resp.account_rev))?)
            })
            .await
        }
        Err(SyncError::Server {
            code: ErrorCode::Conflict,
            ..
        }) => {
            let current = handle.client().account().await?;
            with_vault(vault, move |v| {
                Ok(v.install_account_material(&current.material, current.account_rev)?)
            })
            .await?;
            Err(SyncError::Server {
                code: ErrorCode::Conflict,
                message: "the password was changed on another device meanwhile; unlock with it \
                          and try again"
                    .into(),
            })
        }
        Err(e) => Err(e),
    }
}

/// Resets the master password with the recovery key on a device whose vault
/// syncs: an account-wide change (`/v1/account/recover`), then installed
/// locally and unlocked. With sync off the app keeps calling
/// `VaultService::unlock_with_recovery`.
pub async fn recover_local_synced(
    vault: &SharedVault,
    handle: &SyncHandle,
    recovery: &str,
    new_password: SecretString,
) -> Result<(), SyncError> {
    let _held = handle.gate().lock().await;
    let text = recovery.to_owned();
    let (cfg, recovery_auth) = with_vault(vault, move |v| {
        let cfg = SyncConfig::load(v)?.ok_or_else(|| SyncError::Vault("sync is off".into()))?;
        Ok((cfg, v.recovery_auth(&text)?))
    })
    .await?;
    // The limited token gets a client of its own: it has no device and no
    // refresh, and must not replace this device's session.
    let temp = Client::new(&cfg.url, cfg.pin)?;
    let resp = temp
        .recover_login(&RecoverReq {
            username: cfg.username.clone(),
            recovery_auth_key: recovery_auth.expose_secret().to_vec(),
            device: this_device(),
        })
        .await
        .map_err(refused_as(SyncError::WrongRecoveryKey))?;
    temp.adopt(&resp).await?;
    let current = temp.account().await?;

    let text = recovery.to_owned();
    let reset = with_vault(vault, move |v| {
        // Catch up with the server first (a stale local wrap), then compute
        // the reset from its material.
        v.install_account_material(&current.material, current.account_rev)?;
        Ok(v.prepare_recovery_reset(&current.material, &text, &new_password)?)
    })
    .await?;
    let done = temp
        .recovery_reset(&RecoveryResetReq {
            recovery_auth_key: reset.recovery_auth.expose_secret().to_vec(),
            new_auth_key: reset.new_auth.expose_secret().to_vec(),
            kdf: reset.kdf.clone(),
            secret_by_kek: reset.secret_by_kek.clone(),
        })
        .await
        .map_err(refused_as(SyncError::WrongRecoveryKey))?;
    with_vault(vault, move |v| {
        Ok(v.commit_recovery_reset(&reset, done.account_rev)?)
    })
    .await
}

/// "Forgot password" on a device without the vault: with the server, the
/// username and the recovery key, sets a new master password for the
/// account (the KDF cost stays the account's, with a fresh salt; a cost
/// cheaper than `vault` accepts, or than the production default when no
/// vault could be opened, is [`SyncError::WeakKdf`]). Writes nothing
/// locally.
///
/// The recovery sign-in creates no device (ruling R24), so the next step is
/// an add-device code from another device, or from the server admin
/// (`termlyx-server link USER`) for a lone lost device, then
/// [`join_with_code`] with the new password.
pub async fn recover_on_new_device(
    vault: Option<&SharedVault>,
    url: &str,
    pin: Option<Pin>,
    username: &str,
    recovery: &str,
    new_password: SecretString,
) -> Result<NewDeviceRecovery, SyncError> {
    let key = parse_recovery_key(recovery).map_err(|_| SyncError::WrongRecoveryKey)?;
    let auth = recovery_auth_key(key.expose_secret());
    let floor = match vault {
        Some(vault) => kdf_floor(vault).await?,
        None => KdfParams::new_random(),
    };
    let client = first_contact(url, pin).await?;
    let resp = client
        .recover_login(&RecoverReq {
            username: username.trim().to_owned(),
            recovery_auth_key: auth.expose_secret().to_vec(),
            device: this_device(),
        })
        .await
        .map_err(refused_as(SyncError::WrongRecoveryKey))?;
    client.adopt(&resp).await?;
    let current = client.account().await?;
    let cost = validate_kdf(&current.material.kdf, &floor)?;
    let kdf = KdfParams {
        m_kib: cost.m_kib,
        t: cost.t,
        p: cost.p,
        ..KdfParams::new_random()
    };
    let text = recovery.to_owned();
    let material = current.material.clone();
    let reset = tokio::task::spawn_blocking(move || {
        recovery_reset_for(&material, &text, &new_password, kdf)
    })
    .await
    .map_err(|_| SyncError::Vault("key derivation failed".into()))??;
    let done = client
        .recovery_reset(&RecoveryResetReq {
            recovery_auth_key: reset.recovery_auth.expose_secret().to_vec(),
            new_auth_key: reset.new_auth.expose_secret().to_vec(),
            kdf: reset.kdf.clone(),
            secret_by_kek: reset.secret_by_kek.clone(),
        })
        .await
        .map_err(refused_as(SyncError::WrongRecoveryKey))?;
    Ok(NewDeviceRecovery {
        account_rev: done.account_rev,
    })
}

/// After a local unlock failed with the wrong password while sync is on
/// (spec "Stale device"): fetches the current account material with the
/// device's session, installs it if newer, and retries the unlock once.
/// Any failure ends as [`SyncError::WrongPassword`] unless the vault itself
/// failed.
pub async fn refresh_account_then_retry(
    vault: &SharedVault,
    handle: &SyncHandle,
    password: SecretString,
) -> Result<(), SyncError> {
    {
        let _held = handle.gate().lock().await;
        match handle.client().account().await {
            Ok(current) => {
                with_vault(vault, move |v| {
                    Ok(v.install_account_material(&current.material, current.account_rev)?)
                })
                .await?;
            }
            Err(e) => tracing::debug!(error = %e, "could not refresh the account material"),
        }
    }
    with_vault(vault, move |v| Ok(v.unlock(password)?)).await
}

/// Turns sync off on this device: stops the engine, ends the session on
/// the server (best effort: works offline), forgets the credentials and
/// clears the sync state. Every record stays.
pub async fn turn_off(
    vault: &SharedVault,
    handle: Option<SyncHandle>,
    tokens: Arc<dyn TokenStore>,
) -> Result<(), SyncError> {
    let (cfg, on_fallback) = with_vault(vault, |v| {
        Ok((SyncConfig::load(v)?, fallback::uses_fallback(v)?))
    })
    .await?;
    let client = match handle {
        Some(h) => {
            let client = h.client().clone();
            h.stop().await;
            Some(client)
        }
        None => match &cfg {
            Some(cfg) => {
                let primary = tokens.clone();
                let vault2 = vault.clone();
                let store = with_vault(vault, move |v| {
                    fallback::credential_store(v, &vault2, primary)
                })
                .await?;
                let client = Client::new(&cfg.url, cfg.pin)?.with_store(store);
                client.resume(cfg.device_id).await.ok();
                Some(client)
            }
            None => None,
        },
    };
    if let Some(client) = &client
        && let Err(e) = client.logout().await
    {
        tracing::info!(error = %e, "logout failed; turning sync off locally anyway");
    }
    // Sync is off from here on, whatever the credential stores do. This also
    // removes credentials kept in the fallback (they live in `sync_state`).
    with_vault(vault, |v| Ok(v.sync_clear()?)).await?;
    // Best effort: forget the credentials in the store that held them.
    if let Some(client) = &client
        && let Err(e) = client.forget().await
    {
        tracing::warn!(error = %e, "could not delete the stored sync credentials");
    }
    if let (Some(cfg), false) = (cfg, on_fallback) {
        let device = cfg.device_id;
        match tokio::task::spawn_blocking(move || tokens.delete(device)).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!(error = %e, "could not delete the keychain entry"),
            Err(_) => tracing::warn!("credential store task failed"),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::device_name;

    #[test]
    fn a_real_host_name_is_the_device_name() {
        assert_eq!(device_name("alices-mbp.local", "Tether device"), "alices-mbp");
        assert_eq!(device_name("  work-pc ", "Tether device"), "work-pc");
        assert_eq!(device_name("Alice\u{7}'s iPhone", "iPhone"), "Alice's iPhone");
    }

    #[test]
    fn a_generic_or_empty_host_name_gives_the_fallback() {
        for generic in [
            "",
            "  ",
            ".local",
            "localhost",
            "LocalHost",
            "localhost.localdomain",
            "iPhone",
            "iphone.local",
            "iPad",
            "iOS",
        ] {
            assert_eq!(device_name(generic, "iPad"), "iPad", "{generic:?}");
        }
        assert_eq!(device_name("localhost", "Tether device"), "Tether device");
    }

    #[test]
    fn a_long_host_name_is_cut_to_100_characters() {
        assert_eq!(device_name(&"a".repeat(300), "x").chars().count(), 100);
    }
}
