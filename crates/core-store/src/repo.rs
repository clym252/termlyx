//! Record rows and the row codec.

use core_crypto::{Aad, open, seal};
use core_model::{Hlc, Id, Kind};
use rusqlite::{OptionalExtension, Row, params};
use zeroize::Zeroizing;

use crate::{Store, StoreError};

/// One stored record: routing metadata in the clear, payload in `envelope`.
///
/// A tombstone has `deleted == true` and an empty `envelope`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordRow {
    pub id: Id,
    pub vault_id: Id,
    pub kind: Kind,
    pub key_version: u32,
    pub hlc: Hlc,
    /// Server revision; `None` until the server has acknowledged the record.
    pub rev: Option<i64>,
    pub deleted: bool,
    pub envelope: Vec<u8>,
}

/// Seals `plaintext` into a live row, binding `id|vault_id|kind|key_version`
/// as AAD (ids as hyphenated lowercase UUID text, kind via [`Kind::as_str`]).
pub fn seal_row(
    key: &[u8; 32],
    vault_id: Id,
    kind: Kind,
    id: Id,
    key_version: u32,
    hlc: Hlc,
    plaintext: &[u8],
) -> RecordRow {
    let (id_text, vault_text) = (id.to_string(), vault_id.to_string());
    let aad = Aad {
        id: &id_text,
        vault_id: &vault_text,
        kind: kind.as_str(),
        key_version,
    };
    RecordRow {
        id,
        vault_id,
        kind,
        key_version,
        hlc,
        rev: None,
        deleted: false,
        envelope: seal(key, &aad, plaintext),
    }
}

/// Decrypts a row's envelope using the AAD rebuilt from the row's own fields.
///
/// Returns [`StoreError::Deleted`] for a tombstone, and
/// [`StoreError::Corrupt`] (naming the row id) if the envelope is malformed,
/// was altered, or does not belong to this row.
pub fn open_row(key: &[u8; 32], row: &RecordRow) -> Result<Zeroizing<Vec<u8>>, StoreError> {
    if row.deleted {
        return Err(StoreError::Deleted { id: row.id });
    }
    let (id_text, vault_text) = (row.id.to_string(), row.vault_id.to_string());
    let aad = Aad {
        id: &id_text,
        vault_id: &vault_text,
        kind: row.kind.as_str(),
        key_version: row.key_version,
    };
    open(key, &aad, &row.envelope).map_err(|_| StoreError::Corrupt { id: row.id })
}

pub(crate) const COLUMNS: &str = "id, vault_id, kind, key_version, hlc, rev, deleted, envelope";

fn bad(column: &str, text: &str) -> StoreError {
    StoreError::BadRow(format!("invalid {column}: {text:?}"))
}

pub(crate) fn parse_row(r: &Row<'_>) -> Result<RecordRow, StoreError> {
    let id: String = r.get(0)?;
    let vault_id: String = r.get(1)?;
    let kind: String = r.get(2)?;
    let key_version: i64 = r.get(3)?;
    let hlc: String = r.get(4)?;
    Ok(RecordRow {
        id: Id::parse_str(&id).map_err(|_| bad("id", &id))?,
        vault_id: Id::parse_str(&vault_id).map_err(|_| bad("vault_id", &vault_id))?,
        kind: kind.parse().map_err(|_| bad("kind", &kind))?,
        key_version: u32::try_from(key_version)
            .map_err(|_| StoreError::BadRow(format!("invalid key_version: {key_version}")))?,
        hlc: hlc.parse().map_err(|_| bad("hlc", &hlc))?,
        rev: r.get(5)?,
        deleted: r.get::<_, i64>(6)? != 0,
        envelope: r.get(7)?,
    })
}

impl Store {
    /// Inserts or replaces the row with `r.id`, marking it dirty (unsynced).
    ///
    /// A row with `rev == None` (a fresh local edit) keeps the stored `rev`, so
    /// the next push still knows its base revision.
    pub fn put(&self, r: &RecordRow) -> Result<(), StoreError> {
        self.put_with_dirty(r, true)
    }

    /// Shared upsert behind [`Store::put`] and [`Store::put_synced`]. A dirty
    /// write keeps the stored `rev` when `r.rev` is `None`; a clean one writes
    /// `r.rev` exactly.
    pub(crate) fn put_with_dirty(&self, r: &RecordRow, dirty: bool) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO records (id, vault_id, kind, key_version, hlc, rev, deleted, dirty, envelope) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?9, ?8) \
             ON CONFLICT(id) DO UPDATE SET \
                 vault_id = excluded.vault_id, kind = excluded.kind, \
                 key_version = excluded.key_version, hlc = excluded.hlc, \
                 rev = CASE WHEN excluded.dirty = 1 \
                            THEN COALESCE(excluded.rev, records.rev) ELSE excluded.rev END, \
                 deleted = excluded.deleted, \
                 dirty = excluded.dirty, envelope = excluded.envelope",
            params![
                r.id.to_string(),
                r.vault_id.to_string(),
                r.kind.as_str(),
                r.key_version,
                r.hlc.to_string(),
                r.rev,
                r.deleted,
                r.envelope,
                dirty,
            ],
        )?;
        Ok(())
    }

    /// Returns the row with this id, tombstones included.
    pub fn get(&self, id: &Id) -> Result<Option<RecordRow>, StoreError> {
        let sql = format!("SELECT {COLUMNS} FROM records WHERE id = ?1");
        self.conn
            .query_row(&sql, params![id.to_string()], |r| Ok(parse_row(r)))
            .optional()?
            .transpose()
    }

    /// Live (not deleted) rows of `kind` in `vault_id`, ordered by `hlc`.
    pub fn list(&self, vault_id: &Id, kind: Kind) -> Result<Vec<RecordRow>, StoreError> {
        let sql = format!(
            "SELECT {COLUMNS} FROM records \
             WHERE vault_id = ?1 AND kind = ?2 AND deleted = 0 ORDER BY hlc, id"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![vault_id.to_string(), kind.as_str()], |r| {
            Ok(parse_row(r))
        })?;
        rows.map(|row| row?).collect()
    }

    /// Turns a record into a tombstone: envelope cleared, `deleted = 1`, `hlc`
    /// updated, `dirty = 1`. [`StoreError::NotFound`] if there is no such record.
    pub fn tombstone(&self, id: &Id, hlc: Hlc) -> Result<(), StoreError> {
        let n = self.conn.execute(
            "UPDATE records SET envelope = x'', deleted = 1, hlc = ?2, dirty = 1 WHERE id = ?1",
            params![id.to_string(), hlc.to_string()],
        )?;
        if n == 0 {
            return Err(StoreError::NotFound { id: *id });
        }
        Ok(())
    }
}
