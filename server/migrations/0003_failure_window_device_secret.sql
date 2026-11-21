-- "Consecutive" failed logins are time-bounded: a failure more than one
-- lockout window after the previous one restarts the count.
ALTER TABLE accounts ADD COLUMN last_failed_at INTEGER NOT NULL DEFAULT 0;

-- SHA-256 of the per-device secret issued when the device is created. A
-- password re-login must present the secret for its device (device ids are
-- not secret). NULL only for rows that predate this column.
ALTER TABLE devices ADD COLUMN secret_hash BLOB;
