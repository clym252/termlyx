-- Refresh grace: a lost answer to a refresh must not revoke the device.
--
-- `successor_hash` is set when a refresh token is exchanged: the SHA-256 of
-- the refresh token issued in its place. `refresh_hash` ties each access
-- token to the refresh token issued with it, and `used` records that the
-- access token has been presented. A rotated-out refresh token presented
-- again within ten minutes of its rotation, while its successor has never
-- been used (not exchanged, and its access token never presented), is
-- answered once more instead of revoking the device. Rows that predate this
-- migration have no successor and keep the old rule (reuse revokes).
ALTER TABLE refresh_tokens ADD COLUMN successor_hash BLOB;
ALTER TABLE access_tokens ADD COLUMN refresh_hash BLOB;
ALTER TABLE access_tokens ADD COLUMN used INTEGER NOT NULL DEFAULT 0;
