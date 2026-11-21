//! The vault's side of the sync account flows (spec "Password change and
//! recovery"): the key material a server holds for the account, a password
//! change that can be pushed, installing material another device changed,
//! joining a vault from server material, and the recovery-key paths.
//!
//! Nothing here talks to the network; `core-sync` calls these around its
//! requests, so a server refusal never leaves the local account half changed
//! (prepare, ask the server, then commit). Everything that leaves through
//! [`AccountMaterial`] is already wrapped (`docs/crypto.md`, "Local account
//! record"); the device wrap (`account_secret.device`) and `quick_unlock.*`
//! never do.
//!
//! Material from a server is hostile input: [`validate_material`] bounds the
//! KDF parameters (no cheaper than this device's floor,
//! [`VaultService::kdf_floor`]) and checks every size before anything derives
//! or writes.

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use core_crypto::{
    CryptoError, KdfParams, MasterKeys, derive_master, new_recovery_key, parse_recovery_key,
    recovery_auth_key, unwrap_secret, wrap_secret,
};
use core_model::{Hlc, Id};
use core_proto::{AccountMaterial, KdfWire};
use core_store::Store;
use secrecy::{ExposeSecret, SecretBox, SecretString};

use super::service::{
    Account, KEY_VERSION, MAX_M_KIB, MAX_P, MAX_T, decode_kdf, encode_kdf, keys, open_account,
    wrap_label,
};
use super::{VaultError, VaultService};

/// `sync_state` key: the server `accountRev` the local material matches
/// (decimal text). Written together with the material it describes.
pub const SYNC_ACCOUNT_REV: &str = "account_rev";

/// Sizes of the wrapped pieces (`docs/crypto.md`).
const PUBLIC_LEN: usize = 32;
const WRAP_LEN: usize = 73;
const SEALED_LEN: usize = 81;
const SALT_LEN: usize = 16;

/// A password change computed by
/// [`prepare_password_change`](VaultService::prepare_password_change): the
/// account material with the new KDF parameters and KEK wrap, plus the old
/// and new AuthKeys the server needs (`PasswordChangeReq`).
pub struct PasswordChanged {
    /// The material after the change.
    pub material: AccountMaterial,
    /// AuthKey of the current password (proves it to the server).
    pub old_auth: SecretBox<[u8; 32]>,
    /// AuthKey of the new password (the server's new verifier).
    pub new_auth: SecretBox<[u8; 32]>,
}

impl fmt::Debug for PasswordChanged {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PasswordChanged")
            .field("material", &self.material)
            .finish_non_exhaustive()
    }
}

/// A new recovery key, not yet installed ([`VaultService::new_recovery`]).
pub struct NewRecovery {
    /// The key's text form: show it to the user once, it is stored nowhere.
    pub text: SecretString,
    /// Its `RecoveryAuthKey` (the server's recovery verifier).
    pub auth: SecretBox<[u8; 32]>,
    /// The account secret wrapped by the new key (73 bytes).
    pub wrap: Vec<u8>,
    vault_id: Id,
}

impl fmt::Debug for NewRecovery {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NewRecovery").finish_non_exhaustive()
    }
}

/// A password reset with the recovery key, not yet committed: the request
/// fields for `POST /v1/account/recover` ([`recovery_reset_for`]).
pub struct RecoveryReset {
    /// `RecoveryAuthKey` of the recovery key (re-proved to the server).
    pub recovery_auth: SecretBox<[u8; 32]>,
    /// AuthKey of the new password.
    pub new_auth: SecretBox<[u8; 32]>,
    /// New KDF parameters (fresh salt).
    pub kdf: KdfWire,
    /// The account secret wrapped by the new password's KEK.
    pub secret_by_kek: Vec<u8>,
    vault_id: Id,
    secret: SecretBox<[u8; 32]>,
}

impl fmt::Debug for RecoveryReset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RecoveryReset")
            .field("kdf", &self.kdf)
            .finish_non_exhaustive()
    }
}

/// KDF parameters in wire form.
pub fn kdf_to_wire(p: &KdfParams) -> KdfWire {
    KdfWire {
        m_kib: p.m_kib,
        t: p.t,
        p: p.p,
        salt: p.salt.to_vec(),
    }
}

/// Checks KDF parameters from a server (`m_kib` <= 1 GiB and >= 8p, `t`
/// 1..=16, `p` 1..=64, a 16-byte salt) before anything derives with them.
/// Memory, passes and lanes must each be at least `floor`'s
/// ([`VaultService::kdf_floor`]), else [`VaultError::WeakKdf`]: a server
/// asking for less wants an AuthKey it can brute-force cheaply.
pub fn validate_kdf(w: &KdfWire, floor: &KdfParams) -> Result<KdfParams, VaultError> {
    let bad = VaultError::BadMaterial("kdf parameters out of range");
    if w.t == 0 || w.t > MAX_T || w.p == 0 || w.p > MAX_P {
        return Err(bad);
    }
    if w.m_kib > MAX_M_KIB || u64::from(w.m_kib) < 8 * u64::from(w.p) {
        return Err(bad);
    }
    if w.m_kib < floor.m_kib || w.t < floor.t || w.p < floor.p {
        return Err(VaultError::WeakKdf);
    }
    let salt: [u8; SALT_LEN] = w.salt.as_slice().try_into().map_err(|_| bad)?;
    Ok(KdfParams {
        m_kib: w.m_kib,
        t: w.t,
        p: w.p,
        salt,
    })
}

/// Checks account material from a server: a hyphenated UUID vault id, the
/// key version this build writes, KDF bounds and `floor` ([`validate_kdf`])
/// and the sizes of every wrapped piece. Writes nothing.
pub fn validate_material(m: &AccountMaterial, floor: &KdfParams) -> Result<(), VaultError> {
    account_from_material(m, 0, floor).map(|_| ())
}

/// The account record `m` describes, validated against `floor`, with HLC
/// node `node`.
fn account_from_material(
    m: &AccountMaterial,
    node: u16,
    floor: &KdfParams,
) -> Result<Account, VaultError> {
    let vault_id = Id::parse_str(&m.vault_id)
        .ok()
        .filter(|id| id.to_string() == m.vault_id)
        .ok_or(VaultError::BadMaterial("vault id"))?;
    if m.key_version != KEY_VERSION {
        return Err(VaultError::BadMaterial("unsupported key version"));
    }
    let kdf = validate_kdf(&m.kdf, floor)?;
    let public: [u8; PUBLIC_LEN] = m
        .account_public
        .as_slice()
        .try_into()
        .map_err(|_| VaultError::BadMaterial("account public key"))?;
    let size = |b: &[u8], n: usize, what: &'static str| {
        if b.len() == n {
            Ok(b.to_vec())
        } else {
            Err(VaultError::BadMaterial(what))
        }
    };
    Ok(Account {
        vault_id,
        key_version: m.key_version,
        node,
        kdf,
        public,
        secret_by_kek: size(&m.secret_by_kek, WRAP_LEN, "password wrap")?,
        secret_by_recovery: size(&m.secret_by_recovery, WRAP_LEN, "recovery wrap")?,
        sealed_vault_key: size(&m.sealed_vault_key, SEALED_LEN, "sealed vault key")?,
        secret_by_device: None,
    })
}

fn material_of(a: &Account) -> AccountMaterial {
    AccountMaterial {
        vault_id: a.vault_id.to_string(),
        key_version: a.key_version,
        kdf: kdf_to_wire(&a.kdf),
        account_public: a.public.to_vec(),
        secret_by_kek: a.secret_by_kek.clone(),
        secret_by_recovery: a.secret_by_recovery.clone(),
        sealed_vault_key: a.sealed_vault_key.clone(),
    }
}

/// Derives the master keys for `password` and unwraps the account secret.
fn open_with_password(
    acct: &Account,
    password: &SecretString,
) -> Result<(MasterKeys, SecretBox<[u8; 32]>), VaultError> {
    let master = derive_master(password, &acct.kdf)?;
    let secret = unwrap_secret(
        master.kek.expose_secret(),
        &acct.secret_by_kek,
        &wrap_label("kek", acct.vault_id),
    )
    .map_err(|e| match e {
        CryptoError::Decrypt => VaultError::WrongPassword,
        e => VaultError::Crypto(e),
    })?;
    Ok((master, secret))
}

/// A 32-byte key held in a zeroizing box.
type Key = SecretBox<[u8; 32]>;

/// Unwraps the account secret with the recovery key `text`; returns the
/// recovery key and the secret.
fn open_with_recovery(vault_id: Id, wrapped: &[u8], text: &str) -> Result<(Key, Key), VaultError> {
    let key = parse_recovery_key(text).map_err(|_| VaultError::WrongRecoveryKey)?;
    let secret = unwrap_secret(
        key.expose_secret(),
        wrapped,
        &wrap_label("recovery", vault_id),
    )
    .map_err(|e| match e {
        CryptoError::Decrypt => VaultError::WrongRecoveryKey,
        e => VaultError::Crypto(e),
    })?;
    Ok((key, secret))
}

/// The rows of an unproven password wrap from the server.
const PENDING_KEK: [&str; 2] = [keys::PENDING_KDF, keys::PENDING_SECRET_BY_KEK];

fn delete_rows(store: &Store, rows: &[&str]) -> Result<(), VaultError> {
    for k in rows {
        store.account_delete(k)?;
    }
    Ok(())
}

/// Drops every unproven wrap from the server: it belongs to a sync account
/// this vault is no longer (or newly) attached to, so adopting it later
/// would make the local password disagree with the server's.
pub(super) fn drop_pending(store: &Store) -> Result<(), VaultError> {
    delete_rows(store, &PENDING_KEK)?;
    Ok(store.account_delete(keys::PENDING_SECRET_BY_RECOVERY)?)
}

/// Writes the whole account record (inside a transaction); nothing stays
/// pending.
fn write_account(store: &Store, a: &Account) -> Result<(), VaultError> {
    drop_pending(store)?;
    store.account_put(keys::VAULT_ID, a.vault_id.as_bytes())?;
    store.account_put(keys::KEY_VERSION, &a.key_version.to_le_bytes())?;
    store.account_put(keys::HLC_NODE, &a.node.to_le_bytes())?;
    store.account_put(keys::KDF, &encode_kdf(&a.kdf))?;
    store.account_put(keys::ACCOUNT_PUBLIC, &a.public)?;
    store.account_put(keys::SECRET_BY_KEK, &a.secret_by_kek)?;
    store.account_put(keys::SECRET_BY_RECOVERY, &a.secret_by_recovery)?;
    store.account_put(keys::SEALED_VAULT_KEY, &a.sealed_vault_key)?;
    Ok(())
}

/// Computes a password reset from server material `m` with the recovery key
/// `recovery`: unwraps the account secret from `m`'s recovery wrap (proving
/// the whole key chain opens), then wraps it under `new_password` with
/// `kdf`. `m`'s KDF cost must be at least `kdf`'s. Writes nothing; used on a
/// device without a vault (recovery on a new device) and by
/// [`VaultService::prepare_recovery_reset`]. Runs Argon2: blocking.
pub fn recovery_reset_for(
    m: &AccountMaterial,
    recovery: &str,
    new_password: &SecretString,
    kdf: KdfParams,
) -> Result<RecoveryReset, VaultError> {
    let acct = account_from_material(m, 0, &kdf)?;
    let (key, secret) = open_with_recovery(acct.vault_id, &acct.secret_by_recovery, recovery)?;
    open_account(&acct, &secret).map_err(|_| VaultError::BadMaterial("keys do not match"))?;
    let master = derive_master(new_password, &kdf)?;
    let secret_by_kek = wrap_secret(
        master.kek.expose_secret(),
        secret.expose_secret(),
        &wrap_label("kek", acct.vault_id),
    );
    Ok(RecoveryReset {
        recovery_auth: recovery_auth_key(key.expose_secret()),
        new_auth: master.auth_key,
        kdf: kdf_to_wire(&kdf),
        secret_by_kek,
        vault_id: acct.vault_id,
        secret,
    })
}

/// `<db file>.before-join-YYYY-MM-DD` next to `db`, with `-2`, `-3`, ...
/// appended if that name is taken.
fn backup_path(db: &Path) -> PathBuf {
    let name = db
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "vault.db".to_owned());
    let base = format!("{name}.before-join-{}", utc_date());
    let first = db.with_file_name(&base);
    if !first.exists() {
        return first;
    }
    (2..)
        .map(|n| db.with_file_name(format!("{base}-{n}")))
        .find(|p| !p.exists())
        .expect("some suffix is free")
}

/// Today's UTC date as `YYYY-MM-DD` (civil-from-days, H. Hinnant).
fn utc_date() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let z = i64::try_from(secs / 86_400).unwrap_or(0) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

impl VaultService {
    /// The vault id, once a vault exists.
    pub fn vault_id(&self) -> Option<Id> {
        self.vault_id
    }

    /// This device's HLC node id.
    pub fn node(&self) -> Result<u16, VaultError> {
        Ok(self.load_account()?.node)
    }

    /// Sets this device's HLC node id (the server assigns one per device so
    /// two devices never mint equal HLCs). Later edits use it at once.
    pub fn set_node(&mut self, node: u16) -> Result<(), VaultError> {
        self.load_account()?;
        self.store
            .account_put(keys::HLC_NODE, &node.to_le_bytes())?;
        if let Some(u) = self.unlocked.as_mut() {
            u.node = node;
            let last = u.last_hlc.get();
            u.last_hlc.set(Hlc { node, ..last });
        }
        Ok(())
    }

    /// The account's wrapped key material as a server holds it. Works locked.
    pub fn account_material(&self) -> Result<AccountMaterial, VaultError> {
        Ok(material_of(&self.load_account()?))
    }

    /// The cheapest KDF cost this device accepts from a server: the cost it
    /// creates accounts with (production: m = 64 MiB, t = 3, p = 4), raised
    /// to the stored account's where that is higher. The salt is not part of
    /// the floor (a password change elsewhere brings a new one).
    pub fn kdf_floor(&self) -> Result<KdfParams, VaultError> {
        let mut floor = (self.new_kdf)();
        match self.load_account() {
            Ok(acct) => {
                floor.m_kib = floor.m_kib.max(acct.kdf.m_kib);
                floor.t = floor.t.max(acct.kdf.t);
                floor.p = floor.p.max(acct.kdf.p);
            }
            Err(VaultError::NotInitialized) => {}
            Err(e) => return Err(e),
        }
        Ok(floor)
    }

    /// The server `accountRev` the local material matches, if known.
    pub fn account_rev(&self) -> Result<Option<i64>, VaultError> {
        Ok(self
            .store
            .sync_get(SYNC_ACCOUNT_REV)?
            .and_then(|v| std::str::from_utf8(&v).ok()?.parse().ok()))
    }

    /// The AuthKey of `password`, derived with the stored KDF parameters.
    /// Checks the password first ([`VaultError::WrongPassword`]). Works
    /// locked; runs Argon2 (blocking).
    pub fn auth_key(&self, password: &SecretString) -> Result<SecretBox<[u8; 32]>, VaultError> {
        Ok(self.open_by_password(password)?.1.auth_key)
    }

    /// Opens the account secret with `password`: the stored wrap first, then
    /// the unproven one from the server ([`install_account_material`]).
    /// When that one opens and its secret opens the vault key, it replaces
    /// the stored wrap. Returns the account as it then stands. Blocking.
    ///
    /// [`install_account_material`]: Self::install_account_material
    pub(super) fn open_by_password(
        &self,
        password: &SecretString,
    ) -> Result<(Account, MasterKeys, Key), VaultError> {
        let acct = self.load_account()?;
        match open_with_password(&acct, password) {
            Err(VaultError::WrongPassword) => {}
            other => return other.map(|(master, secret)| (acct, master, secret)),
        }
        let get = |k| self.store.account_get(k);
        let (Some(kdf), Some(kek)) = (get(keys::PENDING_KDF)?, get(keys::PENDING_SECRET_BY_KEK)?)
        else {
            return Err(VaultError::WrongPassword);
        };
        let new = Account {
            kdf: decode_kdf(&kdf)?,
            secret_by_kek: kek,
            ..acct
        };
        // Server data: whatever keeps it from opening, the password is wrong.
        let (master, secret) =
            open_with_password(&new, password).map_err(|_| VaultError::WrongPassword)?;
        open_account(&new, &secret).map_err(|_| VaultError::WrongPassword)?;
        let store = &self.store;
        store.atomically(|| -> Result<(), VaultError> {
            store.account_put(keys::KDF, &kdf)?;
            store.account_put(keys::SECRET_BY_KEK, &new.secret_by_kek)?;
            delete_rows(store, &PENDING_KEK)
        })?;
        Ok((new, master, secret))
    }

    /// Opens the account secret with the recovery key `text`, like
    /// [`open_by_password`](Self::open_by_password): the stored recovery
    /// wrap first, then the unproven one from the server, which replaces the
    /// stored wrap once it opens the vault key. Returns the account as it
    /// then stands, the recovery key and the secret.
    pub(super) fn open_by_recovery(&self, text: &str) -> Result<(Account, Key, Key), VaultError> {
        let acct = self.load_account()?;
        match open_with_recovery(acct.vault_id, &acct.secret_by_recovery, text) {
            Err(VaultError::WrongRecoveryKey) => {}
            other => return other.map(|(key, secret)| (acct, key, secret)),
        }
        let Some(wrap) = self.store.account_get(keys::PENDING_SECRET_BY_RECOVERY)? else {
            return Err(VaultError::WrongRecoveryKey);
        };
        let (key, secret) = open_with_recovery(acct.vault_id, &wrap, text)
            .map_err(|_| VaultError::WrongRecoveryKey)?;
        open_account(&acct, &secret).map_err(|_| VaultError::WrongRecoveryKey)?;
        let store = &self.store;
        store.atomically(|| -> Result<(), VaultError> {
            store.account_put(keys::SECRET_BY_RECOVERY, &wrap)?;
            store.account_delete(keys::PENDING_SECRET_BY_RECOVERY)?;
            Ok(())
        })?;
        let acct = Account {
            secret_by_recovery: wrap,
            ..acct
        };
        Ok((acct, key, secret))
    }

    /// Computes a password change (fresh salt; only `account_secret.kek` is
    /// re-wrapped, records are not touched) without writing it. Commit it
    /// with [`commit_password_change`](Self::commit_password_change) once
    /// the server (if any) accepted it. Runs Argon2 twice (blocking).
    pub fn prepare_password_change(
        &self,
        old: &SecretString,
        new: &SecretString,
    ) -> Result<PasswordChanged, VaultError> {
        let (acct, old_master, secret) = self.open_by_password(old)?;
        let kdf = (self.new_kdf)();
        let new_master = derive_master(new, &kdf)?;
        let mut material = material_of(&acct);
        material.kdf = kdf_to_wire(&kdf);
        material.secret_by_kek = wrap_secret(
            new_master.kek.expose_secret(),
            secret.expose_secret(),
            &wrap_label("kek", acct.vault_id),
        );
        Ok(PasswordChanged {
            material,
            old_auth: old_master.auth_key,
            new_auth: new_master.auth_key,
        })
    }

    /// Writes a prepared password change (and, with sync on, the server's
    /// new `accountRev`) in one transaction. The session is kept: this is the
    /// device the user changed it on.
    pub fn commit_password_change(
        &mut self,
        change: &PasswordChanged,
        account_rev: Option<i64>,
    ) -> Result<(), VaultError> {
        let acct = self.load_account()?;
        let new = account_from_material(&change.material, acct.node, &self.kdf_floor()?)?;
        if new.vault_id != acct.vault_id {
            return Err(VaultError::BadMaterial("belongs to another vault"));
        }
        let store = &self.store;
        store.atomically(|| -> Result<(), VaultError> {
            store.account_put(keys::KDF, &encode_kdf(&new.kdf))?;
            store.account_put(keys::SECRET_BY_KEK, &new.secret_by_kek)?;
            delete_rows(store, &PENDING_KEK)?;
            if let Some(rev) = account_rev {
                store.sync_put(SYNC_ACCOUNT_REV, rev.to_string().as_bytes())?;
            }
            Ok(())
        })
    }

    /// Changes the master password locally (sync off): prepare and commit.
    /// Works locked or unlocked and keeps the current lock state. Blocking.
    pub fn change_password(
        &mut self,
        old: SecretString,
        new: SecretString,
    ) -> Result<PasswordChanged, VaultError> {
        let change = self.prepare_password_change(&old, &new)?;
        self.commit_password_change(&change, None)?;
        Ok(change)
    }

    /// Installs account material another device changed (password change or
    /// recovery), as revision `account_rev`. Works locked.
    ///
    /// Validates `m` first ([`validate_material`]; it must be this vault's,
    /// with the same account key). A revision not above the stored one is
    /// ignored. Nothing proves a server's wraps open (they cannot be checked
    /// without the new password or recovery key), so the stored ones are
    /// kept: a password wrap (with its `kdf`) or recovery wrap that differs
    /// is kept beside them as pending, and replaces them only once it opens
    /// the vault ([`open_by_password`](Self::open_by_password),
    /// [`open_by_recovery`](Self::open_by_recovery)). Until then the old
    /// password and recovery key keep working on this device. If the
    /// password wrap differs and the vault is unlocked, it locks (the user
    /// unlocks with the new password). Returns whether anything changed.
    pub fn install_account_material(
        &mut self,
        m: &AccountMaterial,
        account_rev: i64,
    ) -> Result<bool, VaultError> {
        let acct = self.load_account()?;
        let new = account_from_material(m, acct.node, &self.kdf_floor()?)?;
        if new.vault_id != acct.vault_id {
            return Err(VaultError::BadMaterial("belongs to another vault"));
        }
        if new.public != acct.public {
            return Err(VaultError::BadMaterial("account key differs"));
        }
        if self
            .account_rev()?
            .is_some_and(|stored| stored >= account_rev)
        {
            return Ok(false);
        }
        let kek_changed = new.kdf != acct.kdf || new.secret_by_kek != acct.secret_by_kek;
        let recovery_changed = new.secret_by_recovery != acct.secret_by_recovery;
        let store = &self.store;
        store.atomically(|| -> Result<(), VaultError> {
            if kek_changed {
                store.account_put(keys::PENDING_KDF, &encode_kdf(&new.kdf))?;
                store.account_put(keys::PENDING_SECRET_BY_KEK, &new.secret_by_kek)?;
            } else {
                delete_rows(store, &PENDING_KEK)?;
            }
            if recovery_changed {
                store.account_put(keys::PENDING_SECRET_BY_RECOVERY, &new.secret_by_recovery)?;
            } else {
                store.account_delete(keys::PENDING_SECRET_BY_RECOVERY)?;
            }
            store.sync_put(SYNC_ACCOUNT_REV, account_rev.to_string().as_bytes())?;
            Ok(())
        })?;
        if kek_changed {
            self.lock();
        }
        Ok(kek_changed || recovery_changed)
    }

    /// Creates this (empty, uninitialized) vault from server material `m`
    /// with HLC node `node`, after checking that `password` opens it. Leaves
    /// it unlocked. Runs Argon2 (blocking).
    pub fn join(
        &mut self,
        m: &AccountMaterial,
        password: SecretString,
        node: u16,
    ) -> Result<(), VaultError> {
        if self.vault_id.is_some() {
            return Err(VaultError::AlreadyInitialized);
        }
        let acct = account_from_material(m, node, &self.kdf_floor()?)?;
        let (_, secret) = open_with_password(&acct, &password)?;
        let unlocked = open_account(&acct, &secret)
            .map_err(|_| VaultError::BadMaterial("keys do not match"))?;
        let store = &self.store;
        store.atomically(|| write_account(store, &acct))?;
        self.vault_id = Some(acct.vault_id);
        self.set_unlocked(unlocked)
    }

    /// Replaces this device's vault with the synced one described by `m`
    /// (spec "Add-device code on a device that already has a vault"): checks
    /// `password` against `m`, copies the current database to
    /// `<file>.before-join-YYYY-MM-DD` next to it, then in one transaction
    /// deletes every record, the account and the sync state and writes the
    /// new account. Returns the backup's path. Leaves the vault unlocked.
    pub fn replace_with_join(
        &mut self,
        m: &AccountMaterial,
        password: SecretString,
        node: u16,
    ) -> Result<PathBuf, VaultError> {
        let old_id = self.vault_id.ok_or(VaultError::NotInitialized)?;
        let acct = account_from_material(m, node, &self.kdf_floor()?)?;
        let (_, secret) = open_with_password(&acct, &password)?;
        let unlocked = open_account(&acct, &secret)
            .map_err(|_| VaultError::BadMaterial("keys do not match"))?;
        let backup = backup_path(&self.store.path()?);
        self.store.copy_to(&backup)?;
        self.unlocked = None;
        let store = &self.store;
        store.atomically(|| -> Result<(), VaultError> {
            store.wipe()?;
            write_account(store, &acct)
        })?;
        if old_id != acct.vault_id {
            // Quick unlock belonged to the old vault; its wrap is gone.
            let _ = self.keystore.delete(old_id);
        }
        self.vault_id = Some(acct.vault_id);
        self.set_unlocked(unlocked)?;
        Ok(backup)
    }

    /// Reconnects this vault to the synced account it already belongs to
    /// (`m` has this vault's id and account key): after sync was turned off,
    /// the device was revoked, or its sign-in was lost. Checks `password`
    /// against `m` (the server's material is current: the password may have
    /// changed elsewhere), then in one transaction installs `m`'s KDF
    /// parameters and wraps, sets HLC node `node`, forgets every server
    /// revision (dirty flags kept), empties the sync bookkeeping and the
    /// inbound queue, and writes `sync_state` (the engine's configuration and
    /// a pending full resync). Every record stays: the resync pulls the
    /// server's copy, keeps local edits that are newer, and pushes what the
    /// server lacks. Leaves the vault unlocked. Runs Argon2 (blocking).
    pub fn rejoin(
        &mut self,
        m: &AccountMaterial,
        password: SecretString,
        node: u16,
        sync_state: &[(&str, &[u8])],
    ) -> Result<(), VaultError> {
        let local = self.load_account()?;
        let mut acct = account_from_material(m, node, &self.kdf_floor()?)?;
        if acct.vault_id != local.vault_id {
            return Err(VaultError::BadMaterial("belongs to another vault"));
        }
        if acct.public != local.public {
            return Err(VaultError::BadMaterial("account key differs"));
        }
        let (_, secret) = open_with_password(&acct, &password)?;
        let unlocked = open_account(&acct, &secret)
            .map_err(|_| VaultError::BadMaterial("keys do not match"))?;
        // The password proved the server's password wrap, not its recovery
        // wrap: a differing one waits beside the local one.
        let server_recovery = (acct.secret_by_recovery != local.secret_by_recovery)
            .then(|| std::mem::replace(&mut acct.secret_by_recovery, local.secret_by_recovery));
        let store = &self.store;
        store.atomically(|| -> Result<(), VaultError> {
            write_account(store, &acct)?;
            if let Some(wrap) = &server_recovery {
                store.account_put(keys::PENDING_SECRET_BY_RECOVERY, wrap)?;
            }
            store.sync_clear()?;
            store.reset_revs()?;
            for (k, v) in sync_state {
                store.sync_put(k, v)?;
            }
            Ok(())
        })?;
        self.set_unlocked(unlocked)
    }

    /// Commits turning sync on, after the server registered the account, in
    /// one transaction: the new recovery wrap (when "I lost it" made one),
    /// HLC node `node`, every server revision forgotten with the dirty flags
    /// kept, every live record queued for upload as new, the inbound queue
    /// emptied, and `sync_state` written (the configuration and the enabled
    /// flag). Either all of it lands or none. Works locked.
    pub fn commit_enable(
        &mut self,
        recovery: Option<&NewRecovery>,
        node: u16,
        sync_state: &[(&str, &[u8])],
    ) -> Result<(), VaultError> {
        if let Some(r) = recovery
            && self.vault_id != Some(r.vault_id)
        {
            return Err(VaultError::Invalid("recovery key for another vault"));
        }
        self.load_account()?;
        let store = &self.store;
        store.atomically(|| -> Result<(), VaultError> {
            if let Some(r) = recovery {
                store.account_put(keys::SECRET_BY_RECOVERY, &r.wrap)?;
            }
            drop_pending(store)?;
            store.account_put(keys::HLC_NODE, &node.to_le_bytes())?;
            store.sync_clear()?;
            store.reset_revs()?;
            store.mark_unsynced_dirty()?;
            for (k, v) in sync_state {
                store.sync_put(k, v)?;
            }
            Ok(())
        })?;
        if let Some(u) = self.unlocked.as_mut() {
            u.node = node;
            let last = u.last_hlc.get();
            u.last_hlc.set(Hlc { node, ..last });
        }
        Ok(())
    }

    /// The `RecoveryAuthKey` of `recovery`, after checking that it opens this
    /// vault's recovery wrap ([`VaultError::WrongRecoveryKey`]). Works locked.
    pub fn recovery_auth(&self, recovery: &str) -> Result<SecretBox<[u8; 32]>, VaultError> {
        let (_, key, _) = self.open_by_recovery(recovery)?;
        Ok(recovery_auth_key(key.expose_secret()))
    }

    /// A new recovery key and the account secret wrapped by it, not yet
    /// installed ([`install_recovery`](Self::install_recovery)). Needs the
    /// vault unlocked.
    pub fn new_recovery(&self) -> Result<NewRecovery, VaultError> {
        let u = self.unlocked.as_ref().ok_or(VaultError::Locked)?;
        let (key, text) = new_recovery_key();
        Ok(NewRecovery {
            wrap: wrap_secret(
                key.expose_secret(),
                u.account.secret.expose_secret(),
                &wrap_label("recovery", u.vault_id),
            ),
            auth: recovery_auth_key(key.expose_secret()),
            text: SecretString::from(text),
            vault_id: u.vault_id,
        })
    }

    /// Replaces `account_secret.recovery` with `r`'s wrap: the old recovery
    /// key stops opening this vault.
    pub fn install_recovery(&mut self, r: &NewRecovery) -> Result<(), VaultError> {
        if self.vault_id != Some(r.vault_id) {
            return Err(VaultError::Invalid("recovery key for another vault"));
        }
        let store = &self.store;
        store.atomically(|| -> Result<(), VaultError> {
            store.account_put(keys::SECRET_BY_RECOVERY, &r.wrap)?;
            Ok(store.account_delete(keys::PENDING_SECRET_BY_RECOVERY)?)
        })
    }

    /// Makes and installs a new recovery key locally; returns its text (show
    /// it once) and its `RecoveryAuthKey`. With sync on, the server learns it
    /// through `PUT /v1/account/recovery`. Needs the vault unlocked.
    pub fn rotate_recovery_key(&mut self) -> Result<(String, SecretBox<[u8; 32]>), VaultError> {
        let r = self.new_recovery()?;
        self.install_recovery(&r)?;
        Ok((r.text.expose_secret().to_owned(), r.auth))
    }

    /// Computes a recovery reset of this vault against the server's current
    /// material `m` (sync on): [`recovery_reset_for`] with fresh KDF
    /// parameters. Works locked. Blocking.
    pub fn prepare_recovery_reset(
        &self,
        m: &AccountMaterial,
        recovery: &str,
        new_password: &SecretString,
    ) -> Result<RecoveryReset, VaultError> {
        if self.vault_id.map(|id| id.to_string()).as_deref() != Some(m.vault_id.as_str()) {
            return Err(VaultError::BadMaterial("belongs to another vault"));
        }
        recovery_reset_for(m, recovery, new_password, (self.new_kdf)())
    }

    /// Writes a recovery reset the server accepted as `account_rev` and
    /// unlocks with it.
    pub fn commit_recovery_reset(
        &mut self,
        r: &RecoveryReset,
        account_rev: i64,
    ) -> Result<(), VaultError> {
        let acct = self.load_account()?;
        if acct.vault_id != r.vault_id {
            return Err(VaultError::BadMaterial("belongs to another vault"));
        }
        let kdf = validate_kdf(&r.kdf, &self.kdf_floor()?)?;
        let store = &self.store;
        store.atomically(|| -> Result<(), VaultError> {
            store.account_put(keys::KDF, &encode_kdf(&kdf))?;
            store.account_put(keys::SECRET_BY_KEK, &r.secret_by_kek)?;
            delete_rows(store, &PENDING_KEK)?;
            store.sync_put(SYNC_ACCOUNT_REV, account_rev.to_string().as_bytes())?;
            Ok(())
        })?;
        let acct = self.load_account()?;
        let unlocked = open_account(&acct, &r.secret)?;
        self.set_unlocked(unlocked)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_date_is_iso() {
        let d = utc_date();
        assert_eq!(d.len(), 10);
        assert!(d.starts_with("20"), "{d}");
        assert_eq!(&d[4..5], "-");
    }

    fn wire(m_kib: u32, t: u32, p: u32) -> KdfWire {
        KdfWire {
            m_kib,
            t,
            p,
            salt: vec![0; 16],
        }
    }

    fn cost(m_kib: u32, t: u32, p: u32) -> KdfParams {
        KdfParams {
            m_kib,
            t,
            p,
            salt: [0; 16],
        }
    }

    #[test]
    fn kdf_bounds() {
        let none = cost(1, 1, 1);
        validate_kdf(&wire(64, 1, 1), &none).unwrap();
        let m_below_8p = wire(15, 1, 2);
        assert!(matches!(
            validate_kdf(&m_below_8p, &none),
            Err(VaultError::BadMaterial(_))
        ));
        validate_kdf(&wire(MAX_M_KIB, MAX_T, MAX_P), &none).unwrap();
    }

    fn open(dir: &tempfile::TempDir, new_kdf: fn() -> KdfParams) -> VaultService {
        VaultService::open_inner(
            &dir.path().join("v.db"),
            Box::new(crate::vault::MemoryKeyStore::default()),
            new_kdf,
        )
        .unwrap()
    }

    #[test]
    fn production_floor_refuses_a_downgrade() {
        let dir = tempfile::tempdir().unwrap();
        // What `VaultService::open` uses: the floor is the v1 default cost.
        let v = open(&dir, KdfParams::new_random);
        let floor = v.kdf_floor().unwrap();
        let (m, t, p) = (
            KdfParams::DEFAULT_M_KIB,
            KdfParams::DEFAULT_T,
            KdfParams::DEFAULT_P,
        );
        assert_eq!((floor.m_kib, floor.t, floor.p), (m, t, p));
        validate_kdf(&wire(m, t, p), &floor).unwrap();
        validate_kdf(&wire(m * 2, t + 1, p), &floor).unwrap();
        // The review's attack (8 KiB, one pass, one lane), and each knob
        // one step under the default.
        for (m, t, p) in [(8, 1, 1), (m - 1, t, p), (m, t - 1, p), (m, t, p - 1)] {
            assert!(
                matches!(
                    validate_kdf(&wire(m, t, p), &floor),
                    Err(VaultError::WeakKdf)
                ),
                "{m} {t} {p}"
            );
        }
    }

    #[test]
    fn stored_parameters_raise_the_floor() {
        fn strong() -> KdfParams {
            KdfParams {
                m_kib: 256,
                t: 2,
                p: 2,
                ..KdfParams::new_random()
            }
        }
        fn cheap() -> KdfParams {
            KdfParams {
                m_kib: 64,
                t: 1,
                p: 1,
                ..KdfParams::new_random()
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let mut v = open(&dir, strong);
        v.create(SecretString::from("pw")).unwrap();
        // A build creating cheaper accounts still holds this one to its own
        // stored cost.
        v.new_kdf = cheap;
        let floor = v.kdf_floor().unwrap();
        assert_eq!((floor.m_kib, floor.t, floor.p), (256, 2, 2));
        validate_kdf(&wire(256, 2, 2), &floor).unwrap();
        for (m, t, p) in [(64, 1, 1), (255, 2, 2), (256, 1, 2), (256, 2, 1)] {
            assert!(
                matches!(
                    validate_kdf(&wire(m, t, p), &floor),
                    Err(VaultError::WeakKdf)
                ),
                "{m} {t} {p}"
            );
        }
        // Material lowering the stored cost is refused before anything is
        // written.
        let mut m = v.account_material().unwrap();
        m.kdf = wire(64, 1, 1);
        assert!(matches!(
            v.install_account_material(&m, 5),
            Err(VaultError::WeakKdf)
        ));
        assert!(v.is_unlocked());
        assert_eq!(v.account_rev().unwrap(), None);
    }
}
