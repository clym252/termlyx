-- Schema version 2 (Phase 2 sync). Applied after v1 on a fresh file, and on its
-- own to a v1 file. Adds no plaintext: `inbound.envelope` is the sealed bytes
-- exactly as the server delivered them.

-- Sync engine bookkeeping (cursor, device id, last error, ...). Opaque blobs.
CREATE TABLE IF NOT EXISTS sync_state (
    k TEXT PRIMARY KEY NOT NULL,
    v BLOB NOT NULL
);

-- Pulled rows waiting to be applied. Filled even while the vault is locked
-- (nothing here is decrypted); drained in ascending `seq` once it is unlocked.
-- `kind` is free text: a kind this build does not know is skipped on apply.
CREATE TABLE IF NOT EXISTS inbound (
    seq         INTEGER PRIMARY KEY NOT NULL,
    id          TEXT NOT NULL,
    kind        TEXT NOT NULL,
    key_version INTEGER NOT NULL,
    hlc         TEXT NOT NULL,
    rev         INTEGER NOT NULL,
    deleted     INTEGER NOT NULL,
    envelope    BLOB NOT NULL
);

-- The outbox scan is `WHERE dirty = 1`.
CREATE INDEX IF NOT EXISTS records_dirty ON records (dirty);
