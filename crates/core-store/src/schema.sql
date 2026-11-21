-- Schema version 1. Applied by `Store::open` when `PRAGMA user_version` is 0.
-- Only envelopes live here: everything sensitive is inside `records.envelope`
-- (or, for `account.v`, already wrapped by core-crypto).
CREATE TABLE IF NOT EXISTS records (
    id          TEXT PRIMARY KEY NOT NULL,
    vault_id    TEXT NOT NULL,
    kind        TEXT NOT NULL,
    key_version INTEGER NOT NULL,
    hlc         TEXT NOT NULL,
    rev         INTEGER,
    deleted     INTEGER NOT NULL DEFAULT 0,
    dirty       INTEGER NOT NULL DEFAULT 1,
    envelope    BLOB NOT NULL
);
CREATE INDEX IF NOT EXISTS records_vault_kind ON records (vault_id, kind);

CREATE TABLE IF NOT EXISTS account (
    k TEXT PRIMARY KEY NOT NULL,
    v BLOB NOT NULL
);
