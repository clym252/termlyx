-- Tether server schema, version 1. Everything here is ciphertext, wrapped key
-- material, hashes or cleartext sync metadata (ids, kinds, HLCs, sizes,
-- timestamps, tombstone flags); see docs/crypto.md "Server metadata".
--
-- Timestamps are Unix milliseconds. Tokens (invite, refresh, access, link) are
-- 32 random bytes; only their SHA-256 is stored. Never edit this file once
-- committed: later changes go in 0002+.

-- Server-wide values, created once on first start:
--   epoch          ASCII hex of 16 random bytes; changes only if the database
--                  is replaced (clients use it to detect a restored/reset server)
--   server_secret  32 random bytes; keys the fake-prelogin HMAC
CREATE TABLE meta (
    k TEXT PRIMARY KEY NOT NULL,
    v BLOB NOT NULL
) WITHOUT ROWID;

CREATE TABLE accounts (
    id                     TEXT PRIMARY KEY NOT NULL,          -- uuid
    username               TEXT NOT NULL UNIQUE COLLATE NOCASE,
    -- Argon2id(AuthKey, verifier_salt) and its salt.
    verifier               BLOB NOT NULL,
    verifier_salt          BLOB NOT NULL,
    -- Same for the recovery auth key; NULL until a recovery key is registered.
    recovery_verifier      BLOB,
    recovery_verifier_salt BLOB,
    -- KDF parameters the client derived the master key with (served by prelogin).
    kdf_m_kib              INTEGER NOT NULL,
    kdf_t                  INTEGER NOT NULL,
    kdf_p                  INTEGER NOT NULL,
    kdf_salt               BLOB NOT NULL,
    -- Account key material, opaque to the server (wrapped exactly as in the
    -- client's local `account` table).
    vault_id               TEXT NOT NULL UNIQUE,
    key_version            INTEGER NOT NULL,
    account_public         BLOB NOT NULL,
    secret_by_kek          BLOB NOT NULL,
    secret_by_recovery     BLOB NOT NULL,
    sealed_vault_key       BLOB NOT NULL,
    -- Bumps on password change or recovery so other devices notice.
    account_rev            INTEGER NOT NULL DEFAULT 1,
    -- Highest record kind set version any device of the account has pushed.
    max_schema             INTEGER NOT NULL DEFAULT 0,
    -- Next HLC node id to hand to a new device of this account.
    next_node              INTEGER NOT NULL DEFAULT 1,
    disabled               INTEGER NOT NULL DEFAULT 0,
    failed_logins          INTEGER NOT NULL DEFAULT 0,
    locked_until           INTEGER NOT NULL DEFAULT 0,
    created_at             INTEGER NOT NULL
);

CREATE TABLE invites (
    token_hash BLOB PRIMARY KEY NOT NULL,
    note       TEXT,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    used_at    INTEGER
) WITHOUT ROWID;

CREATE TABLE devices (
    id         TEXT PRIMARY KEY NOT NULL,                       -- uuid
    account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    name       TEXT NOT NULL,
    platform   TEXT NOT NULL,
    -- HLC node id, unique within the account.
    node       INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    last_seen  INTEGER NOT NULL,
    revoked    INTEGER NOT NULL DEFAULT 0,
    UNIQUE (account_id, node)
);
CREATE INDEX devices_by_account ON devices (account_id);

CREATE TABLE refresh_tokens (
    hash       BLOB PRIMARY KEY NOT NULL,
    device_id  TEXT NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL,
    -- Set when the token is exchanged; presenting a rotated token again means
    -- it leaked, and the device is revoked.
    rotated_at INTEGER
) WITHOUT ROWID;
CREATE INDEX refresh_tokens_by_device ON refresh_tokens (device_id);

CREATE TABLE access_tokens (
    hash       BLOB PRIMARY KEY NOT NULL,
    account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    -- NULL for the limited-scope token issued by recover-login.
    device_id  TEXT REFERENCES devices(id) ON DELETE CASCADE,
    scope      TEXT NOT NULL DEFAULT 'full',                    -- 'full' | 'recover'
    expires_at INTEGER NOT NULL
) WITHOUT ROWID;
CREATE INDEX access_tokens_by_device ON access_tokens (device_id);

CREATE TABLE link_tokens (
    hash       BLOB PRIMARY KEY NOT NULL,
    account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    expires_at INTEGER NOT NULL,
    used_at    INTEGER
) WITHOUT ROWID;

-- The synced vault. `envelope` is ciphertext; everything else is the accepted
-- cleartext metadata. `rev` is per record (1, then +1 per accepted write);
-- `seq` is the per-vault change counter assigned on every accepted write.
CREATE TABLE records (
    vault_id    TEXT NOT NULL,
    id          TEXT NOT NULL,
    kind        TEXT NOT NULL,
    key_version INTEGER NOT NULL,
    hlc         TEXT NOT NULL,
    rev         INTEGER NOT NULL,
    seq         INTEGER NOT NULL,
    deleted     INTEGER NOT NULL DEFAULT 0,
    envelope    BLOB NOT NULL,
    updated_at  INTEGER NOT NULL,
    PRIMARY KEY (vault_id, id)
) WITHOUT ROWID;
CREATE UNIQUE INDEX records_by_seq ON records (vault_id, seq);

CREATE TABLE vault_seq (
    vault_id          TEXT PRIMARY KEY NOT NULL,
    -- Last assigned seq.
    head              INTEGER NOT NULL DEFAULT 0,
    -- Changes at or below this seq may have been compacted away (tombstones);
    -- a cursor older than this gets 410 and does a full resync.
    compacted_through INTEGER NOT NULL DEFAULT 0
) WITHOUT ROWID;
