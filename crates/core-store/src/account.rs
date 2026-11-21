//! Account key/value table: wrapped account keys, KDF params, salts.
//! Values are stored as given; callers store only already-wrapped material.

use rusqlite::{OptionalExtension, params};

use crate::{Store, StoreError};

impl Store {
    /// Returns the value stored under `k`, if any.
    pub fn account_get(&self, k: &str) -> Result<Option<Vec<u8>>, StoreError> {
        Ok(self
            .conn
            .query_row("SELECT v FROM account WHERE k = ?1", params![k], |r| {
                r.get(0)
            })
            .optional()?)
    }

    /// Stores `v` under `k`, replacing any previous value.
    pub fn account_put(&self, k: &str, v: &[u8]) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO account (k, v) VALUES (?1, ?2) \
             ON CONFLICT(k) DO UPDATE SET v = excluded.v",
            params![k, v],
        )?;
        Ok(())
    }

    /// Removes the value stored under `k`; a missing key is not an error.
    pub fn account_delete(&self, k: &str) -> Result<(), StoreError> {
        self.conn
            .execute("DELETE FROM account WHERE k = ?1", params![k])?;
        Ok(())
    }
}
