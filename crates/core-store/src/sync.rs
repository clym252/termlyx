//! Sync support: the `sync_state` table, the outbox queries and the inbound
//! queue. Nothing here decrypts; envelopes pass through as opaque bytes.

use core_model::{Hlc, Id};
use rusqlite::{OptionalExtension, params};

use crate::repo::{COLUMNS, RecordRow, parse_row};
use crate::{Store, StoreError};

/// A pulled server row waiting in the inbound queue. `kind` is the raw string
/// from the server (it may name a kind this build does not know).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboundRow {
    /// Server sequence number; the queue's primary key and ordering.
    pub seq: i64,
    pub id: Id,
    pub kind: String,
    pub key_version: u32,
    pub hlc: Hlc,
    /// Server revision of this version.
    pub rev: i64,
    pub deleted: bool,
    pub envelope: Vec<u8>,
}

fn bad(column: &str, text: &str) -> StoreError {
    StoreError::BadRow(format!("invalid {column}: {text:?}"))
}

const INBOUND_COLUMNS: &str = "seq, id, kind, key_version, hlc, rev, deleted, envelope";

fn parse_inbound(r: &rusqlite::Row<'_>) -> Result<InboundRow, StoreError> {
    let id: String = r.get(1)?;
    let key_version: i64 = r.get(3)?;
    let hlc: String = r.get(4)?;
    Ok(InboundRow {
        seq: r.get(0)?,
        id: Id::parse_str(&id).map_err(|_| bad("id", &id))?,
        kind: r.get(2)?,
        key_version: u32::try_from(key_version)
            .map_err(|_| StoreError::BadRow(format!("invalid key_version: {key_version}")))?,
        hlc: hlc.parse().map_err(|_| bad("hlc", &hlc))?,
        rev: r.get(5)?,
        deleted: r.get::<_, i64>(6)? != 0,
        envelope: r.get(7)?,
    })
}

impl Store {
    /// Returns the sync-state value stored under `k`, if any.
    pub fn sync_get(&self, k: &str) -> Result<Option<Vec<u8>>, StoreError> {
        Ok(self
            .conn
            .query_row("SELECT v FROM sync_state WHERE k = ?1", params![k], |r| {
                r.get(0)
            })
            .optional()?)
    }

    /// Stores `v` under `k`, replacing any previous value.
    pub fn sync_put(&self, k: &str, v: &[u8]) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO sync_state (k, v) VALUES (?1, ?2) \
             ON CONFLICT(k) DO UPDATE SET v = excluded.v",
            params![k, v],
        )?;
        Ok(())
    }

    /// Removes the value stored under `k`; a missing key is not an error.
    pub fn sync_delete(&self, k: &str) -> Result<(), StoreError> {
        self.conn
            .execute("DELETE FROM sync_state WHERE k = ?1", params![k])?;
        Ok(())
    }

    /// Every `sync_state` entry whose key starts with `prefix`, in key order.
    pub fn sync_prefix(&self, prefix: &str) -> Result<Vec<(String, Vec<u8>)>, StoreError> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT k, v FROM sync_state WHERE substr(k, 1, length(?1)) = ?1 ORDER BY k",
        )?;
        let rows = stmt.query_map(params![prefix], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Removes every `sync_state` entry whose key starts with `prefix`.
    pub fn sync_delete_prefix(&self, prefix: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "DELETE FROM sync_state WHERE substr(k, 1, length(?1)) = ?1",
            params![prefix],
        )?;
        Ok(())
    }

    /// Removes every `sync_state` entry whose key starts with `prefix` and
    /// ends with `suffix`.
    pub fn sync_delete_matching(&self, prefix: &str, suffix: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "DELETE FROM sync_state WHERE substr(k, 1, length(?1)) = ?1 \
             AND length(k) >= length(?1) + length(?2) AND substr(k, -length(?2)) = ?2",
            params![prefix, suffix],
        )?;
        Ok(())
    }

    /// Empties `sync_state` and the inbound queue (turning sync off). Records
    /// and the `account` table are untouched.
    pub fn sync_clear(&self) -> Result<(), StoreError> {
        self.conn
            .execute_batch("DELETE FROM sync_state; DELETE FROM inbound;")?;
        Ok(())
    }

    /// Up to `limit` unsynced rows (`dirty = 1`), tombstones included, ordered
    /// by `hlc` then `id` so the order is deterministic.
    pub fn dirty_rows(&self, limit: usize) -> Result<Vec<RecordRow>, StoreError> {
        let sql =
            format!("SELECT {COLUMNS} FROM records WHERE dirty = 1 ORDER BY hlc, id LIMIT ?1");
        let mut stmt = self.conn.prepare(&sql)?;
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = stmt.query_map(params![limit], |r| Ok(parse_row(r)))?;
        rows.map(|row| row?).collect()
    }

    /// Records that the server accepted the version of `id` stamped `hlc` at
    /// revision `rev`.
    ///
    /// `rev` is always stored, so the next push of this record uses the right
    /// base. `dirty` is cleared only if the row's `hlc` still equals `hlc`: if
    /// the user edited the record while the push was in flight, the row carries
    /// a newer `hlc` and stays dirty so that edit goes up next. A missing row
    /// is a no-op.
    pub fn mark_synced(&self, id: &Id, hlc: &Hlc, rev: i64) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE records SET rev = ?3, \
                 dirty = CASE WHEN hlc = ?2 THEN 0 ELSE dirty END \
             WHERE id = ?1",
            params![id.to_string(), hlc.to_string(), rev],
        )?;
        Ok(())
    }

    /// The highest HLC among `vault_id`'s records (tombstones included) and
    /// the inbound queue, if any: what a device has already seen, so its
    /// clock can resume past it after a restart or unlock. HLC text sorts
    /// like the value, so this is a plain `MAX`.
    pub fn max_hlc(&self, vault_id: &Id) -> Result<Option<Hlc>, StoreError> {
        let text: Option<String> = self.conn.query_row(
            "SELECT MAX(h) FROM (\
                 SELECT MAX(hlc) AS h FROM records WHERE vault_id = ?1 \
                 UNION ALL SELECT MAX(hlc) FROM inbound)",
            params![vault_id.to_string()],
            |r| r.get(0),
        )?;
        text.map(|t| t.parse().map_err(|_| bad("hlc", &t)))
            .transpose()
    }

    /// Whether record `id` has an unpushed local change (`dirty = 1`);
    /// `None` when there is no such record.
    pub fn is_dirty(&self, id: &Id) -> Result<Option<bool>, StoreError> {
        Ok(self
            .conn
            .query_row(
                "SELECT dirty FROM records WHERE id = ?1",
                params![id.to_string()],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
            .map(|d| d != 0))
    }

    /// Inserts or replaces the row with `r.id` exactly as given, with
    /// `dirty = 0`, `rev` included even when `None`. Used to apply a row that
    /// came from the server.
    pub fn put_synced(&self, r: &RecordRow) -> Result<(), StoreError> {
        self.put_with_dirty(r, false)
    }

    /// Forgets every record's server revision but keeps the dirty flags: the
    /// first step of a full resync. Afterwards a pull from `since = 0`
    /// re-learns the revisions of the rows the server has (clean rows take
    /// its version or win by HLC, same-HLC rows are acknowledged), and
    /// [`Store::mark_unsynced_dirty`] queues the rest for upload.
    pub fn reset_revs(&self) -> Result<(), StoreError> {
        self.conn.execute("UPDATE records SET rev = NULL", [])?;
        Ok(())
    }

    /// Marks dirty every live record that has no server revision (the
    /// server does not have it), so the next push uploads it as new. Clean
    /// tombstones without a revision are left alone: there is nothing to
    /// tell a server that never had the record.
    pub fn mark_unsynced_dirty(&self) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE records SET dirty = 1 WHERE rev IS NULL AND deleted = 0",
            [],
        )?;
        Ok(())
    }

    /// Number of records with unpushed changes.
    pub fn dirty_count(&self) -> Result<usize, StoreError> {
        let n: i64 =
            self.conn
                .query_row("SELECT count(*) FROM records WHERE dirty = 1", [], |r| {
                    r.get(0)
                })?;
        Ok(usize::try_from(n).unwrap_or(0))
    }

    /// Appends pulled rows to the inbound queue, all or none. A row whose
    /// `seq` is already queued is ignored, so a re-pulled page is harmless.
    pub fn inbound_push(&self, rows: &[InboundRow]) -> Result<(), StoreError> {
        // A savepoint rather than a transaction: it also works when the caller
        // is already inside `atomically`.
        self.conn.execute_batch("SAVEPOINT inbound_push")?;
        match self.inbound_insert(rows) {
            Ok(()) => {
                self.conn.execute_batch("RELEASE inbound_push")?;
                Ok(())
            }
            Err(e) => {
                // Best effort: the original error is the one worth reporting.
                let _ = self
                    .conn
                    .execute_batch("ROLLBACK TO inbound_push; RELEASE inbound_push");
                Err(e)
            }
        }
    }

    fn inbound_insert(&self, rows: &[InboundRow]) -> Result<(), StoreError> {
        let mut stmt = self.conn.prepare_cached(
            "INSERT OR IGNORE INTO inbound \
                 (seq, id, kind, key_version, hlc, rev, deleted, envelope) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )?;
        for r in rows {
            stmt.execute(params![
                r.seq,
                r.id.to_string(),
                r.kind,
                r.key_version,
                r.hlc.to_string(),
                r.rev,
                r.deleted,
                r.envelope,
            ])?;
        }
        Ok(())
    }

    /// Up to `limit` queued rows in ascending `seq`, without deleting them.
    pub fn inbound_take(&self, limit: usize) -> Result<Vec<InboundRow>, StoreError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {INBOUND_COLUMNS} FROM inbound ORDER BY seq LIMIT ?1"
        ))?;
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = stmt.query_map(params![limit], |r| Ok(parse_inbound(r)))?;
        rows.map(|row| row?).collect()
    }

    /// The queued row for record `id` with the highest `seq`, if any: what
    /// the record will look like once the queue is applied.
    pub fn inbound_latest(&self, id: &Id) -> Result<Option<InboundRow>, StoreError> {
        let sql = format!(
            "SELECT {INBOUND_COLUMNS} FROM inbound WHERE id = ?1 ORDER BY seq DESC LIMIT 1"
        );
        self.conn
            .query_row(&sql, params![id.to_string()], |r| Ok(parse_inbound(r)))
            .optional()?
            .transpose()
    }

    /// Deletes every queued row with `seq <= seq`.
    pub fn inbound_delete_through(&self, seq: i64) -> Result<(), StoreError> {
        self.conn
            .execute("DELETE FROM inbound WHERE seq <= ?1", params![seq])?;
        Ok(())
    }

    /// Empties the inbound queue (a full resync pulls everything again).
    pub fn inbound_clear(&self) -> Result<(), StoreError> {
        self.conn.execute("DELETE FROM inbound", [])?;
        Ok(())
    }

    /// Number of rows waiting in the inbound queue.
    pub fn inbound_len(&self) -> Result<usize, StoreError> {
        let n: i64 = self
            .conn
            .query_row("SELECT count(*) FROM inbound", [], |r| r.get(0))?;
        Ok(usize::try_from(n).unwrap_or(0))
    }
}
