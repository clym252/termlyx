//! Devices: creation with HLC node ids, list, rename, revoke, and the
//! add-device link codes.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, patch, post};
use core_proto::{CodeKind, DeviceView, ErrorCode, Event, JoinCode, LinkResp, RenameDeviceReq};
use sha2::{Digest, Sha256};
use sqlx::SqliteConnection;

use crate::accounts::validate_label;
use crate::auth::AuthDevice;
use crate::error::{ApiError, ApiResult};
use crate::state::{AppState, Target};
use crate::util::random_bytes;

/// How long an add-device link code works.
pub const LINK_TTL_MS: i64 = 10 * 60 * 1000;

/// The lowest unused node id (>= 1) among all devices of the account,
/// revoked ones included: their HLCs may still exist in synced data, so their
/// ids stay reserved. The `UNIQUE (account_id, node)` constraint is the final
/// guard; callers run inside a [`crate::db::begin_write`] transaction.
async fn lowest_free_node(conn: &mut SqliteConnection, account_id: &str) -> ApiResult<u16> {
    let used: Vec<i64> =
        sqlx::query_scalar("SELECT node FROM devices WHERE account_id = ? ORDER BY node")
            .bind(account_id)
            .fetch_all(&mut *conn)
            .await?;
    let mut next: i64 = 1;
    for n in used {
        match n.cmp(&next) {
            std::cmp::Ordering::Equal => next += 1,
            std::cmp::Ordering::Greater => break,
            std::cmp::Ordering::Less => {} // node 0, or already passed
        }
    }
    u16::try_from(next).map_err(|_| {
        ApiError::new(
            ErrorCode::Forbidden,
            "this account has used every device id; remove old devices",
        )
    })
}

/// A device just created.
pub struct NewDevice {
    /// Device id (UUID string).
    pub id: String,
    /// Assigned HLC node id.
    pub node: u16,
    /// The per-device secret in client form (base64url, 43 characters). Only its
    /// SHA-256 is stored; this is the one time it exists in the clear.
    pub secret: String,
}

/// Insert a device and assign its HLC node: `hint` if given and free (the
/// first device keeps the node it already writes with), otherwise the lowest
/// unused id >= 1. Also mints the device secret a password re-login needs.
pub async fn create_device(
    conn: &mut SqliteConnection,
    account_id: &str,
    name: &str,
    platform: &str,
    hint: Option<u16>,
    now: i64,
) -> ApiResult<NewDevice> {
    let hinted = match hint {
        Some(h) => {
            let taken: Option<i64> =
                sqlx::query_scalar("SELECT 1 FROM devices WHERE account_id = ? AND node = ?")
                    .bind(account_id)
                    .bind(i64::from(h))
                    .fetch_optional(&mut *conn)
                    .await?;
            taken.is_none().then_some(h)
        }
        None => None,
    };
    let node = match hinted {
        Some(h) => h,
        None => lowest_free_node(conn, account_id).await?,
    };
    let id = uuid::Uuid::now_v7().to_string();
    let secret = random_bytes::<32>();
    sqlx::query(
        "INSERT INTO devices (id, account_id, name, platform, node, created_at, last_seen, secret_hash) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(account_id)
    .bind(name)
    .bind(platform)
    .bind(i64::from(node))
    .bind(now)
    .bind(now)
    .bind(Sha256::digest(secret).to_vec())
    .execute(&mut *conn)
    .await?;
    Ok(NewDevice {
        id,
        node,
        secret: crate::auth::encode_token(&secret),
    })
}

async fn list(State(st): State<AppState>, auth: AuthDevice) -> ApiResult<Json<Vec<DeviceView>>> {
    let rows: Vec<(String, String, String, i64, i64)> = sqlx::query_as(
        "SELECT id, name, platform, created_at, last_seen FROM devices \
         WHERE account_id = ? AND revoked = 0 ORDER BY created_at, id",
    )
    .bind(&auth.account_id)
    .fetch_all(&st.db)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|(id, name, platform, created_at, last_seen)| DeviceView {
                current: id == auth.device_id,
                id,
                name,
                platform,
                created_at,
                last_seen,
            })
            .collect(),
    ))
}

async fn rename(
    State(st): State<AppState>,
    auth: AuthDevice,
    Path(id): Path<String>,
    Json(req): Json<RenameDeviceReq>,
) -> ApiResult<StatusCode> {
    let name = validate_label("device name", &req.name, 100)?;
    let n =
        sqlx::query("UPDATE devices SET name = ? WHERE id = ? AND account_id = ? AND revoked = 0")
            .bind(&name)
            .bind(&id)
            .bind(&auth.account_id)
            .execute(&st.db)
            .await?
            .rows_affected();
    if n == 0 {
        return Err(ApiError::of(ErrorCode::NotFound));
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Mark `device_id` revoked. Its tokens stay in the database on purpose:
/// they keep resolving to the device so its next call is answered "device
/// revoked" rather than a bare "sign in again", and none of them is accepted
/// any more. Returns false if the account has no such device.
pub async fn revoke_device(
    conn: &mut SqliteConnection,
    account_id: &str,
    device_id: &str,
) -> ApiResult<bool> {
    let n = sqlx::query("UPDATE devices SET revoked = 1 WHERE id = ? AND account_id = ?")
        .bind(device_id)
        .bind(account_id)
        .execute(&mut *conn)
        .await?
        .rows_affected();
    Ok(n == 1)
}

async fn revoke(
    State(st): State<AppState>,
    auth: AuthDevice,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    let mut conn = st.db.acquire().await?;
    if !revoke_device(&mut conn, &auth.account_id, &id).await? {
        return Err(ApiError::of(ErrorCode::NotFound));
    }
    tracing::info!(account = %auth.account_id, by = %auth.device_id, device = %id, "device revoked");
    // Tell the revoked device's open socket (if any); it closes after this.
    st.events
        .publish(&auth.vault_id, Target::Only(id), Event::Revoked);
    Ok(StatusCode::NO_CONTENT)
}

/// Store a new 10 minute link token for `account_id` (only its SHA-256), after
/// tidying the account's spent and expired ones. Returns `(token, expires_at)`.
pub async fn create_link_token(
    db: &sqlx::SqlitePool,
    account_id: &str,
    now: i64,
) -> ApiResult<([u8; 32], i64)> {
    let token = random_bytes::<32>();
    let expires_at = now + LINK_TTL_MS;
    let mut tx = crate::db::begin_write(db).await?;
    sqlx::query(
        "DELETE FROM link_tokens WHERE account_id = ? AND (used_at IS NOT NULL OR expires_at <= ?)",
    )
    .bind(account_id)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    sqlx::query("INSERT INTO link_tokens (hash, account_id, expires_at) VALUES (?, ?, ?)")
        .bind(Sha256::digest(token).to_vec())
        .bind(account_id)
        .bind(expires_at)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok((token, expires_at))
}

async fn create_link(State(st): State<AppState>, auth: AuthDevice) -> ApiResult<Json<LinkResp>> {
    let (token, expires_at) = create_link_token(&st.db, &auth.account_id, st.now_ms()).await?;
    let code = JoinCode {
        kind: CodeKind::Link,
        url: st
            .cfg
            .public_url
            .clone()
            .unwrap_or_else(|| "https://localhost".into()), // the client rewrites it
        pin: st.code_pin,
        token,
    }
    .encode();
    tracing::info!(account = %auth.account_id, device = %auth.device_id, "add-device code created");
    Ok(Json(LinkResp { code, expires_at }))
}

/// Routes of this module.
pub fn routes() -> axum::Router<AppState> {
    axum::Router::new()
        .route("/v1/devices", get(list))
        .route("/v1/devices/{id}", patch(rename).delete(revoke))
        .route("/v1/link", post(create_link))
}
