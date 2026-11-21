//! Tombstone compaction.
//!
//! A deleted record stays in `records` as a tombstone so that devices that
//! were offline learn about the deletion. After [`KEEP_DAYS`] days the
//! tombstone is removed and the vault's `compacted_through` rises to the
//! highest `seq` removed: a device whose cursor is behind that point can no
//! longer be given a complete log and is answered `410 gone` (it then
//! downloads everything again). Live records are never touched.

use std::time::Duration;

use sqlx::SqlitePool;
use tokio::task::JoinHandle;

use crate::db;
use crate::state::Clock;

/// How long a tombstone is kept before it may be compacted.
pub const KEEP_DAYS: i64 = 180;

/// How often the background job runs.
const EVERY: Duration = Duration::from_secs(24 * 60 * 60);

const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// Remove the tombstones whose `updated_at` is more than `keep_days` before
/// `now` (Unix milliseconds) and raise each affected vault's
/// `compacted_through` to the highest `seq` removed (it never goes down).
/// Returns the number of rows removed.
pub async fn compact(db: &SqlitePool, now: i64, keep_days: i64) -> Result<u64, sqlx::Error> {
    let cutoff = now.saturating_sub(keep_days.saturating_mul(DAY_MS));
    let mut tx = db::begin_write(db).await?;
    // The horizon first (it reads the rows the delete below removes).
    sqlx::query(
        "UPDATE vault_seq SET compacted_through = MAX(compacted_through, COALESCE( \
           (SELECT MAX(r.seq) FROM records r \
            WHERE r.vault_id = vault_seq.vault_id AND r.deleted = 1 AND r.updated_at < ?1), 0))",
    )
    .bind(cutoff)
    .execute(&mut *tx)
    .await?;
    let n = sqlx::query("DELETE FROM records WHERE deleted = 1 AND updated_at < ?")
        .bind(cutoff)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    tx.commit().await?;
    Ok(n)
}

/// Compact once now, then once a day until the returned task is aborted. A
/// failure is logged and retried at the next interval; it never stops the
/// server.
pub async fn spawn(db: SqlitePool, clock: Clock) -> JoinHandle<()> {
    run_once(&db, &clock).await;
    tokio::spawn(async move {
        let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + EVERY, EVERY);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            run_once(&db, &clock).await;
        }
    })
}

async fn run_once(db: &SqlitePool, clock: &Clock) {
    match compact(db, clock(), KEEP_DAYS).await {
        Ok(0) => {}
        Ok(n) => tracing::info!(removed = n, "compacted old tombstones"),
        Err(e) => tracing::warn!(error = %e, "tombstone compaction failed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn put(db: &SqlitePool, vault: &str, id: &str, seq: i64, deleted: i64, at: i64) {
        sqlx::query(
            "INSERT INTO records (vault_id, id, kind, key_version, hlc, rev, seq, deleted, envelope, updated_at) \
             VALUES (?, ?, 'host', 1, 'h', 1, ?, ?, x'00', ?)",
        )
        .bind(vault)
        .bind(id)
        .bind(seq)
        .bind(deleted)
        .bind(at)
        .execute(db)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO vault_seq (vault_id, head) VALUES (?1, ?2) \
             ON CONFLICT(vault_id) DO UPDATE SET head = MAX(head, ?2)",
        )
        .bind(vault)
        .bind(seq)
        .execute(db)
        .await
        .unwrap();
    }

    async fn horizon(db: &SqlitePool, vault: &str) -> i64 {
        sqlx::query_scalar("SELECT compacted_through FROM vault_seq WHERE vault_id = ?")
            .bind(vault)
            .fetch_one(db)
            .await
            .unwrap()
    }

    async fn ids(db: &SqlitePool, vault: &str) -> Vec<String> {
        sqlx::query_scalar("SELECT id FROM records WHERE vault_id = ? ORDER BY seq")
            .bind(vault)
            .fetch_all(db)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn removes_only_old_tombstones_and_raises_the_horizon() {
        let dir = tempfile::tempdir().unwrap();
        let db = db::open(dir.path()).await.unwrap();
        let now = 400 * DAY_MS;
        put(&db, "v", "live-old", 1, 0, now - 300 * DAY_MS).await;
        put(&db, "v", "dead-old-a", 2, 1, now - 200 * DAY_MS).await;
        put(&db, "v", "dead-recent", 3, 1, now - 10 * DAY_MS).await;
        put(&db, "v", "dead-old-b", 4, 1, now - 181 * DAY_MS).await;
        put(&db, "v", "dead-edge", 5, 1, now - 180 * DAY_MS).await;
        put(&db, "w", "dead-recent-w", 1, 1, now - DAY_MS).await;

        let n = compact(&db, now, 180).await.unwrap();
        assert_eq!(n, 2);
        assert_eq!(
            ids(&db, "v").await,
            vec!["live-old", "dead-recent", "dead-edge"]
        );
        assert_eq!(horizon(&db, "v").await, 4);
        // A vault with nothing to compact keeps its horizon.
        assert_eq!(horizon(&db, "w").await, 0);

        // Idempotent, and the horizon never goes back down.
        assert_eq!(compact(&db, now, 180).await.unwrap(), 0);
        assert_eq!(horizon(&db, "v").await, 4);
        // Later, the rest ages out.
        assert_eq!(compact(&db, now + 200 * DAY_MS, 180).await.unwrap(), 3);
        assert_eq!(ids(&db, "v").await, vec!["live-old"]);
        assert_eq!(horizon(&db, "v").await, 5);
        assert_eq!(horizon(&db, "w").await, 1);
    }

    #[tokio::test]
    async fn runs_on_start() {
        let dir = tempfile::tempdir().unwrap();
        let db = db::open(dir.path()).await.unwrap();
        put(&db, "v", "dead", 1, 1, 0).await;
        let clock: Clock = std::sync::Arc::new(|| 400 * DAY_MS);
        let task = spawn(db.clone(), clock).await;
        // The first pass has finished before `spawn` returns.
        assert!(ids(&db, "v").await.is_empty());
        assert_eq!(horizon(&db, "v").await, 1);
        task.abort();
    }
}
