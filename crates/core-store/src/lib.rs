//! SQLite store for Tether's local vault. It holds envelopes only: record
//! payloads are sealed by `core-crypto` before they reach a row, so neither the
//! database file nor its WAL ever contains plaintext.
//!
//! * [`Store`]: opens the database (WAL, versioned migrations).
//! * [`RecordRow`] storage plus [`seal_row`]/[`open_row`], the one place that
//!   builds the `id|vault_id|kind|key_version` AAD.
//! * A small key/value table (`account_get`/`account_put`) for the account's
//!   already-wrapped keys.
//! * Sync support (schema v2): a `sync_state` key/value table, the outbox
//!   queries (`dirty_rows`, `mark_synced`, ...) and the `inbound` queue of
//!   pulled-but-not-yet-applied rows, which fills even while the vault is locked.

mod account;
mod repo;
mod sync;

use std::path::{Path, PathBuf};

use core_model::Id;
use rusqlite::{Connection, Transaction, TransactionBehavior};

pub use repo::{RecordRow, open_row, seal_row};
pub use sync::InboundRow;

const SCHEMA_V1: &str = include_str!("schema.sql");
const SCHEMA_V2: &str = include_str!("schema_v2.sql");
/// Schema version this build writes and understands (`PRAGMA user_version`).
const SCHEMA_VERSION: i64 = 2;

/// Errors returned by `core-store`. None carries plaintext or key material.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// SQLite failed, including "file is not a database" for a garbage file.
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// The record's envelope failed to decrypt or is malformed: bit rot,
    /// tampering, a wrong key, or an envelope moved between rows.
    #[error("record {id} is corrupt: envelope failed to decrypt")]
    Corrupt { id: Id },
    /// The record is a tombstone and has no envelope to open.
    #[error("record {id} is deleted")]
    Deleted { id: Id },
    /// No record with this id exists.
    #[error("record {id} not found")]
    NotFound { id: Id },
    /// A stored column does not parse (id, kind, hlc or number out of range).
    #[error("malformed row: {0}")]
    BadRow(String),
    /// `PRAGMA quick_check` found structural damage (SQLite's description,
    /// which names pages and tables, never row contents).
    #[error("database file is damaged: {0}")]
    Damaged(String),
    /// The database was written by a newer build.
    #[error("database schema version {found} is newer than supported version {supported}")]
    UnsupportedSchema { found: i64, supported: i64 },
    /// The file exists but is empty (0 bytes): truncated, or a crash right
    /// after creating it. Never treated as a new vault.
    #[error("database file is empty (0 bytes)")]
    Empty,
}

/// The local vault database.
pub struct Store {
    pub(crate) conn: Connection,
}

impl Store {
    /// Opens (creating if missing) the database at `path` in WAL mode and
    /// migrates it to the current schema.
    ///
    /// A file that is not a SQLite database, or one that fails
    /// `PRAGMA quick_check`, yields `Err` and is left untouched. So does an
    /// existing 0-byte file ([`StoreError::Empty`]): SQLite would treat it as
    /// a new database and write a schema into it. Migration (v1, then v2, in one
    /// `BEGIN IMMEDIATE` transaction) only writes when
    /// `user_version` is behind, so opening an up-to-date database performs
    /// no writes.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        if std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() == 0) {
            return Err(StoreError::Empty);
        }
        let mut conn = Connection::open(path)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        // First read of the file: rejects non-databases before any pragma can write.
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version > SCHEMA_VERSION {
            return Err(StoreError::UnsupportedSchema {
                found: version,
                supported: SCHEMA_VERSION,
            });
        }
        // Read-only structural check, still before anything can write: damaged
        // b-tree pages would otherwise open fine and only fail on first use.
        let check: String = conn.query_row("PRAGMA quick_check(1)", [], |r| r.get(0))?;
        if check != "ok" {
            return Err(StoreError::Damaged(check));
        }
        // Returns the resulting mode as a row; a no-op when already WAL.
        conn.query_row("PRAGMA journal_mode = WAL", [], |_| Ok(()))?;
        // Tombstoning clears an envelope; make SQLite overwrite the freed bytes.
        conn.execute_batch("PRAGMA secure_delete = ON;")?;
        if version < SCHEMA_VERSION {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            // Steps run in order inside one transaction: a fresh file (0) gets
            // v1 then v2, a Phase 1 file (1) only v2.
            if version < 1 {
                tx.execute_batch(SCHEMA_V1)?;
            }
            if version < 2 {
                tx.execute_batch(SCHEMA_V2)?;
            }
            tx.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION};"))?;
            tx.commit()?;
        }
        Ok(Store { conn })
    }

    /// The database file's path, as SQLite resolved it.
    pub fn path(&self) -> Result<PathBuf, StoreError> {
        let file: String = self.conn.query_row(
            "SELECT file FROM pragma_database_list WHERE name = 'main'",
            [],
            |r| r.get(0),
        )?;
        Ok(PathBuf::from(file))
    }

    /// Writes a consistent copy of the whole database to `dest`
    /// (`VACUUM INTO`), while it stays open. Fails if `dest` exists.
    pub fn copy_to(&self, dest: &Path) -> Result<(), StoreError> {
        if dest.exists() {
            return Err(StoreError::BadRow(format!(
                "copy target already exists: {}",
                dest.display()
            )));
        }
        let target = dest
            .to_str()
            .ok_or_else(|| StoreError::BadRow("copy target path is not valid UTF-8".to_owned()))?;
        self.conn.execute("VACUUM INTO ?1", [target])?;
        Ok(())
    }

    /// Deletes every record, the account, the sync state and the inbound
    /// queue (`secure_delete` overwrites the freed pages). Call it inside
    /// [`Store::atomically`] together with whatever replaces the contents.
    pub fn wipe(&self) -> Result<(), StoreError> {
        self.conn.execute_batch(
            "DELETE FROM records; DELETE FROM account; DELETE FROM sync_state; DELETE FROM inbound;",
        )?;
        Ok(())
    }

    /// Runs `f` inside one SQLite transaction (`BEGIN IMMEDIATE`): every
    /// write it makes through this store commits together when it returns
    /// `Ok`, and is rolled back when it returns `Err` or panics.
    ///
    /// Not reentrant: calling `atomically` from inside `f` returns an error.
    pub fn atomically<T, E>(&self, f: impl FnOnce() -> Result<T, E>) -> Result<T, E>
    where
        E: From<StoreError>,
    {
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)
            .map_err(StoreError::from)?;
        // On `Err` (or a panic) `tx` is dropped here, which rolls back.
        let out = f()?;
        tx.commit().map_err(StoreError::from)?;
        Ok(out)
    }
}
