//! Record sync: the change log and the push endpoint.
//!
//! The server stores sealed records and never opens them. It also never picks
//! a winner between two devices' edits: it accepts a write only when the
//! writer saw the server's current revision (`baseRev`), and otherwise hands
//! the current row back so the client can merge and push again.
//!
//! * `GET /v1/sync/changes?since=N&limit=500` pages the vault's log by `seq`.
//! * `POST /v1/sync/push` applies up to 500 items in one `BEGIN IMMEDIATE`
//!   transaction, item by item, in order.

use axum::Json;
use axum::extract::{Query, State};
use axum::routing::{get, post};
use core_proto::{ChangesResp, ErrorCode, Event, PushItem, PushReq, PushResp, PushResult, WireRow};
use serde::Deserialize;
use sqlx::SqliteConnection;

use crate::account::require_write_schema;
use crate::auth::AuthDevice;
use crate::db;
use crate::error::{ApiError, ApiResult};
use crate::state::{AppState, Target};

/// Most items in one push.
pub const MAX_PUSH_ITEMS: usize = 500;
/// Largest envelope, bytes.
pub const MAX_ENVELOPE: usize = 1024 * 1024;
/// Page size when `limit` is not given.
pub const DEFAULT_LIMIT: i64 = 500;
/// Largest page.
pub const MAX_LIMIT: i64 = 1000;
/// How far ahead of the server's clock an HLC wall time may be.
pub const MAX_SKEW_MS: i64 = 60 * 60 * 1000;

/// A stored record, in the column order of the `SELECT`s below: id, kind,
/// key version, hlc, rev, seq, deleted, envelope.
type DbRow = (String, String, i64, String, i64, i64, i64, Vec<u8>);

fn wire(r: DbRow) -> ApiResult<WireRow> {
    let (id, kind, key_version, hlc, rev, seq, deleted, envelope) = r;
    Ok(WireRow {
        id,
        kind,
        key_version: u32::try_from(key_version).map_err(ApiError::internal)?,
        hlc,
        rev: Some(rev),
        seq: Some(seq),
        deleted: deleted != 0,
        envelope,
    })
}

// ------------------------------------------------------------- validation

/// The wall-clock part (Unix milliseconds) of an HLC in its text form
/// `{wall_ms:016x}-{counter:08x}-{node:04x}`. `None` when `hlc` is not exactly
/// that shape (lowercase hex, fixed widths).
pub fn hlc_wall_ms(hlc: &str) -> Option<i64> {
    let b = hlc.as_bytes();
    if b.len() != 16 + 1 + 8 + 1 + 4 || b[16] != b'-' || b[25] != b'-' {
        return None;
    }
    let hex = |s: &[u8]| {
        s.iter()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(c))
    };
    if !(hex(&b[..16]) && hex(&b[17..25]) && hex(&b[26..])) {
        return None;
    }
    // 16 hex digits can exceed i64; such a time is absurd, not an error here.
    Some(i64::try_from(u64::from_str_radix(&hlc[..16], 16).ok()?).unwrap_or(i64::MAX))
}

fn valid_kind(kind: &str) -> bool {
    (1..=32).contains(&kind.len()) && kind.bytes().all(|c| c.is_ascii_lowercase() || c == b'_')
}

fn valid_id(id: &str) -> bool {
    uuid::Uuid::parse_str(id).is_ok_and(|u| u.hyphenated().to_string() == id)
}

/// Check one pushed row; the first problem becomes the error for the whole
/// request.
fn validate_item(item: &PushItem, now: i64) -> ApiResult<()> {
    let r = &item.row;
    if !valid_id(&r.id) {
        return Err(ApiError::bad_request(
            "record id must be a lowercase hyphenated UUID",
        ));
    }
    if !valid_kind(&r.kind) {
        return Err(ApiError::bad_request(
            "record kind must be 1 to 32 characters of a-z and _",
        ));
    }
    if r.key_version == 0 {
        return Err(ApiError::bad_request("keyVersion must be at least 1"));
    }
    if r.envelope.len() > MAX_ENVELOPE {
        return Err(ApiError::bad_request("envelope is larger than 1 MiB"));
    }
    if r.deleted && !r.envelope.is_empty() {
        return Err(ApiError::bad_request("a tombstone carries no envelope"));
    }
    if !r.deleted && r.envelope.is_empty() {
        return Err(ApiError::bad_request("a record needs an envelope"));
    }
    if item.base_rev.is_some_and(|b| b < 1) {
        return Err(ApiError::bad_request("baseRev must be at least 1"));
    }
    let wall = hlc_wall_ms(&r.hlc)
        .ok_or_else(|| ApiError::bad_request("hlc is not in the expected format"))?;
    if wall > now.saturating_add(MAX_SKEW_MS) {
        return Err(ApiError::of(ErrorCode::ClockSkew));
    }
    Ok(())
}

// --------------------------------------------------------------- changes

#[derive(Deserialize)]
struct ChangesQuery {
    since: Option<i64>,
    limit: Option<i64>,
}

/// `GET /v1/sync/changes`: rows with `seq > since`, oldest first.
///
/// `since` of `0` means "everything" and is always served. Any other cursor
/// behind the compaction horizon answers `410 gone`: tombstones it should
/// have seen are gone, so the device downloads everything from `0` and
/// reconciles.
async fn changes(
    State(st): State<AppState>,
    auth: AuthDevice,
    Query(q): Query<ChangesQuery>,
) -> ApiResult<Json<ChangesResp>> {
    let since = q.since.unwrap_or(0);
    if since < 0 {
        return Err(ApiError::bad_request("since must not be negative"));
    }
    let limit = q.limit.unwrap_or(DEFAULT_LIMIT);
    if limit < 1 {
        return Err(ApiError::bad_request("limit must be at least 1"));
    }
    let limit = limit.min(MAX_LIMIT);

    // One read transaction: the horizon and the rows are the same snapshot.
    let mut tx = st.db.begin().await?;
    let horizon: i64 =
        sqlx::query_scalar("SELECT compacted_through FROM vault_seq WHERE vault_id = ?")
            .bind(&auth.vault_id)
            .fetch_optional(&mut *tx)
            .await?
            .unwrap_or(0);
    if since != 0 && since < horizon {
        return Err(ApiError::new(
            ErrorCode::Gone,
            "this device is too far behind the server; it must download everything again",
        ));
    }
    let rows: Vec<DbRow> = sqlx::query_as(
        "SELECT id, kind, key_version, hlc, rev, seq, deleted, envelope FROM records \
         WHERE vault_id = ? AND seq > ? ORDER BY seq LIMIT ?",
    )
    .bind(&auth.vault_id)
    .bind(since)
    // One more than the page, to learn whether anything remains.
    .bind(limit + 1)
    .fetch_all(&mut *tx)
    .await?;
    drop(tx);

    let more = i64::try_from(rows.len()).map_or(true, |n| n > limit);
    let take = usize::try_from(limit).unwrap_or(usize::MAX);
    let changes = rows
        .into_iter()
        .take(take)
        .map(wire)
        .collect::<ApiResult<Vec<_>>>()?;
    let next = changes.last().and_then(|r| r.seq).unwrap_or(since);
    Ok(Json(ChangesResp {
        changes,
        next,
        more,
        epoch: st.epoch.clone(),
    }))
}

// ------------------------------------------------------------------ push

/// Apply one item against the vault. `head` is the vault's last assigned seq
/// and moves forward when the write is accepted.
async fn apply(
    conn: &mut SqliteConnection,
    vault_id: &str,
    item: &PushItem,
    head: &mut i64,
    now: i64,
) -> ApiResult<PushResult> {
    let r = &item.row;
    let current: Option<DbRow> = sqlx::query_as(
        "SELECT id, kind, key_version, hlc, rev, seq, deleted, envelope FROM records \
         WHERE vault_id = ? AND id = ?",
    )
    .bind(vault_id)
    .bind(&r.id)
    .fetch_optional(&mut *conn)
    .await?;

    let new_rev = match (&current, item.base_rev) {
        // Never seen: accepted as a new record.
        (None, None) => Some(1),
        // The writer saw the current revision.
        (Some(c), Some(base)) if c.4 == base => Some(base + 1),
        // The writer builds on a record the server does not have: its view is
        // ahead of (or otherwise not part of) this server's history, for
        // example after a restore from an older backup, or the tombstone was
        // compacted. Either way the answer is a full resync.
        (None, Some(_)) => {
            return Err(ApiError::new(
                ErrorCode::Gone,
                "this device is too far behind the server; it must download everything again",
            ));
        }
        _ => None,
    };

    if let Some(rev) = new_rev {
        *head += 1;
        sqlx::query(
            "INSERT INTO records \
               (vault_id, id, kind, key_version, hlc, rev, seq, deleted, envelope, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (vault_id, id) DO UPDATE SET \
               kind = excluded.kind, key_version = excluded.key_version, hlc = excluded.hlc, \
               rev = excluded.rev, seq = excluded.seq, deleted = excluded.deleted, \
               envelope = excluded.envelope, updated_at = excluded.updated_at",
        )
        .bind(vault_id)
        .bind(&r.id)
        .bind(&r.kind)
        .bind(i64::from(r.key_version))
        .bind(&r.hlc)
        .bind(rev)
        .bind(*head)
        .bind(i64::from(r.deleted))
        .bind(&r.envelope)
        .bind(now)
        .execute(&mut *conn)
        .await?;
        return Ok(PushResult::Ok {
            id: r.id.clone(),
            rev,
            seq: *head,
        });
    }

    // Not accepted, so the record exists. If it is exactly this write already
    // (the response to an earlier attempt was lost), say so rather than
    // conflicting with the writer's own change.
    let c = current.ok_or_else(|| ApiError::internal("conflict without a current row"))?;
    if c.3 == r.hlc && c.7 == r.envelope && (c.6 != 0) == r.deleted {
        return Ok(PushResult::Ok {
            id: c.0,
            rev: c.4,
            seq: c.5,
        });
    }
    Ok(PushResult::Conflict {
        id: r.id.clone(),
        current: wire(c)?,
    })
}

/// Bytes of sealed records `vault_id` holds (tombstones have empty envelopes).
async fn vault_bytes(conn: &mut SqliteConnection, vault_id: &str) -> ApiResult<i64> {
    Ok(sqlx::query_scalar(
        "SELECT COALESCE(SUM(length(envelope)), 0) FROM records WHERE vault_id = ?",
    )
    .bind(vault_id)
    .fetch_one(&mut *conn)
    .await?)
}

/// `POST /v1/sync/push`.
async fn push(
    State(st): State<AppState>,
    auth: AuthDevice,
    Json(req): Json<PushReq>,
) -> ApiResult<Json<PushResp>> {
    if req.items.len() > MAX_PUSH_ITEMS {
        return Err(ApiError::bad_request("a push carries at most 500 items"));
    }
    let now = st.now_ms();
    for item in &req.items {
        validate_item(item, now)?;
    }

    let mut tx = db::begin_write(&st.db).await?;
    require_write_schema(&mut tx, &auth.account_id, auth.schema).await?;
    sqlx::query("INSERT OR IGNORE INTO vault_seq (vault_id) VALUES (?)")
        .bind(&auth.vault_id)
        .execute(&mut *tx)
        .await?;
    let mut head: i64 = sqlx::query_scalar("SELECT head FROM vault_seq WHERE vault_id = ?")
        .bind(&auth.vault_id)
        .fetch_one(&mut *tx)
        .await?;
    let start = head;

    let quota = i64::try_from(st.cfg.vault_quota_bytes).unwrap_or(i64::MAX);
    let used_before = if quota > 0 {
        vault_bytes(&mut tx, &auth.vault_id).await?
    } else {
        0
    };

    let mut results = Vec::with_capacity(req.items.len());
    for item in &req.items {
        results.push(apply(&mut tx, &auth.vault_id, item, &mut head, now).await?);
    }
    if quota > 0 {
        // Only growth is refused, so deleting records always gets a full
        // vault back under the limit. Returning drops (rolls back) `tx`.
        let used = vault_bytes(&mut tx, &auth.vault_id).await?;
        if used > quota && used > used_before {
            tracing::warn!(account = %auth.account_id, used, quota, "sync push over the vault quota");
            return Err(ApiError::new(
                ErrorCode::Forbidden,
                format!(
                    "this vault is full: the server allows {} MiB of synced data; \
                     delete some items or ask the server's operator to raise the limit",
                    quota >> 20
                ),
            ));
        }
    }
    if head != start {
        sqlx::query("UPDATE vault_seq SET head = ? WHERE vault_id = ?")
            .bind(head)
            .bind(&auth.vault_id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;

    let accepted = results
        .iter()
        .filter(|r| matches!(r, PushResult::Ok { .. }))
        .count();
    tracing::info!(
        account = %auth.account_id,
        device = %auth.device_id,
        items = req.items.len(),
        accepted,
        conflicts = results.len() - accepted,
        head,
        "sync push"
    );
    // Everyone but the pusher learns the log moved. A retry of an already
    // applied write also announces: the first attempt's announcement may have
    // been lost along with its response, and an extra one costs a cheap pull.
    if accepted > 0 {
        st.events.publish(
            &auth.vault_id,
            Target::Except(auth.device_id.clone()),
            Event::Changed { seq: head },
        );
    }
    Ok(Json(PushResp { results }))
}

/// Routes of this module.
pub fn routes() -> axum::Router<AppState> {
    axum::Router::new()
        .route("/v1/sync/changes", get(changes))
        .route("/v1/sync/push", post(push))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hlc_wall_part_is_parsed_strictly() {
        assert_eq!(
            hlc_wall_ms("0000019a2b3c4d5e-00000001-0002"),
            Some(0x19a2b3c4d5e)
        );
        assert_eq!(hlc_wall_ms("0000000000000000-00000000-0000"), Some(0));
        for bad in [
            "",
            "0000019a2b3c4d5e-00000001-000",
            "0000019a2b3c4d5e-00000001-00022",
            "0000019A2B3C4D5E-00000001-0002",
            "0000019a2b3c4d5e_00000001-0002",
            "0000019a2b3c4d5e-0000000g-0002",
            "+000019a2b3c4d5e-00000001-0002",
        ] {
            assert_eq!(hlc_wall_ms(bad), None, "{bad}");
        }
        // Beyond i64 is a huge time, which the skew rule then refuses.
        assert_eq!(
            hlc_wall_ms("ffffffffffffffff-00000000-0000"),
            Some(i64::MAX)
        );
    }

    #[test]
    fn kinds_and_ids() {
        assert!(valid_kind("host") && valid_kind("known_host") && valid_kind("a"));
        assert!(!valid_kind("") && !valid_kind("Host") && !valid_kind("a-b"));
        assert!(!valid_kind(&"a".repeat(33)) && valid_kind(&"a".repeat(32)));
        assert!(valid_id("0192f0aa-1b2c-7d3e-8f40-0123456789ab"));
        assert!(!valid_id("0192F0AA-1B2C-7D3E-8F40-0123456789AB"));
        assert!(!valid_id("0192f0aa1b2c7d3e8f400123456789ab"));
        assert!(!valid_id("x"));
    }
}
