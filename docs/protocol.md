# Termlyx sync protocol (v1)

HTTP+JSON over TLS, plus one WebSocket for change notifications. The wire types
live in `crates/core-proto` (`api.rs`); this document is the human-readable
contract. The server only ever sees ciphertext, wrapped keys and the AuthKey
(see `docs/crypto.md`, "Server authentication").

## Versioning headers

| Header | Direction | Value |
| --- | --- | --- |
| `X-Tether-Protocol` | request and response | `PROTOCOL_VERSION` (currently `1`). A request with a major newer than the server's gets `400 update_server`; one older than the server's minimum gets `400 update_client` (currently both are `1`, so neither happens yet). A client that sees a newer server major than it understands shows "update Tether". The server sends the header on every response. |
| `X-Tether-Schema` | request | The client's record kind set version (`KIND_SET_VERSION`). The server remembers the highest value any device of an account has sent; a write from a client below it is refused with `400 update_client` (see "Schema gating"). A missing, zero or non-numeric value counts as `1`; a number above `KNOWN_SCHEMA + 8` (currently 11) is refused with `400 bad_request` on every authenticated endpoint and recorded nowhere. |
| `X-Tether-Device` | request | The calling device's id, once it has one. |
| `X-Tether-Device-Secret` | request | The device's secret (`TokenResp.deviceSecret`); required with `X-Tether-Device` for a password re-login. |

Header names are case-insensitive.

## Conventions

* JSON, camelCase field names. Ids (vault, record, device) are UUID strings.
* **Byte fields are base64, standard alphabet, with padding** (auth keys,
  envelopes, salts, wrapped secrets, invite and link tokens). The one other
  encoding is the join code's base32, below.
* Timestamps are Unix milliseconds. `hlc` is the fixed-width text form of the
  hybrid logical clock.
* Access and refresh tokens are opaque strings: 32 random bytes as base64url
  without padding (43 characters), so they use only HTTP token characters. The
  access token is sent as `Authorization: Bearer <token>` and also travels in
  a WebSocket subprotocol. The server stores only the SHA-256 of the 32 bytes.
* **Body limit: 2 MiB.** The server has a 4 MiB request-body layer, but the JSON
  extractor underneath it applies axum's default of 2 MiB, so in practice a
  request body over 2 MiB gets `413 payload_too_large`. A client must treat
  2 MiB as the limit: the Termlyx client caps each push at about 1.5 MiB of
  JSON (a single record's envelope may be up to 1 MiB, so one maximal record
  still fits on its own) and sends large vaults in several pushes.

## Errors

Every non-2xx response carries:

```json
{ "code": "update_client", "message": "update Tether" }
```

`code` is one of `bad_request`, `unauthorized`, `revoked`, `forbidden`,
`not_found`, `conflict`, `gone`, `clock_skew`, `rate_limited`, `update_client`,
`update_server`, `invite_invalid`, `username_taken`, `payload_too_large`,
`internal`. `message` is plain language that is safe to show to the user; it
never echoes request bodies, tokens or key material. Failures the server
itself did not anticipate (router 404/405, malformed JSON, the body limit) are
rewritten to the same shape, so a client only ever parses one error body.

HTTP status per `code`:

| `code` | Status |
| --- | --- |
| `bad_request`, `clock_skew`, `update_client`, `update_server` | 400 |
| `unauthorized`, `revoked` | 401 |
| `forbidden`, `invite_invalid` | 403 |
| `not_found` | 404 |
| `conflict`, `username_taken` | 409 |
| `gone` | 410 |
| `payload_too_large` | 413 |
| `rate_limited` | 429 |
| `internal` | 500 (generic message; the cause is only in the server log) |

`GET /v1/health` answers `{"status": "ok", "protocol": 1, "epoch": "<32 hex>"}`.
`epoch` identifies the database and never changes unless it is replaced. A
request whose `X-Tether-Protocol` major is above the server's gets
`400 update_server`; a malformed value gets `400 bad_request`; a missing header
is accepted.

The server logs, per request, a request id, method, matched route pattern,
status and latency, and nothing else: no headers, query strings, raw paths or
bodies.

## Join codes

An invite code (create an account) or an add-device code (link a new device to
an existing account) is one line of text:

```
tether1-<base32>
```

`<base32>` is RFC 4648 base32, lowercase, no padding, of

```
[version = 1][kind: 0 invite | 1 link][has_pin: 0|1][pin: 32 bytes, only if has_pin]
[token: 32 bytes][url_len: u16 little-endian][url: UTF-8]
```

* `pin` is the SHA-256 of the server certificate's SubjectPublicKeyInfo (for a
  self-signed server). Without a pin the client uses normal WebPKI trust.
* `url` must be `https://`. Plain `http://` is accepted only for a loopback host
  (`localhost`, `127.0.0.0/8`, `::1`) in a code that has no pin. A URL with
  userinfo (`user@host`) is rejected.
* Parsers ignore whitespace and letter case. Any other version, kind or flag
  value, a truncated payload or trailing bytes is an error.
* Committed fixtures: `crates/core-proto/tests/golden/{invite,link}.txt`.

## Short codes

A short code (`TLX-XXXX-XXXX-XXXX-XXXX`) is 10 random bytes as 16 symbols of the
alphabet `0123456789ABCDEFGHJKMNPQRSTVWXYZ`. Parsers ignore case, spaces and
dashes, a leading `TLX`, and read `O` as `0` and `I`/`L` as `1`. Full join codes
(`tether1-...`) are unchanged. The code is never sent; the client proves it.

```
token        = SHA256("termlyx-short-code-v1" || raw10)
key          = SHA256(token)                 // equals invites.token_hash
id           = SHA256("termlyx-code-id-v1" || key)[..16]
server_proof = HMAC-SHA256(key, "termlyx-code-server-v1" || binding || client_nonce)
client_proof = HMAC-SHA256(key, "termlyx-code-client-v1" || binding || client_nonce)
binding      = 32-byte SPKI pin when pinned, else empty
```

`POST /v1/code/verify` (no auth): `CodeVerifyReq {codeId, clientNonce, pinned}`
(16 and 32 bytes, base64) returns `CodeVerifyResp {serverProof}`. The client
checks `server_proof` before trusting the server. An unknown, used or expired
code answers `invite_invalid` (403).

`RegisterReq.codeProof` (`{codeId, clientNonce, pinned, proof}`, omitted when
absent) redeems a short code instead of `inviteToken`, which is then empty;
`proof` is `client_proof`. A bad or missing proof is `invite_invalid`.

## WebSocket

`GET /v1/events` upgrades to a WebSocket (`server/src/events.rs`).

* **Auth.** The client offers two subprotocols: `tether.v1, bearer.<access
  token>` (the token never appears in a URL). The server validates the token
  with the rules of every device endpoint and selects `tether.v1` only; the
  `bearer.` value is never echoed. Missing or invalid token: no upgrade, an
  ordinary `ErrorBody` (`401 unauthorized`; `401 revoked` for a revoked device;
  `403 forbidden` for the limited recovery token or a disabled account). A
  request that does not offer `tether.v1` is `400 bad_request`. The token is
  checked once, at the upgrade; an open socket outlives the 15 minute token.
* **Events.** Each text frame is one `Event` JSON object tagged by `type`:
  `{"type":"changed","seq":42}`, `{"type":"account_changed","accountRev":3}`,
  `{"type":"revoked"}`. Clients send nothing (frames over 4 KiB close the
  socket).
* **Who hears what.** `changed {seq}` (the vault head) goes to every device of
  the account **except the pusher**, after a push with at least one `ok`
  result. `account_changed` goes to every device, the caller included.
  `revoked` goes to the revoked device only (`DELETE /v1/devices/{id}`, or
  refresh-token reuse), after which the server closes the socket.
* **Liveness.** The server pings every 30 s and closes a peer that sent no
  frame (a pong counts) for two intervals. On every tick it also re-checks the
  device: if it was revoked it sends `revoked` and closes, so a missed message
  cannot leave a revoked device listening; a deleted device or disabled
  account just closes. A subscriber that falls more than 64 events behind is
  closed too. Clients reconnect with backoff and then pull `changes`.

## Accounts and sessions

All of these are in `server/src/{accounts,auth,devices}.rs`. Request and
response bodies are the `core-proto` types named in the table below.

### Rules

* **Usernames** are trimmed, 3 to 64 characters of `[a-z0-9._-]` after
  lowercasing (so they are case-insensitive), unique per server. Anything else
  is `400 bad_request`.
* **Device descriptions** (`DeviceInfo`): name 1 to 100 characters, platform 1
  to 40, no control characters.
* **Verifier.** `authKey` and `recoveryAuthKey` are exactly 32 bytes. The
  server stores `Argon2id(key, 16-byte random salt, m = 19 MiB, t = 2, p = 1)`
  (32-byte output, compared in constant time) and never the key itself.
* **Account material** (`RegisterReq.material`) is stored as sent and served
  back by `GET /v1/account`. It must look right: `vaultId` a lowercase
  hyphenated UUID, `keyVersion` >= 1, `kdf` within `m` <= 1 GiB, `t` 1..=16,
  `p` 1..=64, `m` >= 8p and a 16-byte salt, `accountPublic` 32 bytes,
  `secretByKek` and `secretByRecovery` 73 bytes, `sealedVaultKey` 81 bytes
  (`docs/crypto.md`, "Local account record"). Otherwise `400 bad_request`, and
  an invite is not used up by a rejected request.
* **Tokens.** Access tokens live 15 minutes (`expiresIn: 900`). A refresh
  token is single-use: `POST /v1/refresh` marks it rotated and returns a new
  pair. Presenting a rotated-out refresh token again means it leaked, so the
  device is **revoked** (`401 revoked` from then on, for every token the device
  holds); rotated tokens are remembered for 30 days. Refresh tokens do not
  expire on their own.
* **Refresh grace (a lost answer).** One exception to reuse → revoke: a
  rotated-out refresh token presented again less than 10 minutes after its
  rotation, while the pair that rotation issued has never been used (its
  refresh token not exchanged, its access token never presented on any
  endpoint or the event socket), is answered with a fresh pair once more. The
  unused pair is invalidated (it becomes unknown: `401 unauthorized`), and the
  old token keeps no further grace, so a third presentation revokes. This is
  what a device does when the answer to its refresh was lost (Wi-Fi drop,
  sleep, a timeout); two concurrent refreshes with one token end with exactly
  one live session (the later answer's). Server migration `0004` adds the
  columns this needs; tokens rotated before it get no grace.
* **Revocation.** A revoked device keeps its (now useless) token rows, so its
  next request of any kind, including `refresh` and `login`, is answered
  `401 revoked` ("device revoked") rather than a generic "sign in again". It
  is no longer listed. Its HLC node id stays reserved.
* **Node ids.** The device that registers keeps the `nodeHint` it asked for
  (any `u16`, it is the only device so it is free). Every later device gets the
  lowest unused `u16` >= 1 among all the account's devices, revoked ones
  included. `TokenResp.node` carries the result.
* **Device secret.** Creating a device (register, or login with a link token)
  mints a 32-byte secret, returned once as `TokenResp.deviceSecret` (base64url,
  no padding) and stored only as a SHA-256. Device ids are not secret, so the
  secret is what proves a re-login comes from that device. The client keeps it
  next to the refresh token. Refresh, re-login and recover-login responses omit
  the field.
* **Add device.** `POST /v1/link` returns a `tether1-` code of kind link (URL,
  pin, 32-byte link token). The token is valid for 10 minutes, once, and only
  for that account. The new device sends `POST /v1/login` with
  `linkToken` set to the 32 bytes in the code. A `login` **without** a link
  token only signs an existing device back in: it must send
  `X-Tether-Device` (a device of the account) **and** `X-Tether-Device-Secret`
  with the correct AuthKey. A wrong or missing secret is `401 unauthorized` and
  counts as a failed sign-in; an id that is not one of the account's devices,
  or no id, is `403 forbidden`; the right secret of a revoked device is
  `401 revoked`. A password alone never adds a device, and never revives a
  revoked one. The previous session of the device ends. A lone device that lost
  both its tokens and its secret gets a link code from the operator:
  `tether-server link USER` prints one (valid 10 minutes, once).
* **Invites.** Created offline with `tether-server invite [--note TEXT]
  [--expires 7d]` (units `m`, `h`, `d`), which prints the code and nothing
  else on stdout. An invite is single-use and expires (default 7 days).
  Codes carry the server's public URL and, in self-signed TLS mode, the
  certificate pin.
* **Rate limits** on `prelogin`, `register`, `login`, `recover-login` and
  `code/verify` (limited per address and per code id instead of username):
  20 requests per minute per client address and 10 per minute per username
  (sliding windows; the address is the TCP peer, IPv6 counted per /64, and
  proxy headers are not trusted), then `429 rate_limited`. Ten consecutive
  failed `login`/`recover-login` attempts on a username lock it for 15 minutes
  (`429 rate_limited`, even for the right key). Failures are "consecutive"
  within a window: a failure more than 15 minutes after the previous one
  starts the count again, and a success clears it. The lock of a real account
  is stored in the database and survives a restart. Unknown usernames follow
  the same rules in memory (so a locked account looks like any other name),
  bounded to 100 000 names with the oldest evicted first; **their counts and
  locks are lost when the server restarts**.
* **No enumeration.** `prelogin` answers an unknown username with production
  default KDF parameters (`m` 65536 KiB, `t` 3, `p` 4) and a stable salt
  `HMAC-SHA256(server_secret, "prelogin|" + username)[..16]`: same shape, same
  values every time, different per name. A wrong key and an unknown username
  both get `401 unauthorized` and cost the same Argon2 work.
* A disabled account (`tether-server users disable`) is refused with
  `403 forbidden` at login, refresh and on every authenticated call.

### Endpoints

| Method and path | Auth | Request | Response | Errors |
| --- | --- | --- | --- | --- |
| `POST /v1/prelogin` | none | `PreloginReq {username}` | `PreloginResp {kdf}` | `bad_request`, `rate_limited` |
| `POST /v1/register` | none (invite token in body) | `RegisterReq` | `TokenResp` (with `deviceSecret`) | `bad_request`, `invite_invalid` (unknown, used, expired or malformed token; bad or malformed `codeProof`), `username_taken`, `conflict` (vault id already exists), `rate_limited` |
| `POST /v1/code/verify` | none | `CodeVerifyReq` | `CodeVerifyResp` | `invite_invalid` (unknown, used or expired code, malformed fields, or `pinned` with TLS off), `rate_limited` |
| `POST /v1/login` | none | `LoginReq` (`linkToken` to add a device; otherwise `X-Tether-Device` + `X-Tether-Device-Secret`) | `TokenResp` (`deviceSecret` set when a device was created) | `bad_request`, `unauthorized` (wrong key, unknown user, or wrong/missing device secret), `rate_limited`, `forbidden` (no link token and not an existing device; disabled account), `invite_invalid` (link token unknown, spent, expired or another account's), `revoked` |
| `POST /v1/recover-login` | none | `RecoverReq` | `TokenResp` (limited) | `bad_request`, `unauthorized`, `rate_limited`, `forbidden` |
| `POST /v1/refresh` | none (refresh token in body) | `RefreshReq` | `TokenResp` | `unauthorized`, `revoked` (also on reuse of a rotated token outside the grace), `forbidden` |
| `POST /v1/logout` | bearer | | `204` | `unauthorized`, `revoked`, `forbidden` |
| `GET /v1/devices` | bearer | | `[DeviceView]` (not revoked, oldest first; `current` marks the caller) | `unauthorized`, `revoked` |
| `PATCH /v1/devices/{id}` | bearer | `RenameDeviceReq {name}` | `204` | `bad_request`, `not_found` (not this account's, or revoked) |
| `DELETE /v1/devices/{id}` | bearer | | `204` (revoking an already revoked device is fine; a device may revoke itself) | `not_found` (not this account's) |
| `POST /v1/link` | bearer | | `LinkResp {code, expiresAt}` | `unauthorized`, `revoked` |

`recover-login` takes the account's recovery auth key (the verifier stored at
registration) and answers with a `TokenResp` whose `accessToken` is a
**limited-scope** token valid for 10 minutes (`expiresIn: 600`): it is accepted
only by `GET /v1/account` and `POST /v1/account/recover`, and every other
authenticated endpoint answers it `403 forbidden`. `refreshToken` and `deviceId` are empty strings and
`node` is `0`: it creates no device and cannot be refreshed. The `device` in
`RecoverReq` is accepted but unused.

`logout` ends the calling device's session (its access and refresh tokens are
deleted); the device stays registered and listed until it is revoked, and can
sign in again with `login` + `X-Tether-Device` + `X-Tether-Device-Secret`.
The same password re-login brings a device back after a server restore: the
restored database predates the device's current refresh token, so `refresh`
answers `401 unauthorized`, but the device row and its secret hash are still
there. A device created after the backup is unknown to the restored server
(`403 forbidden`) and needs an add-device code.

Error precedence on authenticated calls: a revoked device is `revoked` even if
its token has also expired; otherwise an unknown or expired token is
`unauthorized`; a disabled account or the limited recovery token is
`forbidden`.

## Account key material, password change, recovery

Implemented in `server/src/account.rs`. The server holds the opaque pieces a
fresh device needs (`vaultId`, `keyVersion`, `kdf`, `accountPublic`,
`secretByKek`, `secretByRecovery`, `sealedVaultKey`: `AccountMaterial`), never
`account_secret.device` or `quick_unlock.*`, together with `accountRev`
(starts at 1). Every endpoint below answers an `AccountResp {material,
accountRev}` describing the account **after** the call.

| Method and path | Auth | Request | Errors |
| --- | --- | --- | --- |
| `GET /v1/account` | bearer, or the recovery token | | `unauthorized`, `revoked`, `forbidden` |
| `POST /v1/account/password` | bearer | `PasswordChangeReq` | `bad_request`, `unauthorized` (wrong `oldAuthKey`), `rate_limited` (account locked), `conflict` (stale `accountRevBase`), `update_client` |
| `POST /v1/account/recover` | recovery token only | `RecoveryResetReq` | `bad_request`, `unauthorized` (wrong `recoveryAuthKey`, or no/expired token), `forbidden` (a normal session token), `rate_limited`, `conflict`, `update_client` |
| `PUT /v1/account/recovery` | bearer | `RecoveryUpdateReq {authKey, recoveryAuthKey, secretByRecovery}` | `bad_request`, `unauthorized` (wrong `authKey`), `rate_limited`, `conflict`, `update_client` |

* **Read.** `GET /v1/account` returns the same material for a device session
  and for the recovery token: a client that is recovering needs
  `secretByRecovery` to unwrap the account secret.
* **Change password.** The body carries `oldAuthKey` (32 bytes), `newAuthKey`
  (32 bytes), the new `kdf`, the re-wrapped `secretByKek` (73 bytes) and
  `accountRevBase`, the `accountRev` the client based the change on. The old
  key is checked like a sign-in: against the AuthKey verifier, in constant time,
  and a wrong key answers `401 unauthorized`, counts toward the same lockout
  as failed logins (ten consecutive failures lock the account for 15 minutes,
  `429 rate_limited` even for the right key), and a success clears the count.
  The verifier is read together with `accountRev`, and a base that is not
  that revision is `409 conflict` before any hashing, so a dead key can never
  be checked against a newer verifier. Then, in one `BEGIN IMMEDIATE`
  transaction, the update requires `accountRev = accountRevBase` again
  (`409 conflict` if it moved meanwhile) and the verifier is replaced (fresh random 16-byte salt), `kdf` and
  `secretByKek` are replaced and `accountRev` increases by one.
  `kdf` is validated like the one at registration (`m` 1..=1 GiB, at least
  `8p`; `t` 1..=16; `p` 1..=64; 16-byte salt) and `secretByKek` must be 73
  bytes, else `400 bad_request`; such a request costs no failure point. Records
  are not re-encrypted. **Other devices keep their sessions.**
* **Recovery reset.** `POST /v1/account/recover` takes the limited token from
  `recover-login` **and** re-proves the recovery key: `recoveryAuthKey` in the
  body is verified against the recovery verifier (same lockout counting).
  The effect on the account is that of a password change (`newAuthKey`, `kdf`,
  `secretByKek` replaced, `accountRev` + 1; the recovery wrap is unchanged,
  there is no `accountRevBase`: it conflicts only if the account changed while
  the key was being checked). Afterwards the recovery token, and any other
  recovery token of the account, is deleted: it cannot be used again (a second
  call is `401 unauthorized`). The token is accepted by `GET /v1/account` and
  this endpoint only; everything else answers it `403 forbidden`.
* **Replace the recovery key.** `PUT /v1/account/recovery` (a signed-in device)
  also proves the master password: `authKey` (32 bytes) is checked like
  `oldAuthKey` above (wrong key: `401`, counted toward the lockout), so a stolen
  session token alone cannot swap the recovery key. It then
  stores a verifier of the new `recoveryAuthKey` with a fresh salt and the new
  73-byte `secretByRecovery`, increases `accountRev` and deletes outstanding
  recovery tokens (they were earned with the old key). The old recovery key
  stops working immediately. Used when enabling sync on an existing vault
  creates a new recovery key.
* **Failure count.** A successful write resets the account's failed-sign-in
  count inside the same transaction, so the event below always follows a
  committed change.
* **Notification.** Each of the three writes publishes
  `account_changed {accountRev}` (the new revision) on the account's event hub
  after the transaction commits, so every connected device hears it, the
  caller included. Failed calls publish nothing. A device that was offline
  sees the higher `accountRev` on its next `GET /v1/account` or sync.
* **Schema gating.** Every authenticated request whose token the endpoint
  accepts (a rejected scope records nothing; a database error while recording
  is logged, not returned) raises the account's stored `max_schema` to the request's `X-Tether-Schema`
  if that is higher. A *write* (`password`, `recover`, `recovery` here, and
  `sync/push`) from a client whose schema is below `max_schema` is
  refused with `400 update_client` ("update Tether") before any key is checked
  or changed. Reads are always allowed. The check is
  `account::require_write_schema(conn, account_id, auth.schema)`, called again
  inside the write transaction.

## Records: changes and push

`server/src/sync.rs`. Both endpoints need a device session (the recovery
token is `403 forbidden`, a revoked device `401 revoked`). The server stores
envelopes as opaque bytes and never decides a winner.

`GET /v1/sync/changes?since=N&limit=M` returns `ChangesResp {changes, next,
more, epoch}`: rows with `seq > since`, ascending, as `WireRow`s with `rev` and
`seq` set (a tombstone has `deleted: true` and an empty envelope). `since`
defaults to 0 and must be >= 0; `limit` defaults to 500, is capped at 1000 and
must be >= 1 (otherwise `400 bad_request`). `next` is the last `seq` returned
(or `since` when the page is empty) and `more` says rows remain. `epoch` is the
database identity from `/v1/health`.

* **410 gone.** Tombstones older than 180 days are compacted away (once at
  start, then daily) and the vault's `compactedThrough` rises to the highest
  `seq` removed. A cursor with `0 < since < compactedThrough` is answered
  `410 gone`: the device downloads everything again and reconciles. `since=0`
  always means "everything that exists" and is never `410`, which is how a
  full resync (and a brand-new device) reads the vault.

`POST /v1/sync/push` takes `PushReq {items: [{row, baseRev}]}` (at most 500)
and answers `PushResp {results}`, one per item, in order. Everything runs in
one `BEGIN IMMEDIATE` transaction, item by item (later items see earlier ones):

* **Accepted** iff (`baseRev` null and no such record) or (`baseRev` equals the
  current `rev`): `rev` becomes `baseRev + 1` (1 for a new record), `seq` is
  the next vault counter; result `ok {id, rev, seq}`. A tombstone can be
  written over a live record and vice versa.
* **Idempotent retry.** Otherwise, if the current row has the same `hlc`,
  `envelope` and `deleted` as the item, the result is `ok` with the current
  `rev` and `seq` (a lost response can be retried safely). Nothing is written.
* **Conflict.** Otherwise `conflict {id, current}` with the server's row.
  Nothing is written for that item; other items in the request still are.
* `baseRev` set for a record the server does not have: `410 gone`, whole
  request rolled back. The device's view is ahead of this server's history (a
  server restored from an older backup) or the tombstone was compacted; either
  way the device does a full resync (`since=0`) and pushes again.

Whole-request errors (nothing is written): `400 bad_request` for more than 500
items, a record id that is not a lowercase hyphenated UUID, a `kind` outside
`[a-z_]{1,32}`, `keyVersion` 0, an envelope over 1 MiB, a tombstone with an
envelope, a live record with an empty envelope, `baseRev` below 1, or an `hlc`
not of the form `{wall_ms:016x}-{counter:08x}-{node:04x}` (lowercase hex);
`400 clock_skew` if any `hlc` wall time is more than one hour ahead of the
server's clock; `400 update_client` when the client's `X-Tether-Schema` is
below the account's high-water mark (reads are never gated). The 2 MiB body
limit (see Conventions) still applies (`413`), so large vaults are pushed in
several requests.

After a push with at least one `ok` result the server publishes `changed {seq:
head}` to the account's other devices (see WebSocket).

## Endpoint index

Auth: "none" = no bearer; "bearer" = access token; "recover" = the
limited-scope token from `recover-login`. The sections above describe each
endpoint; this is the whole surface. Unknown routes are `404 not_found`, wrong
methods `405` (rewritten to the standard error body).

| Method and path | Auth | Request | Response |
| --- | --- | --- | --- |
| `GET /v1/health` | none | | `{status, protocol, epoch}` |
| `POST /v1/prelogin` | none | `PreloginReq` | `PreloginResp` |
| `POST /v1/register` | none (invite token in body) | `RegisterReq` | `TokenResp` |
| `POST /v1/login` | none (link token optional) | `LoginReq` | `TokenResp` |
| `POST /v1/recover-login` | none | `RecoverReq` | `TokenResp` (limited scope) |
| `POST /v1/refresh` | none | `RefreshReq` | `TokenResp` |
| `POST /v1/logout` | bearer | | `204` |
| `GET /v1/devices` | bearer | | `[DeviceView]` |
| `PATCH /v1/devices/{id}` | bearer | `RenameDeviceReq` | `204` |
| `DELETE /v1/devices/{id}` | bearer | | `204` |
| `POST /v1/link` | bearer | | `LinkResp` |
| `GET /v1/account` | bearer or recover | | `AccountResp` |
| `POST /v1/account/password` | bearer | `PasswordChangeReq` | `AccountResp` |
| `PUT /v1/account/recovery` | bearer | `RecoveryUpdateReq` | `AccountResp` |
| `POST /v1/account/recover` | recover | `RecoveryResetReq` | `AccountResp` |
| `GET /v1/sync/changes?since=N&limit=500` | bearer | | `ChangesResp` |
| `POST /v1/sync/push` | bearer | `PushReq` | `PushResp` |
| `GET /v1/events` | bearer (subprotocol) | | WebSocket of `Event` |

## Client behaviour (`crates/core-sync`)

Informative: what the Termlyx client does with the contract above. Other
clients may differ as long as they obey the rules above.

* **Transport.** HTTPS only (plain `http://` only for loopback without a pin).
  With a pin in the join code the client accepts exactly the certificate
  whose public key hashes to the pin and nothing else; a different key is
  "server certificate changed" and sync stops until the user retries. Without
  a pin, normal WebPKI validation. Every request sends `X-Tether-Protocol`,
  `X-Tether-Schema` and, once it has one, `X-Tether-Device`.
* **Credentials.** The refresh token and device secret are kept together in
  the OS keychain (or, where no keychain works, in the vault database's
  `sync_state` table; not behind the vault key, so a locked device still
  syncs, and revoking the device kills them). A rotated refresh token is persisted **before** the client
  switches to it, so a crash cannot strand the device.
* **Sign in again.** When the session cannot be refreshed (`401 unauthorized`
  on `refresh`, typically after a server restore) sync stops with "sign in
  again" and the app asks for the master password (it also uses the password
  of the next successful unlock, then drops it): `prelogin` for the
  account's KDF parameters (bounded like any server-supplied parameters,
  and no cheaper than the ones the device stores),
  the AuthKey, then `login` without a link token, with `X-Tether-Device` and
  the stored device secret. The device keeps its id, node and records; a
  restored server's new `epoch` then triggers the full resync below, which
  pushes edits made after the backup again. A wrong password is "wrong
  password"; `revoked`, missing stored credentials or an unknown device
  (`403`) mean the device needs an add-device code.
* **Joining on a device that has a vault.** The add-device code is redeemed
  first (`login` with the link token, then `GET /v1/account`). If the
  account's `vaultId` is this device's own vault (sync was turned off, the
  device was revoked or lost its sign-in) the vault is reconnected: every
  record stays, the account's current material and the new node are
  installed, and a full resync starts (below). Another `vaultId` replaces
  the local vault only after the user confirmed it (the old file is kept as
  `<file>.before-join-<date>`); the app keeps the redeemed sign-in while it
  asks, since the code is single-use, and a refused join revokes the device
  it created.
* **A cycle.** Refresh the session, fetch the account material (on connect
  and after an `account_changed` event), pull `changes` pages of 500 until `more` is false,
  merge the queue, then push the outbox in requests of at most 500 items and
  about 1.5 MiB. Triggers: a `changed` event, a local edit (pushed 500 ms
  after the first of a burst), an unlock, "Sync now", and a 5 minute timer.
  The event socket reconnects with backoff (1 s up to 5 min, jittered) and
  every reconnect is followed by a pull.
* **Full resync.** A changed `epoch` (a restored server), `410 gone` on pull
  **or push**, a cursor ahead of the server's `next`, or a newer kind set
  forgets every `rev`, keeps the dirty flags, pulls from `since=0`, then
  pushes the live records the server lacks as new. A local record whose
  clock is newer than the server's copy is kept and pushed again, so a
  restore from an older backup does not discard later edits.
* **Conflicts.** The server never decides a winner: `conflict` push results
  and concurrent pulls are merged on the device by clock; the losing version
  is stored as a `conflict` record (`docs/crypto.md`, "Metadata visible to the
  server") and synced like any other record.
* **Errors shown to the user.** `revoked` is "device revoked" (sync stops),
  `update_client` is "update Tether", `clock_skew` says the device's clock is
  wrong, an unreachable server is "offline" and retried, a bad pin is
  "server certificate changed".
