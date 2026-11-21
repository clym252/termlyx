//! The account's key material: serving it, changing the master password,
//! resetting it with the recovery key, replacing the recovery key, and the
//! schema gate that keeps old clients from writing.
//!
//! The server never sees a password or a key. A password change swaps four
//! opaque values in one transaction: the AuthKey verifier (fresh salt), the KDF
//! parameters, and `secret_by_kek`, and bumps `account_rev`. The bump is what
//! other devices notice: they get [`Event::AccountChanged`] and fetch the new
//! material. Their sessions stay valid (they are the user's own devices).

use axum::Json;
use axum::extract::State;
use axum::routing::{get, post, put};
use core_proto::{
    AccountMaterial, AccountResp, ErrorCode, Event, KdfWire, PasswordChangeReq, RecoveryResetReq,
    RecoveryUpdateReq,
};
use sqlx::SqliteConnection;

use crate::accounts::{fixed, new_verifier, validate_kdf, validate_wrapped_secret};
use crate::auth::{self, AnyAuth, AuthDevice, RecoveryAuth, Secret};
use crate::db;
use crate::error::{ApiError, ApiResult};
use crate::state::{AppState, Target};

// ---------------------------------------------------------- schema gating

/// Refuse a write from a client whose record kind set is older than the newest
/// one any device of the account has used: `400 update_client` ("update
/// Tether"). Reads are never gated.
///
/// The extractors ([`AuthDevice`], [`RecoveryAuth`]) already raised the
/// account's `max_schema` to this request's `X-Tether-Schema` (1 when the
/// header is missing), so `client_schema` is `auth.schema`. Every write
/// handler calls this, ideally again inside its write transaction (`conn` may
/// be one) so the check and the write are atomic.
pub async fn require_write_schema(
    conn: &mut SqliteConnection,
    account_id: &str,
    client_schema: u32,
) -> ApiResult<()> {
    let max: i64 = sqlx::query_scalar("SELECT max_schema FROM accounts WHERE id = ?")
        .bind(account_id)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(|| ApiError::of(ErrorCode::Unauthorized))?;
    if i64::from(client_schema) < max {
        return Err(ApiError::of(ErrorCode::UpdateClient));
    }
    Ok(())
}

/// [`require_write_schema`] on a pooled connection, for the cheap early check
/// before any Argon2 work or lockout bookkeeping.
async fn early_schema_check(st: &AppState, account_id: &str, schema: u32) -> ApiResult<()> {
    let mut conn = st.db.acquire().await?;
    require_write_schema(&mut conn, account_id, schema).await
}

// -------------------------------------------------------------- material

/// The account's material and `account_rev`, exactly as stored.
async fn load(conn: &mut SqliteConnection, account_id: &str) -> ApiResult<AccountResp> {
    type Row = (
        String,
        i64,
        i64,
        i64,
        i64,
        Vec<u8>,
        Vec<u8>,
        Vec<u8>,
        Vec<u8>,
        Vec<u8>,
        i64,
    );
    let (
        vault_id,
        key_version,
        m_kib,
        t,
        p,
        salt,
        account_public,
        secret_by_kek,
        secret_by_recovery,
        sealed_vault_key,
        account_rev,
    ): Row = sqlx::query_as(
        "SELECT vault_id, key_version, kdf_m_kib, kdf_t, kdf_p, kdf_salt, account_public, \
         secret_by_kek, secret_by_recovery, sealed_vault_key, account_rev \
         FROM accounts WHERE id = ?",
    )
    .bind(account_id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(|| ApiError::of(ErrorCode::Unauthorized))?;
    Ok(AccountResp {
        material: AccountMaterial {
            vault_id,
            key_version: u32::try_from(key_version).map_err(ApiError::internal)?,
            kdf: KdfWire {
                m_kib: u32::try_from(m_kib).map_err(ApiError::internal)?,
                t: u32::try_from(t).map_err(ApiError::internal)?,
                p: u32::try_from(p).map_err(ApiError::internal)?,
                salt,
            },
            account_public,
            secret_by_kek,
            secret_by_recovery,
            sealed_vault_key,
        },
        account_rev,
    })
}

/// `GET /v1/account`: a device, or the recovery token (the client needs
/// `secretByRecovery` to recover), reads the key material.
async fn get_account(State(st): State<AppState>, auth: AnyAuth) -> ApiResult<Json<AccountResp>> {
    let mut conn = st.db.acquire().await?;
    Ok(Json(load(&mut conn, &auth.account_id).await?))
}

// -------------------------------------------------------------- password

/// Swap in a new AuthKey verifier, KDF and KEK wrap and bump `account_rev`,
/// provided the revision is still `expected_rev`. Runs inside the caller's
/// write transaction.
async fn set_password(
    conn: &mut SqliteConnection,
    account_id: &str,
    expected_rev: i64,
    verifier: (Vec<u8>, Vec<u8>),
    kdf: &KdfWire,
    secret_by_kek: &[u8],
) -> ApiResult<()> {
    let n = sqlx::query(
        "UPDATE accounts SET verifier = ?, verifier_salt = ?, kdf_m_kib = ?, kdf_t = ?, \
         kdf_p = ?, kdf_salt = ?, secret_by_kek = ?, account_rev = account_rev + 1 \
         WHERE id = ? AND account_rev = ?",
    )
    .bind(verifier.0)
    .bind(verifier.1)
    .bind(i64::from(kdf.m_kib))
    .bind(i64::from(kdf.t))
    .bind(i64::from(kdf.p))
    .bind(&kdf.salt)
    .bind(secret_by_kek)
    .bind(account_id)
    .bind(expected_rev)
    .execute(&mut *conn)
    .await?
    .rows_affected();
    if n != 1 {
        return Err(conflict());
    }
    Ok(())
}

fn conflict() -> ApiError {
    ApiError::new(
        ErrorCode::Conflict,
        "the account changed on another device; refresh and try again",
    )
}

/// `POST /v1/account/password`.
async fn change_password(
    State(st): State<AppState>,
    auth: AuthDevice,
    Json(req): Json<PasswordChangeReq>,
) -> ApiResult<Json<AccountResp>> {
    // Cheap checks first: nothing malformed costs Argon2 work or a failure
    // point.
    let old_key = fixed::<32>("oldAuthKey", &req.old_auth_key)?;
    let new_key = fixed::<32>("newAuthKey", &req.new_auth_key)?;
    validate_kdf(&req.kdf)?;
    validate_wrapped_secret("secretByKek", &req.secret_by_kek)?;
    early_schema_check(&st, &auth.account_id, auth.schema).await?;

    // The same lockout path as a sign-in. The verifier and `account_rev` are
    // read together and a base that is not that revision is a conflict, so the
    // key is checked against the verifier of exactly the revision the client
    // names, never a dead key against a newer one.
    auth::verify_account_key(
        &st,
        &auth.account_id,
        old_key,
        Secret::Auth,
        Some(req.account_rev_base),
    )
    .await?;
    let verifier = new_verifier(new_key.to_vec()).await?;

    let mut tx = db::begin_write(&st.db).await?;
    require_write_schema(&mut tx, &auth.account_id, auth.schema).await?;
    // Any change of the verifier bumps the revision, so requiring the base here
    // proves nothing changed since the key was checked.
    set_password(
        &mut tx,
        &auth.account_id,
        req.account_rev_base,
        verifier,
        &req.kdf,
        &req.secret_by_kek,
    )
    .await?;
    auth::clear_failures_in(&mut tx, &auth.account_id).await?;
    let resp = load(&mut tx, &auth.account_id).await?;
    tx.commit().await?;

    tracing::info!(account = %auth.account_id, device = %auth.device_id, account_rev = resp.account_rev, "password changed");
    st.events.publish(
        &auth.vault_id,
        Target::All,
        Event::AccountChanged {
            account_rev: resp.account_rev,
        },
    );
    Ok(Json(resp))
}

/// `POST /v1/account/recover`: the recovery-scoped token plus the recovery
/// auth key reset the password. The token is spent.
async fn recover(
    State(st): State<AppState>,
    auth: RecoveryAuth,
    Json(req): Json<RecoveryResetReq>,
) -> ApiResult<Json<AccountResp>> {
    let recovery_key = fixed::<32>("recoveryAuthKey", &req.recovery_auth_key)?;
    let new_key = fixed::<32>("newAuthKey", &req.new_auth_key)?;
    validate_kdf(&req.kdf)?;
    validate_wrapped_secret("secretByKek", &req.secret_by_kek)?;
    early_schema_check(&st, &auth.account_id, auth.schema).await?;

    // The bearer token is not enough: the body must prove the recovery key too.
    // The returned revision is the one the verifier was read at; requiring it
    // below means a recovery key replaced in between (which bumps the
    // revision) is a conflict, not a reset on the strength of a dead key.
    let rev = auth::verify_account_key(&st, &auth.account_id, recovery_key, Secret::Recovery, None)
        .await?;
    let verifier = new_verifier(new_key.to_vec()).await?;

    let mut tx = db::begin_write(&st.db).await?;
    require_write_schema(&mut tx, &auth.account_id, auth.schema).await?;
    set_password(
        &mut tx,
        &auth.account_id,
        rev,
        verifier,
        &req.kdf,
        &req.secret_by_kek,
    )
    .await?;
    // Spend the recovery token(s): they have no device.
    sqlx::query("DELETE FROM access_tokens WHERE account_id = ? AND device_id IS NULL")
        .bind(&auth.account_id)
        .execute(&mut *tx)
        .await?;
    auth::clear_failures_in(&mut tx, &auth.account_id).await?;
    let resp = load(&mut tx, &auth.account_id).await?;
    tx.commit().await?;

    tracing::info!(account = %auth.account_id, account_rev = resp.account_rev, "password reset with the recovery key");
    st.events.publish(
        &auth.vault_id,
        Target::All,
        Event::AccountChanged {
            account_rev: resp.account_rev,
        },
    );
    Ok(Json(resp))
}

// -------------------------------------------------------------- recovery

/// `PUT /v1/account/recovery`: a signed-in device replaces the recovery key
/// (new verifier with a fresh salt, new recovery wrap). Bumps `account_rev` so
/// the other devices fetch the new wrap.
async fn update_recovery(
    State(st): State<AppState>,
    auth: AuthDevice,
    Json(req): Json<RecoveryUpdateReq>,
) -> ApiResult<Json<AccountResp>> {
    let auth_key = fixed::<32>("authKey", &req.auth_key)?;
    let key = fixed::<32>("recoveryAuthKey", &req.recovery_auth_key)?;
    validate_wrapped_secret("secretByRecovery", &req.secret_by_recovery)?;
    early_schema_check(&st, &auth.account_id, auth.schema).await?;
    // A session token alone must not be able to swap the recovery key: the
    // master password is proven too (sign-in rules, lockout included).
    let rev = auth::verify_account_key(&st, &auth.account_id, auth_key, Secret::Auth, None).await?;
    let (hash, salt) = new_verifier(key.to_vec()).await?;

    let mut tx = db::begin_write(&st.db).await?;
    require_write_schema(&mut tx, &auth.account_id, auth.schema).await?;
    let n = sqlx::query(
        "UPDATE accounts SET recovery_verifier = ?, recovery_verifier_salt = ?, \
         secret_by_recovery = ?, account_rev = account_rev + 1 WHERE id = ? AND account_rev = ?",
    )
    .bind(hash)
    .bind(salt)
    .bind(&req.secret_by_recovery)
    .bind(&auth.account_id)
    .bind(rev)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if n != 1 {
        return Err(conflict());
    }
    // Recovery sessions earned with the old key die with it.
    sqlx::query("DELETE FROM access_tokens WHERE account_id = ? AND device_id IS NULL")
        .bind(&auth.account_id)
        .execute(&mut *tx)
        .await?;
    auth::clear_failures_in(&mut tx, &auth.account_id).await?;
    let resp = load(&mut tx, &auth.account_id).await?;
    tx.commit().await?;

    tracing::info!(account = %auth.account_id, device = %auth.device_id, account_rev = resp.account_rev, "recovery key replaced");
    st.events.publish(
        &auth.vault_id,
        Target::All,
        Event::AccountChanged {
            account_rev: resp.account_rev,
        },
    );
    Ok(Json(resp))
}

/// Routes of this module.
pub fn routes() -> axum::Router<AppState> {
    axum::Router::new()
        .route("/v1/account", get(get_account))
        .route("/v1/account/password", post(change_password))
        .route("/v1/account/recover", post(recover))
        .route("/v1/account/recovery", put(update_recovery))
}
