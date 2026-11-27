//! Accounts: usernames, key verifiers, invites, prelogin and registration.
//!
//! The server never sees a password. It stores `Argon2id(AuthKey, salt)` for
//! the account's AuthKey and for its recovery auth key (both uniformly random
//! 32-byte values derived on the client), the KDF parameters the client used
//! (served by prelogin) and the account's wrapped key material, opaque to it.

use std::time::Duration;

use argon2::{Algorithm, Argon2, Params, Version};
use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::routing::post;
use core_proto::{
    AccountMaterial, CodeVerifyReq, CodeVerifyResp, DeviceInfo, ErrorCode, HEADER_DEVICE, KdfWire,
    PreloginReq, PreloginResp, RegisterReq, TokenResp,
};
use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use subtle::ConstantTimeEq;
use tokio::sync::{Semaphore, SemaphorePermit};

use crate::error::{ApiError, ApiResult};
use crate::limits::ClientIp;
use crate::state::AppState;
use crate::util::random_bytes;
use crate::{auth, db, devices};

/// Memory cost of the verifier hash: 19 MiB.
const VERIFIER_M_KIB: u32 = 19 * 1024;
/// Passes of the verifier hash.
const VERIFIER_T: u32 = 2;
/// Parallelism of the verifier hash.
const VERIFIER_P: u32 = 1;
/// Bytes of an AuthKey / recovery auth key and of a verifier.
const KEY_LEN: usize = 32;
/// Bytes of a verifier salt (and of a client KDF salt).
const SALT_LEN: usize = 16;

// The client's production KDF defaults (`core_crypto::KdfParams`: 64 MiB,
// 3 passes, 4 lanes). Copied, not imported: the server does not depend on
// core-crypto. Prelogin answers unknown usernames with these so a made-up
// account looks like every real one.
const FAKE_M_KIB: u32 = 65536;
const FAKE_T: u32 = 3;
const FAKE_P: u32 = 4;

/// Bounds on KDF parameters a client may register (the same bounds clients
/// enforce on what the server sends back: m <= 1 GiB, t 1..=16, p 1..=64).
const MAX_M_KIB: u32 = 1024 * 1024;
const MAX_T: u32 = 16;
const MAX_P: u32 = 64;

/// Wrapped key sizes from `docs/crypto.md` ("Local account record").
const ACCOUNT_PUBLIC_LEN: usize = 32;
const WRAPPED_SECRET_LEN: usize = 73;
const SEALED_VAULT_KEY_LEN: usize = 81;

/// A stand-in salt for the dummy verification that unknown users get.
const DUMMY_SALT: [u8; SALT_LEN] = [0x5a; SALT_LEN];

/// Trim, lowercase and validate a username: 3 to 64 characters of
/// `[a-z0-9._-]`.
pub fn normalize_username(raw: &str) -> ApiResult<String> {
    let name = raw.trim().to_ascii_lowercase();
    let ok = (3..=64).contains(&name.len())
        && name.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        });
    if ok {
        Ok(name)
    } else {
        Err(ApiError::bad_request(
            "usernames are 3 to 64 characters: letters, digits, '.', '_' and '-'",
        ))
    }
}

/// Validate and trim a device description: a name of 1 to 100 characters and a
/// platform of 1 to 40, no control characters.
pub fn validate_device(d: &DeviceInfo) -> ApiResult<(String, String)> {
    let name = validate_label("device name", &d.name, 100)?;
    let platform = validate_label("platform", &d.platform, 40)?;
    Ok((name, platform))
}

/// Trim `text` and check it is 1..=`max` characters without control characters.
pub fn validate_label(what: &str, text: &str, max: usize) -> ApiResult<String> {
    let t = text.trim();
    let n = t.chars().count();
    if n == 0 || n > max || t.chars().any(char::is_control) {
        return Err(ApiError::bad_request(format!(
            "{what} must be 1 to {max} characters"
        )));
    }
    Ok(t.to_owned())
}

/// `bytes` as a fixed-size array, or `bad_request` naming `what`.
pub fn fixed<const N: usize>(what: &str, bytes: &[u8]) -> ApiResult<[u8; N]> {
    bytes
        .try_into()
        .map_err(|_| ApiError::bad_request(format!("{what} must be {N} bytes")))
}

/// Check client-chosen KDF parameters are inside the bounds clients enforce on
/// what the server sends back: `m` 1..=1 GiB (and at least `8p`), `t` 1..=16,
/// `p` 1..=64, a 16-byte salt.
pub fn validate_kdf(k: &KdfWire) -> ApiResult<()> {
    if !(1..=MAX_M_KIB).contains(&k.m_kib)
        || !(1..=MAX_T).contains(&k.t)
        || !(1..=MAX_P).contains(&k.p)
        || k.m_kib < 8 * k.p
        || k.salt.len() != SALT_LEN
    {
        return Err(ApiError::bad_request("kdf parameters are out of range"));
    }
    Ok(())
}

/// Check a wrapped account secret (`secretByKek`, `secretByRecovery`) has the
/// documented size.
pub fn validate_wrapped_secret(what: &str, bytes: &[u8]) -> ApiResult<()> {
    if bytes.len() != WRAPPED_SECRET_LEN {
        return Err(ApiError::bad_request(format!(
            "{what} must be {WRAPPED_SECRET_LEN} bytes"
        )));
    }
    Ok(())
}

/// Check the account material is plausible: sizes from `docs/crypto.md`, a
/// canonical vault id, KDF parameters inside the bounds clients enforce.
pub fn validate_material(m: &AccountMaterial) -> ApiResult<()> {
    let canonical = uuid::Uuid::parse_str(&m.vault_id)
        .ok()
        .filter(|u| u.hyphenated().to_string() == m.vault_id);
    if canonical.is_none() {
        return Err(ApiError::bad_request(
            "vaultId must be a lowercase hyphenated UUID",
        ));
    }
    if m.key_version == 0 {
        return Err(ApiError::bad_request("keyVersion must be at least 1"));
    }
    validate_kdf(&m.kdf)?;
    for (what, bytes, len) in [
        ("accountPublic", &m.account_public, ACCOUNT_PUBLIC_LEN),
        ("secretByKek", &m.secret_by_kek, WRAPPED_SECRET_LEN),
        (
            "secretByRecovery",
            &m.secret_by_recovery,
            WRAPPED_SECRET_LEN,
        ),
        ("sealedVaultKey", &m.sealed_vault_key, SEALED_VAULT_KEY_LEN),
    ] {
        if bytes.len() != len {
            return Err(ApiError::bad_request(format!("{what} must be {len} bytes")));
        }
    }
    Ok(())
}

// ------------------------------------------------------------- verifiers

fn verifier_argon() -> Argon2<'static> {
    let params = Params::new(VERIFIER_M_KIB, VERIFIER_T, VERIFIER_P, Some(KEY_LEN))
        .expect("static verifier parameters are valid");
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
}

/// `Argon2id(key, salt)` with the verifier parameters (CPU and memory heavy:
/// call from a blocking thread).
fn hash_blocking(key: &[u8], salt: &[u8]) -> ApiResult<[u8; KEY_LEN]> {
    let mut out = [0u8; KEY_LEN];
    verifier_argon()
        .hash_password_into(key, salt, &mut out)
        .map_err(ApiError::internal)?;
    Ok(out)
}

/// At most this many Argon2 runs (19 MiB each) at once: unauthenticated
/// sign-ins must not be able to exhaust the memory of a small host.
const MAX_CONCURRENT_HASHES: usize = 4;
/// How long a request waits for a free slot before answering `rate_limited`.
const HASH_WAIT: Duration = Duration::from_secs(3);
static HASH_SLOTS: Semaphore = Semaphore::const_new(MAX_CONCURRENT_HASHES);

/// Take a slot from `slots`, or fail fast with `rate_limited` after `wait`.
async fn take_slot(slots: &Semaphore, wait: Duration) -> ApiResult<SemaphorePermit<'_>> {
    match tokio::time::timeout(wait, slots.acquire()).await {
        Ok(Ok(permit)) => Ok(permit),
        _ => Err(ApiError::new(ErrorCode::RateLimited, "server busy; try again in a moment")),
    }
}

/// `hash_blocking` on a blocking thread, bounded by `HASH_SLOTS`. The slot is
/// held until the hash finishes, even if the client has gone away.
async fn hash_bounded(key: Vec<u8>, salt: Vec<u8>) -> ApiResult<[u8; KEY_LEN]> {
    let permit = take_slot(&HASH_SLOTS, HASH_WAIT).await?;
    let hash = tokio::task::spawn_blocking(move || {
        let out = hash_blocking(&key, &salt);
        drop(permit);
        out
    })
    .await
    .map_err(ApiError::internal)??;
    Ok(hash)
}

/// Hash `key` with a fresh random salt; returns `(hash, salt)`.
pub async fn new_verifier(key: Vec<u8>) -> ApiResult<(Vec<u8>, Vec<u8>)> {
    let salt = random_bytes::<SALT_LEN>();
    let hash = hash_bounded(key, salt.to_vec()).await?;
    Ok((hash.to_vec(), salt.to_vec()))
}

/// Does `key` match the stored `(hash, salt)`? Always performs one Argon2
/// computation, even with nothing stored (`None`, or malformed values), so an
/// unknown account costs the same time as a wrong key. The comparison is
/// constant-time. `Err(rate_limited)` when the server is busy hashing.
pub async fn check_verifier(key: Vec<u8>, stored: Option<(Vec<u8>, Vec<u8>)>) -> ApiResult<bool> {
    let wellformed = stored.filter(|(hash, salt)| hash.len() == KEY_LEN && salt.len() == SALT_LEN);
    let have = wellformed.is_some();
    let (expected, salt) = wellformed.unwrap_or_else(|| (vec![0; KEY_LEN], DUMMY_SALT.to_vec()));
    let computed = hash_bounded(key, salt).await?;
    let equal: bool = computed.as_slice().ct_eq(&expected).into();
    Ok(equal && have)
}

// -------------------------------------------------------------- prelogin

/// How long a made-up username keeps one fake salt.
pub const FAKE_SALT_PERIOD_MS: i64 = 90 * 24 * 60 * 60 * 1000;

/// Fake KDF parameters for a username that has no account at `now`:
/// production defaults and a salt that is stable for [`FAKE_SALT_PERIOD_MS`]
/// and then changes, at a different moment for each name. A real account's
/// salt changes now and then (password change, recovery), so a fake one that
/// never changed would give the made-up name away to a patient poller.
pub fn fake_kdf(server_secret: &[u8; 32], username: &str, now: i64) -> KdfWire {
    let hmac = |parts: &[&[u8]]| {
        let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(server_secret)
            .expect("HMAC accepts any key length");
        for p in parts {
            mac.update(p);
        }
        mac.finalize().into_bytes()
    };
    let phase = hmac(&[b"prelogin-phase|", username.as_bytes()]);
    let phase = u64::from_be_bytes(phase[..8].try_into().expect("8 bytes"))
        % FAKE_SALT_PERIOD_MS.unsigned_abs();
    let epoch = (now.max(0).unsigned_abs() + phase) / FAKE_SALT_PERIOD_MS.unsigned_abs();
    let tag = hmac(&[
        b"prelogin|",
        &epoch.to_be_bytes(),
        b"|",
        username.as_bytes(),
    ]);
    KdfWire {
        m_kib: FAKE_M_KIB,
        t: FAKE_T,
        p: FAKE_P,
        salt: tag[..SALT_LEN].to_vec(),
    }
}

async fn prelogin(
    State(st): State<AppState>,
    ClientIp(ip): ClientIp,
    headers: HeaderMap,
    Json(req): Json<PreloginReq>,
) -> ApiResult<Json<PreloginResp>> {
    let username = normalize_username(&req.username)?;
    // One of the account's own devices skips the username window, which
    // anyone who knows the name could keep full. Device ids are not secret,
    // but prelogin hands out nothing a stranger cannot already get.
    let device = headers.get(HEADER_DEVICE).and_then(|v| v.to_str().ok());
    let own: Option<i64> = match device {
        Some(d) => {
            sqlx::query_scalar(
                "SELECT 1 FROM devices d JOIN accounts a ON a.id = d.account_id \
                 WHERE a.username = ? AND d.id = ? AND d.revoked = 0",
            )
            .bind(&username)
            .bind(d)
            .fetch_optional(&st.db)
            .await?
        }
        None => None,
    };
    if own.is_some() {
        st.limiter.check_ip(ip, st.now_ms(), &st.limits)?;
    } else {
        st.limiter
            .check_attempt(ip, &username, st.now_ms(), &st.limits)?;
    }
    let row: Option<(i64, i64, i64, Vec<u8>)> =
        sqlx::query_as("SELECT kdf_m_kib, kdf_t, kdf_p, kdf_salt FROM accounts WHERE username = ?")
            .bind(&username)
            .fetch_optional(&st.db)
            .await?;
    let kdf = match row {
        Some((m_kib, t, p, salt)) => KdfWire {
            m_kib: u32::try_from(m_kib).map_err(ApiError::internal)?,
            t: u32::try_from(t).map_err(ApiError::internal)?,
            p: u32::try_from(p).map_err(ApiError::internal)?,
            salt,
        },
        None => fake_kdf(&st.server_secret, &username, st.now_ms()),
    };
    Ok(Json(PreloginResp { kdf }))
}

// --------------------------------------------------------------- invites

/// Store a new single-use invite as a short code: `token_hash` is the code's
/// key (so its full-token form also redeems it) and `code_id` its public id.
pub async fn create_short_invite<'e>(
    db: impl sqlx::Executor<'e, Database = sqlx::Sqlite>,
    now: i64,
    note: Option<&str>,
    ttl_ms: i64,
) -> Result<core_proto::ShortCode, sqlx::Error> {
    let code = core_proto::ShortCode::from_bytes(random_bytes::<10>());
    sqlx::query(
        "INSERT INTO invites (token_hash, note, created_at, expires_at, code_id) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(code.key().to_vec())
    .bind(note)
    .bind(now)
    .bind(now.saturating_add(ttl_ms))
    .bind(code.id().to_vec())
    .execute(db)
    .await?;
    Ok(code)
}

fn invite_invalid() -> ApiError {
    ApiError::of(ErrorCode::InviteInvalid)
}

/// Proof binding for a short code: the served certificate's SPKI pin when the
/// client pinned, else empty. A pin claim with TLS off cannot be honoured.
fn code_binding(st: &AppState, pinned: bool) -> ApiResult<Vec<u8>> {
    match (pinned, st.own_spki) {
        (false, _) => Ok(Vec::new()),
        (true, Some(pin)) => Ok(pin.to_vec()),
        (true, None) => Err(invite_invalid()),
    }
}

/// The key (`token_hash`) of the unused, unexpired invite with `code_id`.
async fn usable_code_key(db: &SqlitePool, code_id: &[u8; 16], now: i64) -> ApiResult<[u8; 32]> {
    let key: Option<Vec<u8>> = sqlx::query_scalar(
        "SELECT token_hash FROM invites WHERE code_id = ? AND used_at IS NULL AND expires_at > ?",
    )
    .bind(code_id.as_slice())
    .bind(now)
    .fetch_optional(db)
    .await?;
    key.and_then(|k| k.try_into().ok())
        .ok_or_else(invite_invalid)
}

/// `bytes` as a fixed-size array, or `invite_invalid` (malformed code fields).
fn code_field<const N: usize>(bytes: &[u8]) -> ApiResult<[u8; N]> {
    bytes.try_into().map_err(|_| invite_invalid())
}

// ----------------------------------------------------------- code verify

async fn code_verify(
    State(st): State<AppState>,
    ClientIp(ip): ClientIp,
    Json(req): Json<CodeVerifyReq>,
) -> ApiResult<Json<CodeVerifyResp>> {
    let code_id: [u8; 16] = code_field(&req.code_id)?;
    let nonce: [u8; 32] = code_field(&req.client_nonce)?;
    let binding = code_binding(&st, req.pinned)?;
    let now = st.now_ms();
    // Per code id, so a stranger cannot block setup without the id; '#' is
    // never in a username.
    let key = format!("#code:{}", data_encoding::HEXLOWER.encode(&code_id));
    st.limiter.check_attempt(ip, &key, now, &st.limits)?;
    let key = usable_code_key(&st.db, &code_id, now).await?;
    Ok(Json(CodeVerifyResp {
        server_proof: core_proto::server_proof(&key, &binding, &nonce).to_vec(),
    }))
}

// -------------------------------------------------------------- register

async fn register(
    State(st): State<AppState>,
    ClientIp(ip): ClientIp,
    Json(req): Json<RegisterReq>,
) -> ApiResult<Json<TokenResp>> {
    // Everything cheap and syntactic first, so malformed requests neither use
    // the rate limit nor the invite.
    let username = normalize_username(&req.username)?;
    let auth_key: [u8; KEY_LEN] = fixed("authKey", &req.auth_key)?;
    let recovery_key: [u8; KEY_LEN] = fixed("recoveryAuthKey", &req.recovery_auth_key)?;
    let (device_name, platform) = validate_device(&req.device)?;
    validate_material(&req.material)?;
    // A short-code proof (checked below, after the rate limit) or a full token.
    let code = match &req.code_proof {
        Some(p) => Some((
            code_field::<16>(&p.code_id)?,
            code_field::<32>(&p.client_nonce)?,
            code_field::<32>(&p.proof)?,
            code_binding(&st, p.pinned)?,
        )),
        None => None,
    };
    let token_hash = match code {
        Some(_) => Vec::new(),
        None => Sha256::digest(code_field::<32>(&req.invite_token)?).to_vec(),
    };

    let now = st.now_ms();
    st.limiter.check_attempt(ip, &username, now, &st.limits)?;

    // Refuse an unusable invite (or a wrong proof) before spending two Argon2
    // runs on it. The authoritative claim below is atomic.
    let invite_hash = match code {
        Some((code_id, nonce, proof, binding)) => {
            let key = usable_code_key(&st.db, &code_id, now).await?;
            if !core_proto::verify_client_proof(&key, &binding, &nonce, &proof) {
                return Err(invite_invalid());
            }
            key.to_vec()
        }
        None => {
            let usable: Option<i64> = sqlx::query_scalar(
                "SELECT 1 FROM invites WHERE token_hash = ? AND used_at IS NULL AND expires_at > ?",
            )
            .bind(&token_hash)
            .bind(now)
            .fetch_optional(&st.db)
            .await?;
            if usable.is_none() {
                return Err(invite_invalid());
            }
            token_hash
        }
    };

    let (auth_hash, auth_salt) = new_verifier(auth_key.to_vec()).await?;
    let (rec_hash, rec_salt) = new_verifier(recovery_key.to_vec()).await?;

    let mut tx = db::begin_write(&st.db).await?;
    let claimed = sqlx::query(
        "UPDATE invites SET used_at = ?1 WHERE token_hash = ?2 AND used_at IS NULL AND expires_at > ?1",
    )
    .bind(now)
    .bind(&invite_hash)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if claimed != 1 {
        return Err(invite_invalid());
    }
    let taken: Option<i64> = sqlx::query_scalar("SELECT 1 FROM accounts WHERE username = ?")
        .bind(&username)
        .fetch_optional(&mut *tx)
        .await?;
    if taken.is_some() {
        // Dropping the transaction rolls the invite claim back.
        return Err(ApiError::of(ErrorCode::UsernameTaken));
    }

    let account_id = uuid::Uuid::now_v7().to_string();
    let m = &req.material;
    let inserted = sqlx::query(
        "INSERT INTO accounts (id, username, verifier, verifier_salt, recovery_verifier, \
         recovery_verifier_salt, kdf_m_kib, kdf_t, kdf_p, kdf_salt, vault_id, key_version, \
         account_public, secret_by_kek, secret_by_recovery, sealed_vault_key, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&account_id)
    .bind(&username)
    .bind(&auth_hash)
    .bind(&auth_salt)
    .bind(&rec_hash)
    .bind(&rec_salt)
    .bind(i64::from(m.kdf.m_kib))
    .bind(i64::from(m.kdf.t))
    .bind(i64::from(m.kdf.p))
    .bind(&m.kdf.salt)
    .bind(&m.vault_id)
    .bind(i64::from(m.key_version))
    .bind(&m.account_public)
    .bind(&m.secret_by_kek)
    .bind(&m.secret_by_recovery)
    .bind(&m.sealed_vault_key)
    .bind(now)
    .execute(&mut *tx)
    .await;
    match inserted {
        Ok(_) => {}
        Err(e)
            if e.as_database_error()
                .is_some_and(|d| d.is_unique_violation()) =>
        {
            return Err(ApiError::new(
                ErrorCode::Conflict,
                "a vault with that id already exists",
            ));
        }
        Err(e) => return Err(e.into()),
    }
    // The first account closes the setup path, whichever invite it used.
    crate::setup::close(&mut tx, now).await?;

    let dev = devices::create_device(
        &mut tx,
        &account_id,
        &device_name,
        &platform,
        Some(req.node_hint),
        now,
    )
    .await?;
    let session = auth::start_session(&mut tx, &account_id, &dev.id, now).await?;
    tx.commit().await?;

    tracing::info!(account = %account_id, device = %dev.id, node = dev.node, "account registered");
    let mut resp = auth::token_resp(&st, session, dev.id, dev.node);
    resp.device_secret = dev.secret;
    Ok(Json(resp))
}

/// Routes of this module.
pub fn routes() -> axum::Router<AppState> {
    axum::Router::new()
        .route("/v1/prelogin", post(prelogin))
        .route("/v1/register", post(register))
        .route("/v1/code/verify", post(code_verify))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn hash_slots_are_bounded_and_refuse_fast() {
        let slots = Semaphore::new(2);
        let a = take_slot(&slots, Duration::from_millis(10)).await.unwrap();
        let _b = take_slot(&slots, Duration::from_millis(10)).await.unwrap();
        let err = take_slot(&slots, Duration::from_millis(10)).await.unwrap_err();
        assert_eq!(err.code(), ErrorCode::RateLimited);
        assert_eq!(err.message(), "server busy; try again in a moment");
        drop(a);
        assert!(take_slot(&slots, Duration::from_millis(10)).await.is_ok());
    }

    #[test]
    fn usernames() {
        assert_eq!(normalize_username("  Cl.ym_1-x ").unwrap(), "cl.ym_1-x");
        assert!(normalize_username("ab").is_err());
        assert!(normalize_username(&"a".repeat(64)).is_ok());
        assert!(normalize_username(&"a".repeat(65)).is_err());
        for bad in ["a b c", "ünï", "a/b", "a@b.c", "a\nb", "", "   "] {
            assert!(normalize_username(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn fake_kdf_is_stable_and_per_name() {
        let secret = [7u8; 32];
        let now = 1_800_000_000_000;
        let a = fake_kdf(&secret, "alice", now);
        assert_eq!(a, fake_kdf(&secret, "alice", now));
        assert_ne!(a.salt, fake_kdf(&secret, "bob", now).salt);
        assert_ne!(a.salt, fake_kdf(&[8u8; 32], "alice", now).salt);
        assert_eq!((a.m_kib, a.t, a.p, a.salt.len()), (65536, 3, 4, 16));
    }

    #[test]
    fn fake_salt_changes_once_a_period_at_a_per_name_moment() {
        let secret = [7u8; 32];
        let now = 1_800_000_000_000;
        let step = FAKE_SALT_PERIOD_MS / 100;
        // Over one period each name's salt changes exactly once...
        let changes = |name: &str| {
            (0..100)
                .filter(|i| {
                    fake_kdf(&secret, name, now + i * step).salt
                        != fake_kdf(&secret, name, now + (i + 1) * step).salt
                })
                .collect::<Vec<_>>()
        };
        let (a, b) = (changes("alice"), changes("bob"));
        assert_eq!((a.len(), b.len()), (1, 1));
        // ...and not at the same moment for every name.
        assert_ne!(a, b);
    }

    #[tokio::test]
    async fn verifier_matches_only_the_right_key() {
        let key = vec![5u8; 32];
        let (hash, salt) = new_verifier(key.clone()).await.unwrap();
        assert_ne!(hash, key);
        assert!(check_verifier(key.clone(), Some((hash.clone(), salt.clone()))).await.unwrap());
        assert!(!check_verifier(vec![6u8; 32], Some((hash.clone(), salt.clone()))).await.unwrap());
        // A different salt is a different hash.
        assert!(!check_verifier(key.clone(), Some((hash, vec![1u8; 16]))).await.unwrap());
        // Nothing stored, or garbage stored: never a match.
        assert!(!check_verifier(key.clone(), None).await.unwrap());
        assert!(!check_verifier(key, Some((vec![0; 32], vec![0; 3]))).await.unwrap());
    }

    #[test]
    fn material_bounds() {
        let ok = AccountMaterial {
            vault_id: uuid::Uuid::now_v7().to_string(),
            key_version: 1,
            kdf: KdfWire {
                m_kib: 65536,
                t: 3,
                p: 4,
                salt: vec![0; 16],
            },
            account_public: vec![0; 32],
            secret_by_kek: vec![0; 73],
            secret_by_recovery: vec![0; 73],
            sealed_vault_key: vec![0; 81],
        };
        assert!(validate_material(&ok).is_ok());
        let mut m = ok.clone();
        m.vault_id = m.vault_id.to_uppercase();
        assert!(validate_material(&m).is_err());
        let mut m = ok.clone();
        m.kdf.t = 17;
        assert!(validate_material(&m).is_err());
        let mut m = ok.clone();
        m.kdf.p = 65;
        assert!(validate_material(&m).is_err());
        let mut m = ok.clone();
        m.kdf.m_kib = 8 * 4 - 1;
        assert!(validate_material(&m).is_err());
        let mut m = ok;
        m.key_version = 0;
        assert!(validate_material(&m).is_err());
    }
}
