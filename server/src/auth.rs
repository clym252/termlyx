//! Sessions and authentication: login, recovery login, refresh, logout, and
//! the extractors that guard the other endpoints.
//!
//! Tokens (access, refresh, and the invite and link tokens of other modules)
//! are 32 random bytes. Clients hold them as base64url without padding (they
//! travel in the WebSocket subprotocol); the database holds only the SHA-256
//! of the 32 bytes. Access tokens last 15 minutes, refresh tokens rotate on
//! every use, and a rotated-out refresh token presented again revokes the
//! device (it must have leaked) — except once, within [`REFRESH_GRACE_MS`]
//! of the rotation, while the token issued in its place has never been used:
//! then the answer to the exchange was most likely lost on the way, and the
//! exchange is answered again (the unused successor is invalidated).

use axum::Json;
use axum::extract::{FromRequestParts, State};
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use core_proto::{
    ErrorCode, Event, HEADER_DEVICE, HEADER_DEVICE_SECRET, HEADER_SCHEMA, KNOWN_SCHEMA, LoginReq,
    RecoverReq, RefreshReq, TokenResp,
};
use data_encoding::BASE64URL_NOPAD;
use sha2::{Digest, Sha256};
use sqlx::SqliteConnection;
use subtle::ConstantTimeEq;

use crate::accounts::{check_verifier, fixed, normalize_username, validate_device};
use crate::db;
use crate::devices;
use crate::error::{ApiError, ApiResult};
use crate::limits::ClientIp;
use crate::state::{AppState, Target};
use crate::util::random_bytes;

/// Access token lifetime.
pub const ACCESS_TTL_MS: i64 = 15 * 60 * 1000;
/// Lifetime of the limited token `recover-login` issues.
pub const RECOVERY_TTL_MS: i64 = 10 * 60 * 1000;
/// Rotated refresh tokens are kept this long for reuse detection, then pruned.
const ROTATED_KEEP_MS: i64 = 30 * 24 * 60 * 60 * 1000;
/// How long after its rotation a refresh token may be presented once more,
/// provided its successor was never used (a lost answer, not a theft).
pub const REFRESH_GRACE_MS: i64 = 10 * 60 * 1000;
/// Do not rewrite `devices.last_seen` more often than this.
const LAST_SEEN_GRANULARITY_MS: i64 = 60 * 1000;

/// `accounts.scope` / `access_tokens.scope` of an ordinary session.
const SCOPE_FULL: &str = "full";
/// Scope of the token `recover-login` issues: it may only reset the password.
const SCOPE_RECOVER: &str = "recover";

/// A token as the client holds it (43 base64url characters).
pub fn encode_token(bytes: &[u8; 32]) -> String {
    BASE64URL_NOPAD.encode(bytes)
}

/// SHA-256 of the 32 bytes a client-held token encodes; `None` when `token`
/// is not exactly a canonical base64url encoding of 32 bytes.
pub fn token_hash(token: &str) -> Option<Vec<u8>> {
    if token.len() != 43 {
        return None;
    }
    let bytes = BASE64URL_NOPAD.decode(token.as_bytes()).ok()?;
    (bytes.len() == 32).then(|| Sha256::digest(&bytes).to_vec())
}

fn unauthorized() -> ApiError {
    ApiError::of(ErrorCode::Unauthorized)
}

fn wrong_credentials() -> ApiError {
    ApiError::new(ErrorCode::Unauthorized, "wrong username or password")
}

/// A freshly issued session, before it is shaped into a [`TokenResp`].
pub struct Session {
    access_token: String,
    refresh_token: String,
    /// SHA-256 of the refresh token (empty for the recovery session).
    refresh_hash: Vec<u8>,
    ttl_ms: i64,
}

/// Issue an access and a refresh token for `device_id`, pruning what is
/// stale: this device's expired access tokens and long-rotated refresh
/// tokens.
pub async fn start_session(
    conn: &mut SqliteConnection,
    account_id: &str,
    device_id: &str,
    now: i64,
) -> ApiResult<Session> {
    let access = random_bytes::<32>();
    let refresh = random_bytes::<32>();
    let refresh_hash = Sha256::digest(refresh).to_vec();
    sqlx::query("DELETE FROM access_tokens WHERE device_id = ? AND expires_at <= ?")
        .bind(device_id)
        .bind(now)
        .execute(&mut *conn)
        .await?;
    sqlx::query("DELETE FROM refresh_tokens WHERE device_id = ? AND rotated_at <= ?")
        .bind(device_id)
        .bind(now - ROTATED_KEEP_MS)
        .execute(&mut *conn)
        .await?;
    sqlx::query(
        "INSERT INTO access_tokens (hash, account_id, device_id, scope, expires_at, refresh_hash) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(Sha256::digest(access).to_vec())
    .bind(account_id)
    .bind(device_id)
    .bind(SCOPE_FULL)
    .bind(now + ACCESS_TTL_MS)
    .bind(&refresh_hash)
    .execute(&mut *conn)
    .await?;
    sqlx::query("INSERT INTO refresh_tokens (hash, device_id, created_at) VALUES (?, ?, ?)")
        .bind(&refresh_hash)
        .bind(device_id)
        .bind(now)
        .execute(&mut *conn)
        .await?;
    Ok(Session {
        access_token: encode_token(&access),
        refresh_token: encode_token(&refresh),
        refresh_hash,
        ttl_ms: ACCESS_TTL_MS,
    })
}

/// Issue the limited-scope token for the password-reset flow: no device, no
/// refresh token, ten minutes.
async fn start_recovery_session(
    conn: &mut SqliteConnection,
    account_id: &str,
    now: i64,
) -> ApiResult<Session> {
    let access = random_bytes::<32>();
    sqlx::query(
        "DELETE FROM access_tokens WHERE account_id = ? AND device_id IS NULL AND expires_at <= ?",
    )
    .bind(account_id)
    .bind(now)
    .execute(&mut *conn)
    .await?;
    sqlx::query(
        "INSERT INTO access_tokens (hash, account_id, device_id, scope, expires_at) \
         VALUES (?, ?, NULL, ?, ?)",
    )
    .bind(Sha256::digest(access).to_vec())
    .bind(account_id)
    .bind(SCOPE_RECOVER)
    .bind(now + RECOVERY_TTL_MS)
    .execute(&mut *conn)
    .await?;
    Ok(Session {
        access_token: encode_token(&access),
        refresh_token: String::new(),
        refresh_hash: Vec::new(),
        ttl_ms: RECOVERY_TTL_MS,
    })
}

/// Shape a [`Session`] into the wire response.
pub fn token_resp(st: &AppState, s: Session, device_id: String, node: u16) -> TokenResp {
    TokenResp {
        access_token: s.access_token,
        refresh_token: s.refresh_token,
        expires_in: u64::try_from(s.ttl_ms / 1000).unwrap_or(0),
        device_id,
        node,
        epoch: st.epoch.clone(),
        device_secret: String::new(),
    }
}

// ------------------------------------------------------------ extractors

/// A request authenticated by a device's access token
/// (`Authorization: Bearer <token>`).
///
/// Rejects: no/malformed/unknown/expired token with 401 `unauthorized`; a token
/// of a revoked device with 401 `revoked`; a disabled account, or the limited
/// recovery token, with 403 `forbidden`.
#[derive(Debug, Clone)]
pub struct AuthDevice {
    /// The account.
    pub account_id: String,
    /// The calling device.
    pub device_id: String,
    /// The account's vault.
    pub vault_id: String,
    /// The client's record kind set version (`X-Tether-Schema`; 1 if absent).
    /// Writes pass it to [`crate::account::require_write_schema`].
    pub schema: u32,
}

/// A request authenticated by the limited token from `recover-login`. Only the
/// password-reset endpoint accepts it; full session tokens are refused here
/// with 403 `forbidden`.
#[derive(Debug, Clone)]
pub struct RecoveryAuth {
    /// The account being recovered.
    pub account_id: String,
    /// The account's vault.
    pub vault_id: String,
    /// The client's record kind set version (`X-Tether-Schema`; 1 if absent).
    pub schema: u32,
}

/// A request authenticated by either kind of token: a device session or the
/// limited recovery token. For the read-only endpoints both may call
/// (`GET /v1/account`).
#[derive(Debug, Clone)]
pub struct AnyAuth {
    /// The account.
    pub account_id: String,
    /// The account's vault.
    pub vault_id: String,
}

struct Resolved {
    account_id: String,
    device_id: Option<String>,
    vault_id: String,
    scope: String,
    schema: u32,
    /// The account's stored high-water mark when the token was looked up.
    max_schema: i64,
}

/// How far above [`KNOWN_SCHEMA`] a client's `X-Tether-Schema` may be. A value
/// beyond it is refused, so one buggy or hostile device cannot raise an
/// account's high-water mark out of reach of the user's other devices.
const SCHEMA_SLACK: u32 = 8;

/// The client's schema from `X-Tether-Schema`. Absent, empty, zero or not a
/// number count as 1 (the first kind set), so a bad header can never raise the
/// account's high-water mark. A number above `KNOWN_SCHEMA + 8` is
/// `400 bad_request`.
fn client_schema(parts: &Parts) -> ApiResult<u32> {
    let Some(text) = parts
        .headers
        .get(HEADER_SCHEMA)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
    else {
        return Ok(1);
    };
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return Ok(1);
    }
    // All digits: too long for a u64 is certainly absurd.
    let n = text.parse::<u64>().unwrap_or(u64::MAX);
    if n > u64::from(KNOWN_SCHEMA + SCHEMA_SLACK) {
        return Err(ApiError::bad_request(
            "X-Tether-Schema is far newer than this server knows",
        ));
    }
    Ok(u32::try_from(n).map_or(1, |v| v.max(1)))
}

/// Remember the newest kind set any device of the account speaks, so older
/// clients can be refused writes (see `account::require_write_schema`). Called
/// by the extractors only once the token's scope was accepted. Only a higher
/// value writes, so ordinary requests stay read-only. Best effort: a database
/// error is logged, never returned (the request itself is authenticated).
async fn record_schema(st: &AppState, r: &Resolved) {
    if i64::from(r.schema) <= r.max_schema {
        return;
    }
    let res = sqlx::query("UPDATE accounts SET max_schema = ?1 WHERE id = ?2 AND max_schema < ?1")
        .bind(i64::from(r.schema))
        .bind(&r.account_id)
        .execute(&st.db)
        .await;
    if let Err(e) = res {
        tracing::warn!(account = %r.account_id, error = %e, "could not record the client schema");
    }
}

fn bearer(parts: &Parts) -> ApiResult<&str> {
    parts
        .headers
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            let (scheme, token) = v.split_once(' ')?;
            scheme
                .eq_ignore_ascii_case("bearer")
                .then_some(token.trim())
        })
        .ok_or_else(unauthorized)
}

/// Look the `Authorization` token up and apply the checks every kind of token
/// shares.
async fn resolve(st: &AppState, parts: &Parts) -> ApiResult<Resolved> {
    resolve_token(st, bearer(parts)?, client_schema(parts)).await
}

/// [`resolve`] for a token that did not come from the `Authorization` header.
/// `schema` is the client's kind set version, or the error its header earned,
/// which is reported last (after the token itself was judged).
async fn resolve_token(st: &AppState, token: &str, schema: ApiResult<u32>) -> ApiResult<Resolved> {
    let hash = token_hash(token).ok_or_else(unauthorized)?;
    // account, device, scope, expiry, vault, disabled, max schema, revoked,
    // last seen (device columns are NULL for the recovery token), used.
    type Row = (
        String,
        Option<String>,
        String,
        i64,
        String,
        i64,
        i64,
        Option<i64>,
        Option<i64>,
        i64,
    );
    let row: Option<Row> = sqlx::query_as(
        "SELECT t.account_id, t.device_id, t.scope, t.expires_at, a.vault_id, a.disabled, \
             a.max_schema, d.revoked, d.last_seen, t.used \
             FROM access_tokens t JOIN accounts a ON a.id = t.account_id \
             LEFT JOIN devices d ON d.id = t.device_id WHERE t.hash = ?",
    )
    .bind(&hash)
    .fetch_optional(&st.db)
    .await?;
    let (
        account_id,
        device_id,
        scope,
        expires_at,
        vault_id,
        disabled,
        max_schema,
        revoked,
        last_seen,
        used,
    ) = row.ok_or_else(unauthorized)?;
    let now = st.now_ms();
    // A revoked device is told so even if its token has also expired.
    if revoked == Some(1) {
        return Err(ApiError::of(ErrorCode::Revoked));
    }
    if expires_at <= now {
        return Err(unauthorized());
    }
    if disabled != 0 {
        return Err(ApiError::new(
            ErrorCode::Forbidden,
            "this account is disabled",
        ));
    }
    if used == 0 && device_id.is_some() {
        // The first use of a session's access token ends the refresh grace
        // of the token it replaced (see `refresh`). Best effort, once per
        // access token: a failure only leaves that grace open.
        if let Err(e) = sqlx::query("UPDATE access_tokens SET used = 1 WHERE hash = ?")
            .bind(&hash)
            .execute(&st.db)
            .await
        {
            tracing::warn!(error = %e, "could not mark an access token used");
        }
    }
    if let (Some(dev), Some(seen)) = (&device_id, last_seen)
        && now - seen >= LAST_SEEN_GRANULARITY_MS
    {
        // Best effort: a failure to record presence must not fail the request.
        let _ = sqlx::query("UPDATE devices SET last_seen = ? WHERE id = ?")
            .bind(now)
            .bind(dev)
            .execute(&st.db)
            .await;
    }
    let schema = schema?;
    Ok(Resolved {
        account_id,
        device_id,
        vault_id,
        scope,
        schema,
        max_schema,
    })
}

/// Accept a resolved token as a device session (full scope, has a device).
async fn device_session(st: &AppState, r: Resolved) -> ApiResult<AuthDevice> {
    // The schema is recorded only for a token this accepts.
    let Some(device_id) = r.device_id.clone().filter(|_| r.scope == SCOPE_FULL) else {
        return Err(ApiError::new(
            ErrorCode::Forbidden,
            "this token can only be used to reset the password",
        ));
    };
    record_schema(st, &r).await;
    Ok(AuthDevice {
        account_id: r.account_id,
        device_id,
        vault_id: r.vault_id,
        schema: r.schema,
    })
}

/// Authenticate an access token that was not sent as `Authorization: Bearer`
/// (the events WebSocket carries it in a subprotocol) with the rules of the
/// [`AuthDevice`] extractor. No `X-Tether-Schema` applies: the schema is 1,
/// which never raises the account's high-water mark.
pub async fn authenticate_token(st: &AppState, token: &str) -> ApiResult<AuthDevice> {
    device_session(st, resolve_token(st, token, Ok(1)).await?).await
}

impl FromRequestParts<AppState> for AuthDevice {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, st: &AppState) -> Result<Self, ApiError> {
        device_session(st, resolve(st, parts).await?).await
    }
}

impl FromRequestParts<AppState> for RecoveryAuth {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, st: &AppState) -> Result<Self, ApiError> {
        let r = resolve(st, parts).await?;
        if r.scope != SCOPE_RECOVER || r.device_id.is_some() {
            return Err(ApiError::new(
                ErrorCode::Forbidden,
                "this needs a recovery sign-in",
            ));
        }
        record_schema(st, &r).await;
        Ok(RecoveryAuth {
            account_id: r.account_id,
            vault_id: r.vault_id,
            schema: r.schema,
        })
    }
}

impl FromRequestParts<AppState> for AnyAuth {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, st: &AppState) -> Result<Self, ApiError> {
        let r = resolve(st, parts).await?;
        // A device token must be a full session; the recovery token has no
        // device. Anything else (a device with a recovery scope) is not issued.
        let ok = matches!(
            (&r.device_id, r.scope.as_str()),
            (Some(_), SCOPE_FULL) | (None, SCOPE_RECOVER)
        );
        if !ok {
            return Err(unauthorized());
        }
        record_schema(st, &r).await;
        Ok(AnyAuth {
            account_id: r.account_id,
            vault_id: r.vault_id,
        })
    }
}

// ----------------------------------------------------------- credentials

/// Which secret a sign-in presents.
#[derive(Clone, Copy)]
pub(crate) enum Secret {
    /// The AuthKey (`login`).
    Auth,
    /// The recovery auth key (`recover-login`).
    Recovery,
}

/// Rate-limit, lockout and credential check shared by `login` and
/// `recover-login`. Returns the account id once the key matched; the caller
/// runs [`clear_failures`] when the whole sign-in has succeeded.
///
/// An unknown username and a wrong key are indistinguishable: the same
/// error, the same Argon2 work (against a dummy verifier), the same lockout.
///
/// `proven` (see [`device_proof`]) skips the per-username window and the
/// lockout, so a stranger who knows the username cannot keep the account's
/// own devices out. The address windows and the Argon2 check still apply.
async fn check_credentials(
    st: &AppState,
    ip: ClientIp,
    username: &str,
    key: [u8; 32],
    which: Secret,
    proven: bool,
) -> ApiResult<String> {
    let now = st.now_ms();
    if proven {
        st.limiter.check_ip(ip.0, now, &st.limits)?;
    } else {
        st.limiter.check_attempt(ip.0, username, now, &st.limits)?;
    }

    type Row = (
        String,
        Vec<u8>,
        Vec<u8>,
        Option<Vec<u8>>,
        Option<Vec<u8>>,
        i64,
        i64,
    );
    let row: Option<Row> = sqlx::query_as(
        "SELECT id, verifier, verifier_salt, recovery_verifier, recovery_verifier_salt, \
         locked_until, disabled FROM accounts WHERE username = ?",
    )
    .bind(username)
    .fetch_optional(&st.db)
    .await?;

    let Some((id, auth_hash, auth_salt, rec_hash, rec_salt, locked_until, disabled)) = row else {
        if st.limiter.unknown_locked(username, now) {
            return Err(ApiError::of(ErrorCode::RateLimited));
        }
        check_verifier(key.to_vec(), None).await?;
        // A wrong key on a real account writes its failure count; write
        // something too, so the answer takes about as long. (A value that
        // changes: SQLite skips rewriting an identical row.)
        sqlx::query(
            "INSERT INTO meta (k, v) VALUES ('unknown_failure', randomblob(8)) \
             ON CONFLICT (k) DO UPDATE SET v = excluded.v",
        )
        .execute(&st.db)
        .await?;
        st.limiter.record_unknown_failure(username, now, &st.limits);
        tracing::warn!("sign-in failed: unknown username");
        return Err(wrong_credentials());
    };

    if locked_until > now && !proven {
        return Err(ApiError::of(ErrorCode::RateLimited));
    }
    let stored = match which {
        Secret::Auth => Some((auth_hash, auth_salt)),
        Secret::Recovery => rec_hash.zip(rec_salt),
    };
    if !check_verifier(key.to_vec(), stored).await? {
        record_failure(st, &id, now).await?;
        tracing::warn!(account = %id, "sign-in failed: wrong key");
        return Err(wrong_credentials());
    }
    if disabled != 0 {
        return Err(ApiError::new(
            ErrorCode::Forbidden,
            "this account is disabled",
        ));
    }
    Ok(id)
}

/// Does a sign-in for `username` carry proof that it comes from the account
/// itself: a live add-device code of the account (`link`), or the id and
/// secret of one of its devices that is not revoked? Unlike the username,
/// neither is known to strangers.
async fn device_proof(
    st: &AppState,
    username: &str,
    headers: &HeaderMap,
    link: Option<&[u8; 32]>,
    now: i64,
) -> ApiResult<bool> {
    if let Some(token) = link {
        let live: Option<i64> = sqlx::query_scalar(
            "SELECT 1 FROM link_tokens l JOIN accounts a ON a.id = l.account_id \
             WHERE a.username = ? AND l.hash = ? AND l.used_at IS NULL AND l.expires_at > ?",
        )
        .bind(username)
        .bind(Sha256::digest(token).to_vec())
        .bind(now)
        .fetch_optional(&st.db)
        .await?;
        return Ok(live.is_some());
    }
    let header = |name| headers.get(name).and_then(|v| v.to_str().ok());
    let (Some(device_id), Some(presented)) = (
        header(HEADER_DEVICE),
        header(HEADER_DEVICE_SECRET).and_then(token_hash),
    ) else {
        return Ok(false);
    };
    let stored: Option<Option<Vec<u8>>> = sqlx::query_scalar(
        "SELECT d.secret_hash FROM devices d JOIN accounts a ON a.id = d.account_id \
         WHERE a.username = ? AND d.id = ? AND d.revoked = 0",
    )
    .bind(username)
    .bind(device_id)
    .fetch_optional(&st.db)
    .await?;
    Ok(
        matches!(stored, Some(Some(h)) if h.len() == presented.len() && bool::from(h.ct_eq(&presented))),
    )
}

/// Count a failed sign-in for a real account. Failures are "consecutive" within
/// a window: one made more than a lockout window after the previous restarts
/// the count. The failure that reaches the threshold starts the lock and
/// restarts the count, so ten more are needed after it ends.
async fn record_failure(st: &AppState, account_id: &str, now: i64) -> ApiResult<()> {
    // ?1 threshold, ?2 lock end, ?3 account, ?4 now, ?5 window. The inner
    // CASE is the count after applying the decay rule.
    sqlx::query(
        "UPDATE accounts SET \
           locked_until = CASE WHEN \
             (CASE WHEN ?4 - last_failed_at > ?5 THEN 0 ELSE failed_logins END) + 1 >= ?1 \
             THEN ?2 ELSE locked_until END, \
           failed_logins = CASE WHEN \
             (CASE WHEN ?4 - last_failed_at > ?5 THEN 0 ELSE failed_logins END) + 1 >= ?1 \
             THEN 0 \
             ELSE (CASE WHEN ?4 - last_failed_at > ?5 THEN 0 ELSE failed_logins END) + 1 END, \
           last_failed_at = ?4 \
         WHERE id = ?3",
    )
    .bind(i64::from(st.limits.lockout_failures))
    .bind(now + st.limits.lockout_ms)
    .bind(account_id)
    .bind(now)
    .bind(st.limits.lockout_ms)
    .execute(&st.db)
    .await?;
    Ok(())
}

/// Check a key for an account that is already authenticated by a token
/// (password change, recovery reset, recovery key replacement). The same rules
/// as a sign-in: a locked account answers `429 rate_limited` even for the
/// right key, a wrong key counts toward the lockout and answers `401
/// unauthorized`.
///
/// Returns the `account_rev` that was current when the verifier was read (the
/// same row), so the caller's write can require that revision and be certain
/// the key it checked is the one in force. With `expected_rev`, a different
/// revision is `409 conflict` before any hashing. The caller clears the
/// failure count ([`clear_failures_in`]) inside its write transaction.
pub(crate) async fn verify_account_key(
    st: &AppState,
    account_id: &str,
    key: [u8; 32],
    which: Secret,
    expected_rev: Option<i64>,
) -> ApiResult<i64> {
    let now = st.now_ms();
    type Row = (Vec<u8>, Vec<u8>, Option<Vec<u8>>, Option<Vec<u8>>, i64, i64);
    let row: Row = sqlx::query_as(
        "SELECT verifier, verifier_salt, recovery_verifier, recovery_verifier_salt, \
         locked_until, account_rev FROM accounts WHERE id = ?",
    )
    .bind(account_id)
    .fetch_optional(&st.db)
    .await?
    .ok_or_else(unauthorized)?;
    let (auth_hash, auth_salt, rec_hash, rec_salt, locked_until, rev) = row;
    if locked_until > now {
        return Err(ApiError::of(ErrorCode::RateLimited));
    }
    if expected_rev.is_some_and(|e| e != rev) {
        return Err(ApiError::new(
            ErrorCode::Conflict,
            "the account changed on another device; refresh and try again",
        ));
    }
    let stored = match which {
        Secret::Auth => Some((auth_hash, auth_salt)),
        Secret::Recovery => rec_hash.zip(rec_salt),
    };
    if !check_verifier(key.to_vec(), stored).await? {
        record_failure(st, account_id, now).await?;
        tracing::warn!(account = %account_id, "key check failed");
        return Err(ApiError::new(
            ErrorCode::Unauthorized,
            match which {
                Secret::Auth => "wrong password",
                Secret::Recovery => "wrong recovery key",
            },
        ));
    }
    Ok(rev)
}

/// Forget earlier failed sign-ins, on the caller's connection (so inside its
/// transaction).
pub(crate) async fn clear_failures_in(
    conn: &mut SqliteConnection,
    account_id: &str,
) -> ApiResult<()> {
    sqlx::query("UPDATE accounts SET failed_logins = 0 WHERE id = ? AND failed_logins <> 0")
        .bind(account_id)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// A sign-in fully succeeded: forget earlier failures.
async fn clear_failures(st: &AppState, account_id: &str) -> ApiResult<()> {
    sqlx::query("UPDATE accounts SET failed_logins = 0 WHERE id = ? AND failed_logins <> 0")
        .bind(account_id)
        .execute(&st.db)
        .await?;
    Ok(())
}

fn link_invalid() -> ApiError {
    ApiError::new(
        ErrorCode::InviteInvalid,
        "this add-device code is not valid or has expired",
    )
}

// -------------------------------------------------------------- handlers

async fn login(
    State(st): State<AppState>,
    ip: ClientIp,
    headers: HeaderMap,
    Json(req): Json<LoginReq>,
) -> ApiResult<Json<TokenResp>> {
    let username = normalize_username(&req.username)?;
    let key = fixed::<32>("authKey", &req.auth_key)?;
    let (name, platform) = validate_device(&req.device)?;
    let link = req
        .link_token
        .as_deref()
        .map(|t| fixed::<32>("linkToken", t))
        .transpose()?;

    let now = st.now_ms();
    let proven = device_proof(&st, &username, &headers, link.as_ref(), now).await?;
    let account_id = check_credentials(&st, ip, &username, key, Secret::Auth, proven).await?;

    // Without a link code only an existing device of this account may sign in
    // again, and it must prove it is that device: device ids are not secret,
    // so the id alone (plus the password) would let anyone resurrect a
    // revoked device. A wrong or missing secret counts as a failed sign-in.
    // Checked before the write transaction opens.
    let relogin = match link {
        Some(_) => None,
        None => {
            let device_id = headers
                .get(HEADER_DEVICE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default();
            let found: Option<(i64, i64, Option<Vec<u8>>)> = sqlx::query_as(
                "SELECT revoked, node, secret_hash FROM devices WHERE id = ? AND account_id = ?",
            )
            .bind(device_id)
            .bind(&account_id)
            .fetch_optional(&st.db)
            .await?;
            let Some((revoked, node, secret_hash)) = found else {
                return Err(ApiError::new(
                    ErrorCode::Forbidden,
                    "adding a device needs an add-device code from a device that is signed in",
                ));
            };
            let presented = headers
                .get(HEADER_DEVICE_SECRET)
                .and_then(|v| v.to_str().ok())
                .and_then(token_hash);
            let proven = match (presented, secret_hash) {
                (Some(p), Some(h)) if p.len() == h.len() => p.ct_eq(&h).into(),
                _ => false,
            };
            if !proven {
                record_failure(&st, &account_id, now).await?;
                tracing::warn!(account = %account_id, device = %device_id, "sign-in failed: device secret");
                return Err(ApiError::new(
                    ErrorCode::Unauthorized,
                    "this device's secret is missing or wrong; add the device again with a code",
                ));
            }
            if revoked != 0 {
                return Err(ApiError::of(ErrorCode::Revoked));
            }
            Some((
                device_id.to_owned(),
                u16::try_from(node).map_err(ApiError::internal)?,
            ))
        }
    };

    let mut tx = db::begin_write(&st.db).await?;
    let (device_id, node, device_secret) = match (link, relogin) {
        // A link code adds a new device. It is bound to the account and
        // spent only now, after the password was right.
        (Some(token), _) => {
            let claimed = sqlx::query(
                "UPDATE link_tokens SET used_at = ?1 \
                 WHERE hash = ?2 AND account_id = ?3 AND used_at IS NULL AND expires_at > ?1",
            )
            .bind(now)
            .bind(Sha256::digest(token).to_vec())
            .bind(&account_id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
            if claimed != 1 {
                return Err(link_invalid());
            }
            let dev =
                devices::create_device(&mut tx, &account_id, &name, &platform, None, now).await?;
            (dev.id, dev.node, dev.secret)
        }
        (None, Some((device_id, node))) => {
            // The new session replaces the old one; the device keeps its name.
            sqlx::query("UPDATE devices SET platform = ?, last_seen = ? WHERE id = ?")
                .bind(&platform)
                .bind(now)
                .bind(&device_id)
                .execute(&mut *tx)
                .await?;
            sqlx::query("DELETE FROM access_tokens WHERE device_id = ?")
                .bind(&device_id)
                .execute(&mut *tx)
                .await?;
            sqlx::query("DELETE FROM refresh_tokens WHERE device_id = ?")
                .bind(&device_id)
                .execute(&mut *tx)
                .await?;
            (device_id, node, String::new())
        }
        (None, None) => unreachable!("relogin is computed whenever there is no link token"),
    };
    let session = start_session(&mut tx, &account_id, &device_id, now).await?;
    tx.commit().await?;
    clear_failures(&st, &account_id).await?;

    tracing::info!(account = %account_id, device = %device_id, node, "signed in");
    let mut resp = token_resp(&st, session, device_id, node);
    resp.device_secret = device_secret;
    Ok(Json(resp))
}

async fn recover_login(
    State(st): State<AppState>,
    ip: ClientIp,
    Json(req): Json<RecoverReq>,
) -> ApiResult<Json<TokenResp>> {
    let username = normalize_username(&req.username)?;
    let key = fixed::<32>("recoveryAuthKey", &req.recovery_auth_key)?;
    let account_id = check_credentials(&st, ip, &username, key, Secret::Recovery, false).await?;

    let now = st.now_ms();
    let mut tx = db::begin_write(&st.db).await?;
    let session = start_recovery_session(&mut tx, &account_id, now).await?;
    tx.commit().await?;
    clear_failures(&st, &account_id).await?;

    tracing::info!(account = %account_id, "recovery sign-in");
    // No device and no refresh token: the empty values are part of the
    // contract (see docs/protocol.md).
    Ok(Json(token_resp(&st, session, String::new(), 0)))
}

async fn refresh(
    State(st): State<AppState>,
    Json(req): Json<RefreshReq>,
) -> ApiResult<Json<TokenResp>> {
    let hash = token_hash(&req.refresh_token).ok_or_else(unauthorized)?;
    let now = st.now_ms();
    // A plain read first, so unknown tokens never queue for the write lock.
    let known: Option<i64> = sqlx::query_scalar("SELECT 1 FROM refresh_tokens WHERE hash = ?")
        .bind(&hash)
        .fetch_optional(&st.db)
        .await?;
    if known.is_none() {
        return Err(unauthorized());
    }
    // The write lock is held from here, so two refreshes with the same token
    // cannot both succeed. Everything is read again inside it.
    let mut tx = db::begin_write(&st.db).await?;
    // device, rotated_at, successor, revoked, account, disabled, node, vault
    type Row = (
        String,
        Option<i64>,
        Option<Vec<u8>>,
        i64,
        String,
        i64,
        i64,
        String,
    );
    let row: Option<Row> = sqlx::query_as(
        "SELECT r.device_id, r.rotated_at, r.successor_hash, d.revoked, d.account_id, \
             a.disabled, d.node, a.vault_id \
         FROM refresh_tokens r JOIN devices d ON d.id = r.device_id \
         JOIN accounts a ON a.id = d.account_id WHERE r.hash = ?",
    )
    .bind(&hash)
    .fetch_optional(&mut *tx)
    .await?;
    let (device_id, rotated_at, successor, revoked, account_id, disabled, node, vault_id) =
        row.ok_or_else(unauthorized)?;
    if revoked != 0 {
        return Err(ApiError::of(ErrorCode::Revoked));
    }
    if disabled != 0 {
        return Err(ApiError::new(
            ErrorCode::Forbidden,
            "this account is disabled",
        ));
    }
    if let Some(rotated) = rotated_at {
        let lost_answer = match successor {
            Some(succ) if now - rotated < REFRESH_GRACE_MS => {
                unused_successor(&mut tx, &device_id, &succ).await?
            }
            _ => None,
        };
        let Some(succ) = lost_answer else {
            // An old token again: whoever holds it did not get it from this
            // device's latest exchange. Revoke the device; both parties must
            // sign in anew with an add-device code.
            devices::revoke_device(&mut tx, &account_id, &device_id).await?;
            tx.commit().await?;
            tracing::warn!(account = %account_id, device = %device_id, "refresh token reused: device revoked");
            st.events
                .publish(&vault_id, Target::Only(device_id), Event::Revoked);
            return Err(ApiError::of(ErrorCode::Revoked));
        };
        // The grace: nobody has used what the last exchange issued, so its
        // answer was most likely lost. Invalidate that session and answer
        // again — once (the old token keeps no successor, so a third
        // presentation revokes).
        sqlx::query("DELETE FROM access_tokens WHERE device_id = ? AND refresh_hash = ?")
            .bind(&device_id)
            .bind(&succ)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM refresh_tokens WHERE hash = ?")
            .bind(&succ)
            .execute(&mut *tx)
            .await?;
        let session = start_session(&mut tx, &account_id, &device_id, now).await?;
        sqlx::query("UPDATE refresh_tokens SET successor_hash = NULL WHERE hash = ?")
            .bind(&hash)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        let node = u16::try_from(node).map_err(ApiError::internal)?;
        tracing::info!(account = %account_id, device = %device_id, "tokens refreshed again (lost answer)");
        return Ok(Json(token_resp(&st, session, device_id, node)));
    }
    sqlx::query("UPDATE devices SET last_seen = ? WHERE id = ?")
        .bind(now)
        .bind(&device_id)
        .execute(&mut *tx)
        .await?;
    let session = start_session(&mut tx, &account_id, &device_id, now).await?;
    sqlx::query("UPDATE refresh_tokens SET rotated_at = ?, successor_hash = ? WHERE hash = ?")
        .bind(now)
        .bind(&session.refresh_hash)
        .bind(&hash)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    let node = u16::try_from(node).map_err(ApiError::internal)?;
    tracing::info!(account = %account_id, device = %device_id, "tokens refreshed");
    Ok(Json(token_resp(&st, session, device_id, node)))
}

/// `Some(successor)` when the refresh token `successor` is still live and
/// unused: never exchanged, and no access token issued with it presented.
async fn unused_successor(
    conn: &mut SqliteConnection,
    device_id: &str,
    successor: &[u8],
) -> ApiResult<Option<Vec<u8>>> {
    let rotated: Option<Option<i64>> = sqlx::query_scalar(
        "SELECT rotated_at FROM refresh_tokens WHERE hash = ? AND device_id = ?",
    )
    .bind(successor)
    .bind(device_id)
    .fetch_optional(&mut *conn)
    .await?;
    // A missing successor (signed out, signed in again) counts as used.
    if rotated != Some(None) {
        return Ok(None);
    }
    let used: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM access_tokens WHERE device_id = ? AND refresh_hash = ? AND used != 0",
    )
    .bind(device_id)
    .bind(successor)
    .fetch_one(&mut *conn)
    .await?;
    Ok((used == 0).then(|| successor.to_vec()))
}

/// End this device's session: its access and refresh tokens are deleted. The
/// device stays registered (and listed) until someone revokes it.
async fn logout(State(st): State<AppState>, auth: AuthDevice) -> ApiResult<StatusCode> {
    let mut tx = db::begin_write(&st.db).await?;
    sqlx::query("DELETE FROM access_tokens WHERE device_id = ?")
        .bind(&auth.device_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM refresh_tokens WHERE device_id = ?")
        .bind(&auth.device_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    tracing::info!(account = %auth.account_id, device = %auth.device_id, "signed out");
    Ok(StatusCode::NO_CONTENT)
}

/// Routes of this module.
pub fn routes() -> axum::Router<AppState> {
    axum::Router::new()
        .route("/v1/login", post(login))
        .route("/v1/recover-login", post(recover_login))
        .route("/v1/refresh", post(refresh))
        .route("/v1/logout", post(logout))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_round_trip_and_strictness() {
        let raw = [0xABu8; 32];
        let s = encode_token(&raw);
        assert_eq!(s.len(), 43);
        assert!(!s.contains(['/', '+', '=']));
        assert_eq!(token_hash(&s), Some(Sha256::digest(raw).to_vec()));
        for bad in [
            "",
            "short",
            &s[..42],
            &format!("{s}A"),
            &s.replace(&s[..1], "!"),
        ] {
            assert_eq!(token_hash(bad), None, "{bad}");
        }
        // Non-canonical trailing bits do not alias a valid token.
        // The last character carries two padding bits that must be zero;
        // bumping it by one sets one, giving a non-canonical alias.
        let (head, tail) = s.split_at(42);
        let alias = format!("{head}{}", char::from(tail.as_bytes()[0] + 1));
        assert_eq!(token_hash(&alias), None);
    }
}
