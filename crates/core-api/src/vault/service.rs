//! Account lifecycle: create, unlock (password, recovery key), lock.
//!
//! The account lives in the store's key/value table under the keys in
//! [`keys`]; `docs/crypto.md` ("Local account record") documents the layout.

use std::cell::Cell;
use std::path::Path;

use core_crypto::{
    AccountKeys, KdfParams, derive_master, generate_account, new_recovery_key, open_vault_key,
    random_key, seal_vault_key, wrap_secret,
};
use core_model::{Hlc, Id};
use core_store::Store;
use secrecy::{ExposeSecret, SecretBox, SecretString};

use super::{
    DeviceKeyStore, GateKind, KeyringStore, NoGate, PresenceGate, VaultError, default_gate,
};

/// Account table keys (format v1).
pub(super) mod keys {
    /// 16 bytes: the vault id (UUIDv7, big-endian UUID bytes).
    pub const VAULT_ID: &str = "vault_id";
    /// 4 bytes LE: key version records are sealed under (1).
    pub const KEY_VERSION: &str = "key_version";
    /// 2 bytes LE: this device's HLC node id (random at create).
    pub const HLC_NODE: &str = "hlc_node";
    /// 28 bytes: Argon2id `m_kib`, `t`, `p` (u32 LE each) then the 16-byte salt.
    pub const KDF: &str = "kdf";
    /// 32 bytes: X25519 account public key.
    pub const ACCOUNT_PUBLIC: &str = "account_public";
    /// 73 bytes: account secret wrapped by the password KEK.
    pub const SECRET_BY_KEK: &str = "account_secret.kek";
    /// 73 bytes: account secret wrapped by the recovery key.
    pub const SECRET_BY_RECOVERY: &str = "account_secret.recovery";
    /// 73 bytes, optional: account secret wrapped by the keychain device key.
    pub const SECRET_BY_DEVICE: &str = "account_secret.device";
    /// Optional: the biometric enrollment state quick unlock was enabled
    /// (or first used) with; see `presence.rs`. Not secret.
    pub const BIOMETRY_STATE: &str = "quick_unlock.biometry";
    /// 32 bytes, optional: the key-binding challenge, which a key-binding
    /// gate (Windows Hello, the Android Keystore) signs or HMACs to derive the
    /// key wrapping `account_secret.device`; see `presence.rs`. Not secret.
    pub const HELLO_CHALLENGE: &str = "quick_unlock.hello_challenge";
    /// 81 bytes: the vault key HPKE-sealed to the account public key.
    pub const SEALED_VAULT_KEY: &str = "vault_key.sealed";
    /// 28 bytes, optional: the `kdf` of a password wrap from the sync server
    /// that has not opened on this device yet (`install_account_material`).
    pub const PENDING_KDF: &str = "pending.kdf";
    /// 73 bytes, optional: that unproven `account_secret.kek`.
    pub const PENDING_SECRET_BY_KEK: &str = "pending.account_secret.kek";
    /// 73 bytes, optional: an unproven `account_secret.recovery` from the
    /// sync server.
    pub const PENDING_SECRET_BY_RECOVERY: &str = "pending.account_secret.recovery";
}

/// Record key generation written by this build.
pub(super) const KEY_VERSION: u32 = 1;

/// Upper bounds applied to stored Argon2 parameters before they are used, so
/// a damaged or hostile account record cannot demand gigabytes of memory
/// (1 GiB is 16x the default 64 MiB; one flipped bit must not reach 4 GiB).
pub(super) const MAX_M_KIB: u32 = 1024 * 1024;
pub(super) const MAX_T: u32 = 16;
pub(super) const MAX_P: u32 = 64;

/// Wrap label for the account secret under one of its wrapping keys. Bound to
/// the vault id so a wrap cannot be swapped in from another vault.
pub(super) fn wrap_label(slot: &str, vault_id: Id) -> String {
    format!("account-secret/{slot}/{vault_id}")
}

/// The decoded account record.
pub(super) struct Account {
    pub vault_id: Id,
    pub key_version: u32,
    pub node: u16,
    pub kdf: KdfParams,
    pub public: [u8; 32],
    pub secret_by_kek: Vec<u8>,
    pub secret_by_recovery: Vec<u8>,
    pub sealed_vault_key: Vec<u8>,
    pub secret_by_device: Option<Vec<u8>>,
}

/// Key material held while the vault is unlocked. Dropping it zeroizes both
/// keys (`SecretBox`).
pub(super) struct Unlocked {
    pub vault_id: Id,
    pub key_version: u32,
    pub vault_key: SecretBox<[u8; 32]>,
    pub account: AccountKeys,
    pub node: u16,
    pub last_hlc: Cell<Hlc>,
}

/// The encrypted local vault.
///
/// Every read decrypts from the store; no decrypted cache is kept, so
/// [`lock`](Self::lock) only has keys to drop. Not `Sync` (the store is not):
/// share it as [`SharedVault`](super::SharedVault).
///
/// `create`, `unlock` and `unlock_with_recovery` run Argon2id (64 MiB, about
/// a second) synchronously on the calling thread. Async callers must invoke
/// them through `tokio::task::spawn_blocking`.
pub struct VaultService {
    pub(super) store: Store,
    pub(super) keystore: Box<dyn DeviceKeyStore>,
    /// Presence check in front of quick unlock (Touch ID on macOS, Face ID
    /// or Touch ID on iOS).
    pub(super) gate: Box<dyn PresenceGate>,
    /// Source of fresh KDF parameters for `create` and password resets.
    pub(super) new_kdf: fn() -> KdfParams,
    pub(super) vault_id: Option<Id>,
    pub(super) unlocked: Option<Unlocked>,
}

impl VaultService {
    /// Opens (creating if missing) the vault database at `path`, with quick
    /// unlock backed by the OS keychain ([`KeyringStore`]) behind this
    /// platform's presence check ([`default_gate`]: Touch ID on macOS, Face
    /// ID or Touch ID on iOS).
    ///
    /// A file that is not a SQLite database, or is damaged, returns `Err` and
    /// is left untouched.
    pub fn open(path: &Path) -> Result<Self, VaultError> {
        Ok(
            Self::open_with_keystore(path, Box::new(KeyringStore::new()))?
                .with_gate(default_gate()),
        )
    }

    /// Like [`open`](Self::open) with an explicit device-key store and no
    /// presence check (add one with [`with_gate`](Self::with_gate)).
    pub fn open_with_keystore(
        path: &Path,
        keystore: Box<dyn DeviceKeyStore>,
    ) -> Result<Self, VaultError> {
        Self::open_inner(path, keystore, KdfParams::new_random)
    }

    /// Test-only: like [`open_with_keystore`](Self::open_with_keystore) but
    /// vaults it creates use trivially cheap Argon2 parameters. Never use
    /// outside tests: such a vault offers no brute-force resistance. Only
    /// built with the `test-util` feature (dev-dependencies).
    #[cfg(feature = "test-util")]
    #[doc(hidden)]
    pub fn open_for_tests(
        path: &Path,
        keystore: Box<dyn DeviceKeyStore>,
    ) -> Result<Self, VaultError> {
        fn cheap() -> KdfParams {
            KdfParams {
                m_kib: 64,
                t: 1,
                p: 1,
                ..KdfParams::new_random()
            }
        }
        Self::open_inner(path, keystore, cheap)
    }

    pub(super) fn open_inner(
        path: &Path,
        keystore: Box<dyn DeviceKeyStore>,
        new_kdf: fn() -> KdfParams,
    ) -> Result<Self, VaultError> {
        let store = Store::open(path)?;
        let vault_id = store
            .account_get(keys::VAULT_ID)?
            .map(|b| parse_id(&b))
            .transpose()?;
        Ok(Self {
            store,
            keystore,
            gate: Box::new(NoGate),
            new_kdf,
            vault_id,
            unlocked: None,
        })
    }

    /// Replaces the presence check in front of quick unlock.
    pub fn with_gate(mut self, gate: Box<dyn PresenceGate>) -> Self {
        self.gate = gate;
        self
    }

    /// What quick unlock asks for on this device.
    pub fn quick_unlock_kind(&self) -> GateKind {
        self.gate.kind()
    }

    /// Whether a vault has been created in this database.
    pub fn is_initialized(&self) -> bool {
        self.vault_id.is_some()
    }

    /// Whether the vault is currently unlocked.
    pub fn is_unlocked(&self) -> bool {
        self.unlocked.is_some()
    }

    /// Creates the vault and leaves it unlocked. Returns the recovery key's
    /// text form, which is not stored anywhere: show it to the user once.
    ///
    /// Uses the production KDF parameters (Argon2id m = 64 MiB, t = 3, p = 4)
    /// with a fresh random salt; see the type docs about blocking.
    pub fn create(&mut self, password: SecretString) -> Result<String, VaultError> {
        if self.vault_id.is_some() {
            return Err(VaultError::AlreadyInitialized);
        }
        let vault_id = Id::now_v7();
        let kdf = (self.new_kdf)();
        let master = derive_master(&password, &kdf)?;
        let account = generate_account();
        let (recovery_key, recovery_text) = new_recovery_key();
        let vault_key = random_key();
        let node = {
            let r = random_key();
            u16::from_le_bytes([r.expose_secret()[0], r.expose_secret()[1]])
        };
        let sealed = seal_vault_key(
            &account.public,
            vault_key.expose_secret(),
            &vault_id.to_string(),
        )?;
        let by_kek = wrap_secret(
            master.kek.expose_secret(),
            account.secret.expose_secret(),
            &wrap_label("kek", vault_id),
        );
        let by_recovery = wrap_secret(
            recovery_key.expose_secret(),
            account.secret.expose_secret(),
            &wrap_label("recovery", vault_id),
        );

        let store = &self.store;
        store.atomically(|| -> Result<(), VaultError> {
            store.account_put(keys::VAULT_ID, vault_id.as_bytes())?;
            store.account_put(keys::KEY_VERSION, &KEY_VERSION.to_le_bytes())?;
            store.account_put(keys::HLC_NODE, &node.to_le_bytes())?;
            store.account_put(keys::KDF, &encode_kdf(&kdf))?;
            store.account_put(keys::ACCOUNT_PUBLIC, &account.public)?;
            store.account_put(keys::SECRET_BY_KEK, &by_kek)?;
            store.account_put(keys::SECRET_BY_RECOVERY, &by_recovery)?;
            store.account_put(keys::SEALED_VAULT_KEY, &sealed)?;
            Ok(())
        })?;

        self.vault_id = Some(vault_id);
        self.set_unlocked(Unlocked {
            vault_id,
            key_version: KEY_VERSION,
            vault_key,
            account,
            node,
            last_hlc: Cell::new(zero_hlc(node)),
        })?;
        Ok(recovery_text)
    }

    /// Unlocks with the master password. A wrong password returns
    /// [`VaultError::WrongPassword`] and writes nothing. Blocking; see the
    /// type docs.
    pub fn unlock(&mut self, password: SecretString) -> Result<(), VaultError> {
        let (acct, _, secret) = self.open_by_password(&password)?;
        self.set_unlocked(open_account(&acct, &secret)?)
    }

    /// Checks the master password of the unlocked vault without changing the
    /// session: the re-authentication before a sensitive action (exporting
    /// a private key). [`VaultError::Locked`] when locked,
    /// [`VaultError::WrongPassword`] on a mismatch. Blocking (the KDF).
    pub fn verify_password(&self, password: &SecretString) -> Result<(), VaultError> {
        if self.unlocked.is_none() {
            return Err(VaultError::Locked);
        }
        self.open_by_password(password).map(|_| ())
    }

    /// Unlocks with the recovery key and sets a new master password (fresh
    /// salt). The recovery key keeps working afterwards. Blocking; see the
    /// type docs.
    pub fn unlock_with_recovery(
        &mut self,
        recovery: &str,
        new_password: SecretString,
    ) -> Result<(), VaultError> {
        let (acct, _, secret) = self.open_by_recovery(recovery)?;
        // Prove the whole chain opens before replacing the password wrap.
        let unlocked = open_account(&acct, &secret)?;

        let kdf = (self.new_kdf)();
        let master = derive_master(&new_password, &kdf)?;
        let by_kek = wrap_secret(
            master.kek.expose_secret(),
            secret.expose_secret(),
            &wrap_label("kek", acct.vault_id),
        );
        let store = &self.store;
        store.atomically(|| -> Result<(), VaultError> {
            store.account_put(keys::KDF, &encode_kdf(&kdf))?;
            store.account_put(keys::SECRET_BY_KEK, &by_kek)?;
            // A server password wrap waiting to be proven is older than this.
            store.account_delete(keys::PENDING_KDF)?;
            store.account_delete(keys::PENDING_SECRET_BY_KEK)?;
            Ok(())
        })?;
        self.set_unlocked(unlocked)
    }

    /// Keeps `unlocked` as the session, with its clock resumed past every
    /// HLC this vault has seen (its records and the inbound queue), so an
    /// edit made after a relock still sorts after rows already observed
    /// from a device whose clock runs ahead.
    pub(super) fn set_unlocked(&mut self, unlocked: Unlocked) -> Result<(), VaultError> {
        if let Some(seen) = self.store.max_hlc(&unlocked.vault_id)? {
            let last = unlocked.last_hlc.get().max(seen);
            unlocked.last_hlc.set(last);
        }
        self.unlocked = Some(unlocked);
        Ok(())
    }

    /// Locks the vault: the vault key and account secret are zeroized
    /// (dropped `SecretBox`es). Locking a locked vault does nothing.
    pub fn lock(&mut self) {
        self.unlocked = None;
    }

    /// Reads and decodes the account record.
    pub(super) fn load_account(&self) -> Result<Account, VaultError> {
        let get = |k: &str| self.store.account_get(k);
        let need = |k: &'static str| -> Result<Vec<u8>, VaultError> {
            get(k)?.ok_or(VaultError::BadAccount(k))
        };
        let Some(id_bytes) = get(keys::VAULT_ID)? else {
            return Err(VaultError::NotInitialized);
        };
        let key_version = u32::from_le_bytes(fixed(&need(keys::KEY_VERSION)?, keys::KEY_VERSION)?);
        if key_version != KEY_VERSION {
            return Err(VaultError::BadAccount("unsupported key_version"));
        }
        Ok(Account {
            vault_id: parse_id(&id_bytes)?,
            key_version,
            node: u16::from_le_bytes(fixed(&need(keys::HLC_NODE)?, keys::HLC_NODE)?),
            kdf: decode_kdf(&need(keys::KDF)?)?,
            public: fixed(&need(keys::ACCOUNT_PUBLIC)?, keys::ACCOUNT_PUBLIC)?,
            secret_by_kek: need(keys::SECRET_BY_KEK)?,
            secret_by_recovery: need(keys::SECRET_BY_RECOVERY)?,
            sealed_vault_key: need(keys::SEALED_VAULT_KEY)?,
            secret_by_device: get(keys::SECRET_BY_DEVICE)?,
        })
    }
}

/// Rebuilds the account keys from an unwrapped secret and opens the vault key.
pub(super) fn open_account(
    acct: &Account,
    secret: &SecretBox<[u8; 32]>,
) -> Result<Unlocked, VaultError> {
    let account = AccountKeys::from_secret(*secret.expose_secret());
    if account.public != acct.public {
        return Err(VaultError::BadAccount("account public key mismatch"));
    }
    let vault_key = open_vault_key(&account, &acct.sealed_vault_key, &acct.vault_id.to_string())
        .map_err(|_| VaultError::BadAccount("sealed vault key does not open"))?;
    Ok(Unlocked {
        vault_id: acct.vault_id,
        key_version: acct.key_version,
        vault_key,
        account,
        node: acct.node,
        last_hlc: Cell::new(zero_hlc(acct.node)),
    })
}

pub(super) fn zero_hlc(node: u16) -> Hlc {
    Hlc {
        wall_ms: 0,
        counter: 0,
        node,
    }
}

pub(super) fn parse_id(bytes: &[u8]) -> Result<Id, VaultError> {
    Id::from_slice(bytes).map_err(|_| VaultError::BadAccount(keys::VAULT_ID))
}

fn fixed<const N: usize>(bytes: &[u8], what: &'static str) -> Result<[u8; N], VaultError> {
    bytes.try_into().map_err(|_| VaultError::BadAccount(what))
}

pub(super) fn encode_kdf(p: &KdfParams) -> Vec<u8> {
    let mut out = Vec::with_capacity(28);
    out.extend_from_slice(&p.m_kib.to_le_bytes());
    out.extend_from_slice(&p.t.to_le_bytes());
    out.extend_from_slice(&p.p.to_le_bytes());
    out.extend_from_slice(&p.salt);
    out
}

pub(super) fn decode_kdf(bytes: &[u8]) -> Result<KdfParams, VaultError> {
    let b: [u8; 28] = fixed(bytes, keys::KDF)?;
    let word = |i: usize| u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
    let p = KdfParams {
        m_kib: word(0),
        t: word(4),
        p: word(8),
        salt: fixed(&b[12..], keys::KDF)?,
    };
    if p.m_kib > MAX_M_KIB || p.t == 0 || p.t > MAX_T || p.p == 0 || p.p > MAX_P {
        return Err(VaultError::BadAccount("kdf parameters out of range"));
    }
    Ok(p)
}
