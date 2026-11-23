# Termlyx vault cryptography (format v1)

Implemented by `crates/core-crypto` using RustCrypto crates only. These byte
layouts are persisted in the local vault and exchanged by Phase 2 sync (the
wire contract is `docs/protocol.md`), so they are frozen: any change needs a
new version byte, not an edit. Phase 2 only added derivations and labels
(`tether/recovery-auth/v1`, `tether/conflict-id/v1`) and bookkeeping tables;
no byte layout above changed.
Committed fixtures in `crates/core-crypto/tests/golden/` and known-answer tests
in `crates/core-crypto/tests/vectors.rs` fail on drift.

## Key hierarchy

```
master password
   │  Argon2id (m = 64 MiB, t = 3, p = 4, 16-byte salt) → master (32 bytes)
   ▼
HKDF-SHA256 (no salt, ikm = master)
   ├─ info "tether/auth/v1" → AuthKey (32)   server login (see "Server authentication")
   └─ info "tether/kek/v1"  → KEK (32)
                                │ wrap_secret
                                ▼
                  account X25519 secret ◄── also wrapped by RecoveryKey (32 random bytes)
                                │ public half
                                ▼
                  HPKE seal ──► VaultKey (32 random bytes) ──► seals every record envelope
```

* The master password and master key never leave memory; only the KDF
  parameters (including the salt) are stored with the account.
* `AuthKey` and `KEK` are independent HKDF outputs, so a server that learns the
  `AuthKey` learns nothing about the `KEK`.
* The recovery key is 32 uniformly random bytes used directly as a wrapping key
  (no KDF stretching needed for a uniform key).
* The VaultKey is random, so rotating the password re-wraps only the account
  secret; records are not re-encrypted.

## Argon2id and HKDF

* Argon2id, version 0x13, output 32 bytes, no secret key and no associated
  data. Defaults: `m_kib = 65536`, `t = 3`, `p = 4`, salt 16 random bytes.
  `KdfParams` is stored per account; callers may raise it later.
* `master = Argon2id(password_utf8, salt)`.
* `PRK = HKDF-Extract(salt = none, IKM = master)` (RFC 5869: a zero-filled
  HashLen salt). `AuthKey = HKDF-Expand(PRK, "tether/auth/v1", 32)` and
  `KEK = HKDF-Expand(PRK, "tether/kek/v1", 32)`.
* The Argon2 stage is checked against the RFC 9106 section 5.3 Argon2id vector
  (which uses the optional secret and associated-data inputs) through the same
  internal builder that `derive_master` uses. The HKDF split has a pinned
  known-answer test cross-checked with an independent HMAC-SHA256.

## Record envelope

XChaCha20-Poly1305 (192-bit nonce, 128-bit tag).

```
offset  size  field
0       1     version = 0x01
1       24    nonce (random per seal, OS CSPRNG)
25      n+16  ciphertext (n bytes) followed by Poly1305 tag (16 bytes)
```

Total length is `41 + n`. `open` checks, in order: length >= 41 else
`Format`; byte 0 == 0x01 else `BadVersion`; then authenticates, and any failure
(wrong key, wrong AAD, flipped bit anywhere after the version byte) is
`Decrypt`.

### Associated data

```
AAD = utf8(id) '|' utf8(vault_id) '|' utf8(kind) '|' decimal(key_version)
```

For example `0198…4a5b|0198…0001|host|1`. This binds a ciphertext to its record,
vault, type and key generation, so envelopes cannot be moved between rows.

The encoding is unambiguous only if no text field contains `|`. Ids are UUIDs
and kinds are fixed snake_case words, so this holds by construction; the code
enforces it rather than assuming it: `seal` panics and `open` returns
`Format` if `id`, `vault_id` or `kind` contains `|`. (`key_version` is decimal
digits, so it cannot.) Empty fields are allowed (`id||wrap|1`).

### Wrapped secrets

`wrap_secret(kek, secret, label)` is an ordinary envelope over the 32 secret
bytes (total 73 bytes) with

```
AAD = label '|' '' '|' "wrap" '|' "1"      i.e.  "{label}||wrap|1"
```

(`id = label`, empty `vault_id`, `kind = "wrap"`, `key_version = 1`). The label
names what is being wrapped (e.g. `account-key`) so a wrapped blob cannot be
substituted for another. `unwrap_secret` rejects plaintexts that are not 32
bytes with `Format`.

## Account key and recovery key

* Account key: X25519 (RFC 7748), 32-byte private and public keys. The private
  key is 32 random bytes; the public key is derived with the HPKE KEM's
  `sk_to_pk`. It is stored twice, wrapped by the KEK and by the recovery key.
* Recovery key text form: the 32 bytes as RFC 4648 base32, no padding,
  uppercase, 52 characters, split into 13 groups of 4 joined by `-`
  (`ABCD-EFGH-…`, 64 characters). Parsing ignores whitespace and `-`, accepts
  lower case, and returns `Format` for non-alphabet characters or any length
  other than 52 significant characters.

## Server authentication

Added in Phase 2 (sync). Additive: no byte layout above changes.

* **AuthKey** (`HKDF-Expand(PRK, "tether/auth/v1", 32)`, above) is the only
  password-derived value a client sends to a sync server, and only over TLS.
  The server never receives the master password, the master key, the KEK, the
  VaultKey, the account secret or the recovery key. Because AuthKey is already
  32 uniformly random bytes, the server stores a verifier, `Argon2id(AuthKey,
  server_salt)` at modest cost, and compares in constant time. A stolen server
  database therefore yields neither a usable AuthKey nor anything that opens a
  record.
* **RecoveryAuthKey** lets a device that has only the recovery key prove it
  to the server (fetch the recovery-wrapped account secret, then set a new
  password):

  ```
  PRK             = HKDF-Extract(salt = none, IKM = recovery_key)
  RecoveryAuthKey = HKDF-Expand(PRK, "tether/recovery-auth/v1", 32)
  ```

  Same HKDF-SHA256 construction as the split above (`core_crypto::
  recovery_auth_key`). The server stores a verifier of it, like the AuthKey
  verifier. The derivation is one-way, so the server cannot recover the
  recovery key from it, and the recovery key (which unwraps
  `account_secret.recovery`) is never sent. The known answer for
  `recovery_key = [0x42; 32]` is
  `35ae02c78ba0576e6d622f54e2d3fe90a00924a87ef1351fe43a7fe6fc79cfc5`,
  computed with an independent HMAC-SHA256 in `tests/vectors.rs`.
* **KDF parameters from the server are hostile.** The server returns the
  account's Argon2 parameters at login; a client enforces the bounds in
  "Bounds" below before deriving anything.

## Metadata visible to the server

Records reach the server only as envelopes, but the sync protocol needs some
cleartext to route and order them. The server sees, per record: the record
`id`, `kind`, `key_version`, the hybrid logical clock (which includes the
writing device's node id and a wall-clock time), the envelope size, server
timestamps, and the tombstone flag. It also sees account and device ids and
names, usernames, and when each device syncs. This is an accepted leak.

Conflict records (kind `conflict`) have a deterministic id, so two devices
that detect the same conflict write one record: the first 16 bytes of
`SHA-256("tether/conflict-id/v1" || 0x00 || record_id (16 UUID bytes) ||
losing_hlc text)`, stamped as a UUIDv8. Both inputs are already visible
routing metadata, so the server can tell which record a conflict is about
(it can also see that from the push conflict itself); the losing version
stays inside its original envelope, nested in the conflict's.

Not visible: record contents, host labels and addresses, usernames and
passwords stored in the vault, keys, and snippet or note text; these exist only
inside envelopes sealed under the VaultKey. The account key material the
server stores (`vault_id`, `key_version`, `kdf`, `account_public`,
`account_secret.kek`, `account_secret.recovery`, `vault_key.sealed`) is public
or already wrapped. `account_secret.device` and `quick_unlock.*` are
device-local and never leave the machine.

### What a dishonest server can do

The routing metadata is not covered by any key, and a tombstone is an empty
envelope. So whoever controls the sync server (or its database, or can alter
its traffic) cannot read records, but can:

- mark records deleted;
- serve an older version of a record again (for example a known host you
  removed), since the clock and revision are not inside the envelope;
- withhold records or changes.

It cannot replace a record with content of its own: every device opens each
live envelope from the server before storing it, and one that does not open
under the VaultKey (altered, garbage, or another record's envelope) is not
stored. The local copy is kept, and an unpushed local change goes out on top
of it. These rows are counted in the merge report and logged by record id.

Run your own server where only you control it (see
[Self-hosting](self-host.md)) and keep backups of its data
([Backup and restore](self-host.md#7-backup-and-restore)). A later format
version will seal tombstones and bind the clock to the envelope, so deletes
and replays can be checked too.

## Sealed vault key (HPKE)

RFC 9180 base mode. Suite: DHKEM(X25519, HKDF-SHA256) (KEM id 0x0020),
HKDF-SHA256 (KDF id 0x0001), ChaCha20-Poly1305 (AEAD id 0x0003).

```
info = "tether/vault/v1|" + vault_id     (UTF-8)
aad  = empty
```

```
offset  size  field
0       1     version = 0x01
1       32    enc  (ephemeral X25519 public key)
33      48    ciphertext (32-byte vault key) + Poly1305 tag (16)
```

Total 81 bytes. Sealing is fallible: a recipient key that yields an all-zero
Diffie-Hellman output (low-order points such as 32 zero bytes) is rejected with
`Format`, per RFC 9180. `open_vault_key` returns `Format` for a wrong length or a
malformed `enc`, `BadVersion` for a first byte other than 0x01, and `Decrypt`
for the wrong recipient, the wrong `vault_id` (it is bound through `info`), or
any tampering. A new ephemeral key is generated per seal, so sealing the same
vault key twice gives different output.

## Memory handling

Keys are held in `secrecy::SecretBox<[u8; 32]>` (zeroized on drop, `Debug`
prints `[REDACTED]`); decrypted plaintext is returned in
`zeroize::Zeroizing<Vec<u8>>`, and intermediate key and master buffers are
`Zeroizing`. Plain `[u8; 32]` is used only at function boundaries where a
borrowed key is needed.

## Local account record

Implemented by `crates/core-api/src/vault/`. The account lives in the
`core-store` key/value table (`account`), one row per field. Every value is
public or already wrapped; nothing in it opens without the password, the
recovery key or the keychain device key. `create` writes all rows in one
transaction, and a password reset rewrites `kdf` and `account_secret.kek` in
one transaction.

| key                       | bytes | content                                                                |
|---------------------------|-------|------------------------------------------------------------------------|
| `vault_id`                | 16    | vault id, UUIDv7 (RFC 9562 byte order)                                 |
| `key_version`             | 4     | u32 LE, `1`: the key version records are sealed under (AAD field)      |
| `hlc_node`                | 2     | u16 LE: HLC node id for this device's writes. Random when the vault is created; with sync, the server assigns it (the first device keeps its own, later devices get the lowest unused id) and it is rewritten on enable and on join |
| `kdf`                     | 28    | Argon2id `m_kib`, `t`, `p` (u32 LE each), then the 16-byte salt        |
| `account_public`          | 32    | X25519 account public key                                              |
| `account_secret.kek`      | 73    | `wrap_secret(KEK, account_secret, "account-secret/kek/{vault_id}")`    |
| `account_secret.recovery` | 73    | `wrap_secret(RecoveryKey, account_secret, "account-secret/recovery/{vault_id}")` |
| `vault_key.sealed`        | 81    | `seal_vault_key(account_public, VaultKey, vault_id)` (HPKE, above)     |
| `account_secret.device`   | 73    | optional, quick unlock: `wrap_secret(DeviceKey, account_secret, "account-secret/device/{vault_id}")` |
| `quick_unlock.biometry`   | any   | optional, macOS and iOS: the Touch ID or Face ID enrollment state (LocalAuthentication `evaluatedPolicyDomainState`) quick unlock was enabled or first used with; not secret |
| `pending.kdf`, `pending.account_secret.kek` | 28, 73 | optional, sync: a password wrap (and its `kdf`) from the server that has not opened on this device yet; same layouts as `kdf` and `account_secret.kek` |
| `pending.account_secret.recovery` | 73 | optional, sync: a recovery wrap from the server that has not opened on this device yet; same layout as `account_secret.recovery` |
| `quick_unlock.hello_challenge` | 32 | optional, the key-binding challenge (Windows and Android): the random challenge Windows Hello signs, or the Android Keystore key HMACs, to derive the wrapping key (see "Quick unlock on Windows", "Quick unlock on Android"); not secret |

`{vault_id}` is the hyphenated lowercase UUID, so each wrap is bound to its
slot and to its vault. Records are sealed with the VaultKey as described under
"Record envelope", with `key_version = 1` and `vault_id` as above.

* **Create.** `KdfParams::new_random()` (production cost, fresh salt), a new
  account keypair, recovery key and VaultKey; the recovery key's text is
  returned once and never stored.
* **Unlock.** Derive the KEK from the stored `kdf`, unwrap
  `account_secret.kek` (an authentication failure is "wrong password"; nothing
  is written), check the recomputed public key against `account_public`, open
  `vault_key.sealed`. If that wrap does not open and a `pending.*` wrap
  exists, try it the same way; when it opens the vault key it replaces
  `kdf` and `account_secret.kek` (one transaction) and the pending rows go.
* **Material from the server.** Another device's password change or
  recovery reset arrives as new wraps that this device cannot check (it does
  not know the new password or recovery key). They are stored as `pending.*`
  next to the known-good rows, never over them, so a bad or hostile server
  cannot lock the user out: the old password and recovery key keep opening
  this device until the new ones have opened it once. Writing a local wrap
  (password change, recovery, join) drops the matching pending rows; turning
  sync on or off drops them all. Rejoining an account installs the server's
  password wrap (the password just proved it) but keeps a differing recovery
  wrap as pending.
* **Recovery.** Unwrap `account_secret.recovery` (or, failing that, a
  pending one, adopted as for unlock), open the vault key to prove
  the chain, then rewrite `kdf` (new salt) and `account_secret.kek` under the
  new password. The recovery wrap and the device wrap are unchanged, since the
  account secret is.
* **Quick unlock.** A random 32-byte DeviceKey is kept in the OS keychain
  (service `app.tether.vault-device-key`, account = vault id) and wraps the
  account secret into `account_secret.device`. Unlock reads the DeviceKey and
  proceeds as for the password. A missing or failing keychain is an error;
  password unlock is unaffected.
* **Touch ID (macOS).** `VaultService` asks a presence gate before it reads
  the DeviceKey: LocalAuthentication `LAContext` with
  `DeviceOwnerAuthenticationWithBiometrics` (Touch ID only; the fallback
  button means "use the master password"). Enabling needs one Touch ID
  confirmation and stores the enrollment state in `quick_unlock.biometry`.
  On unlock, a different enrollment state (fingers added or removed, or
  occasionally a major macOS upgrade) turns quick unlock off without asking;
  biometry unavailable (no sensor, lid closed, locked out) is an error. A
  vault enabled before the gate existed records the state on its first
  Touch ID unlock. **Limit:** the gate is Termlyx's, not the keychain's. A
  `SecAccessControl` with `biometryCurrentSet` needs the data-protection
  keychain, which needs a signed build with a keychain-access-groups
  entitlement; builds are unsigned today. The DeviceKey stays a login-keychain
  item, so another process running as the user gets the item's ACL prompt,
  not a Touch ID prompt. Moving the item behind `SecAccessControl` is planned
  for when code signing lands. Windows uses Windows Hello (see "Quick unlock
  on Windows"), iOS Face ID or Touch ID (see "Quick unlock on iOS"); other
  platforms have no gate.
* **Lock.** The VaultKey and account secret are dropped (zeroized). No
  decrypted records are cached; every read decrypts from the store.
* **Bounds.** Stored Argon2 parameters are checked before use: `m_kib` at most
  1 GiB (`MAX_M_KIB = 1024 * 1024`), `t` in 1..=16, `p` in 1..=64
  (`crates/core-api/src/vault/service.rs`).

## Quick unlock on Windows

Local to the device; the v1 vault and sync formats are unchanged. Windows has
no keychain ACL prompt, so a DeviceKey stored in Credential Manager would be
readable by any program the user runs. Instead the key that wraps
`account_secret.device` is produced by Windows Hello itself, and nothing is
stored in Credential Manager for the vault: enabling deletes any old
DeviceKey entry first and fails, changing nothing, if that delete fails.

* **Credential.** One Windows Hello key credential (`KeyCredentialManager`,
  name `dev.tether.client.quick-unlock`) per Windows user, shared by all
  vaults and created by the first enable that finds none (only enabling
  creates it). Its private key never leaves Windows; using it is the Hello
  prompt (PIN, fingerprint or face, whichever is set up).
* **Enable.** `challenge` = 32 random bytes. Hello signs it, then

  ```
  PRK      = HKDF-Extract(salt = none, IKM = signature)
  HelloKey = HKDF-Expand(PRK, "tether quick unlock windows hello v1", 32)
  account_secret.device = wrap_secret(HelloKey, account_secret,
                                      "account-secret/device/{vault_id}")
  ```

  (HKDF-SHA256; `salt = none` is RFC 5869's zero-filled HashLen salt.) The
  challenge goes in `quick_unlock.hello_challenge`; `quick_unlock.biometry`
  is removed, in one transaction. Both rows stay in the local account table
  and are never synced.
* **Unlock.** Hello signs the stored challenge again, the HelloKey is
  re-derived and unwraps `account_secret.device`; the rest is as for the
  password. This relies on the credential's RSA PKCS#1 v1.5 signatures being
  deterministic: the same key and challenge give the same signature, hence
  the same HelloKey.
* **Reset.** A reset or re-created Hello credential signs to a different
  HelloKey, so the unwrap fails authentication. A credential that is gone at
  unlock is not re-created (that would be a useless create prompt) and
  counts the same, with no prompt at all. Termlyx then turns quick unlock off
  (deletes `account_secret.device` and `quick_unlock.hello_challenge`) and
  reports it once; the next quick unlock is "not enabled" without a prompt,
  and the master password works as always. A missing challenge row is
  treated the same way, and so is a vault with a challenge opened behind a
  gate that does not bind keys (it has no keychain DeviceKey to read).
  Turning off deletes its rows in one transaction; a failing keychain
  cleanup afterwards does not hide the reason. A cancelled prompt, or Hello
  being unavailable, is only an error: quick unlock stays on.
* **Exposure.** Reading the vault file gives the challenge and the wrapped
  secret, which open nothing without a Hello signature from this user's
  credential on this machine. The HelloKey and the signature exist only in
  memory during enable and unlock and are zeroized after use. Any process
  running as the same user can open `dev.tether.client.quick-unlock` and ask
  Windows Hello to sign the stored challenge (it is not secret). The Hello
  dialog does not clearly name Tether as the requester, so a user who
  approves such a prompt hands that process the HelloKey; this is the
  Windows counterpart of the Mac keychain ACL prompt above.

## Quick unlock on iOS

Local to the device; the v1 vault and sync formats are unchanged. iOS uses
the DeviceKey scheme above (`account_secret.device`, `quick_unlock.biometry`)
with a different keychain and the same kind of gate.

* **Keychain.** The DeviceKey (service `app.tether.vault-device-key`,
  account = vault id) is a generic password in the data-protection keychain
  (`apple-native-keyring-store`, `protected`), in the app's default keychain
  access group,
  with `kSecAttrAccessibleWhenUnlockedThisDeviceOnly`: readable only while
  the device is unlocked, never synced to iCloud Keychain, never restored
  to another device from a backup (`crates/core-api/src/vault/keychain.rs`).
* **Gate.** The macOS gate, unchanged: `LAContext` with
  `DeviceOwnerAuthenticationWithBiometrics` (biometry only, no device
  passcode; the fallback button means "use the master password"). The
  device's `biometryType` names it Face ID or Touch ID; one it cannot name
  still asks, and fails if it cannot (quick unlock never skips the check on
  iOS). Enabling stores the enrollment state in `quick_unlock.biometry`;
  added or removed faces or fingers (or occasionally a major iOS upgrade)
  turn quick unlock off at the next unlock, without a prompt.
* **No `SecAccessControl`.** The item is not additionally bound with a
  biometric `SecAccessControl` (`biometryCurrentSet`); the gate is Tether's,
  as on macOS. On iOS this costs little: the macOS gap (another process
  running as the user can ask for a login-keychain item) does not exist,
  because only apps in Tether's access group can read the item, so the gate
  stands between the key and Tether alone. Binding it would add a second,
  keychain-driven prompt path (the keyring store only offers user presence,
  which also accepts the device passcode, weaker than biometry only) for
  the one case it would cover: code running inside Tether's own process or
  on a jailbroken device, which could read the unlocked vault anyway.
* **Exposure.** Reading the vault file gives the wrapped secret, which opens
  nothing without the DeviceKey from this device's keychain.

## Quick unlock on Android

Local to the device; the v1 vault and sync formats are unchanged. Android
uses the key-binding scheme of Windows (no DeviceKey is stored anywhere, so
nobody holding the unlocked phone can read one): the key that wraps
`account_secret.device` comes from an Android Keystore key that only a
strong biometric can use.

* **Key.** One Keystore key (alias `dev.tether.client.quick-unlock`) per
  install, shared by all vaults: HMAC-SHA256, `PURPOSE_SIGN`, user
  authentication required for every use with `AUTH_BIOMETRIC_STRONG` only
  (no PIN, pattern or password), invalidated when biometrics are enrolled or
  removed. StrongBox when the device has one, else the TEE; never
  exportable. Only enabling creates it, replacing an invalidated one.
* **Enable.** `challenge` = 32 random bytes. The system biometric prompt
  (`BiometricPrompt`, `BIOMETRIC_STRONG`, a `CryptoObject` holding the
  `Mac`) authorises one use of the key, which computes
  `mac = HMAC-SHA256(key, challenge)`, then

  ```
  PRK         = HKDF-Extract(salt = none, IKM = mac)
  KeystoreKey = HKDF-Expand(PRK, "tether quick unlock android keystore v1", 32)
  account_secret.device = wrap_secret(KeystoreKey, account_secret,
                                      "account-secret/device/{vault_id}")
  ```

  (HKDF-SHA256, as on Windows.) The challenge goes in the key-binding
  challenge row (the row keeps its name, `quick_unlock.hello_challenge`);
  `quick_unlock.biometry` is removed, in one transaction. Nothing else is
  stored.
* **Unlock.** Prompt, HMAC the stored challenge again (HMAC is
  deterministic), re-derive, unwrap; the rest is as for the password.
* **Invalidation.** A fingerprint or face enrolled or removed since enabling
  makes Android invalidate the key for good; Tether sees it when it prepares
  the `Mac`, before any prompt, as it sees a key that is gone. Either way
  quick unlock is turned off and reported once, exactly as for a reset Hello
  credential above; the master password works, and enabling again makes a
  new key. A cancel (or "Use master password"), too many attempts, or no
  strong biometrics right now is only an error: quick unlock stays on.
* **Exposure.** Reading the vault file gives the challenge and the wrapped
  secret, which open nothing without the Keystore key on this device and a
  matching biometric. The MAC and the KeystoreKey exist only in memory
  during enable and unlock (the Rust copies are zeroized after use) and are
  never logged; the challenge and MAC cross the Kotlin/Rust bridge as base64
  in the plugin call and its reply, never in an intent or URL. When the
  app's plugin cannot be reached, quick unlock is unavailable; it never
  falls back to an ungated keychain DeviceKey.

## Local sync bookkeeping

Added in Phase 2 (`crates/core-store`, schema v2; a v1 vault upgrades in
place). Nothing here is plaintext record data or key material, and nothing
here is password-derived.

* `records` gains no new secrets: a record is still one sealed envelope, plus
  its `hlc`, the server `rev` it was last based on and a `dirty` flag.
* `inbound` holds rows pulled from the server **exactly as delivered**: the
  sealed envelope plus the routing metadata, filled even while the vault is
  locked and decrypted only when it is applied after an unlock.
* `sync_state` is a key/value table of engine bookkeeping: whether sync is on,
  the server URL and pin, the account name, the device id, the pull cursor and
  server epoch, the last seen `accountRev`, resync markers, and (only where no
  OS keychain works) the device's refresh token and device secret. Those two
  are server credentials, not vault keys: they are not behind the vault key (a
  locked device must keep syncing), they open nothing in the vault, and
  revoking the device kills them. Normally they are in the OS keychain.
* **iOS: a locked phone is not a missing keychain.** On iOS the sync tokens
  (service `app.tether.sync-token`) use the same
  `WhenUnlockedThisDeviceOnly` item as the DeviceKey, so with the phone
  locked (the app kept alive in the background) every keychain access fails
  with `errSecInteractionNotAllowed`. That error is transient
  (`SyncError::StorageLocked`) and never moves the tokens into `sync_state`:
  the engine pauses, and before any refresh it checks that the item is
  readable, so the server never rotates the refresh token into a value this
  device could not store. A rotation the lock still interrupts (between that
  check and the save) is kept in memory and saved first once the keychain
  answers. Sync resumes when Tether comes to the front, the vault unlocks,
  "Sync now" or at the next 5-minute retry. Only a keychain that does not
  work at all (other platforms) still falls back to `sync_state`.
* **Cascade stash** (`sync_state` keys `cascade/<owner>/<dependent>`). Deleting
  a host or group also deletes the forward rules and own login that hang off
  it. With sync on, each cascaded dependent's last live row is kept so it can
  be revived if the owner's delete later loses to an edit on another device.
  An entry is **a sealed envelope and its routing metadata only** (kind, key
  version, clock, server `rev` and the cascade tombstone's clock): the same
  bytes that were already in `records`, never decrypted, never re-encrypted.
  Entries are dropped when the delete is acknowledged by the server, when the
  dependent is written again, when the owner is deleted again, and when sync is
  turned off.
* Conflict records (kind `conflict`) are ordinary sealed records: the losing
  version stays inside its original envelope, nested in the conflict's, and is
  kept until the user settles the conflict.
* **Turning sync off** clears `sync_state`, `inbound` and the stash and keeps
  every record.

## Threat-model notes

* Argon2 parameters are read from stored account data and, with sync, from
  the server's `GET /v1/account`. A caller must treat parameters from an
  untrusted source (the sync server) as hostile: unbounded `m_kib` is a
  memory-exhaustion vector, so bounds are enforced where the parameters are
  loaded (`m_kib` at most 1 GiB, `t` 1..=16, `p` 1..=64), by the client before
  deriving anything and by the server before storing them.
* **No KDF downgrade.** `prelogin` is unauthenticated, so a hostile server
  could ask for `m = 8 KiB, t = 1` and get an AuthKey that is cheap to
  brute-force. The client refuses (`SyncError::WeakKdf`, nothing derived or
  sent) parameters from a server (prelogin, account material, recovery) whose
  `m_kib`, `t` or `p` is below its floor: the cost it creates accounts with
  (the defaults above), raised to the stored account's where higher
  (`VaultService::kdf_floor`).
* **Revocation does not re-key.** A revoked device is cut off from the server
  but keeps the VaultKey it already holds; records written afterwards are
  sealed under the same key (`key_version` stays 1). Rotating the VaultKey
  (a new `key_version`, every record re-sealed) is not implemented; the format
  has room for it through the `key_version` AAD field.
* **Password change** re-wraps only the account secret (`kdf`,
  `account_secret.kek`); the VaultKey, the recovery wrap and every envelope
  are unchanged, which is why other devices can adopt a new password without
  re-downloading anything.
