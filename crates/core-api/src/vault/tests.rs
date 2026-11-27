//! Vault service tests. They live under `vault::` so `cargo test -p core-api
//! vault` runs them. None of them touches the real OS keychain: every service
//! is built with [`MemoryKeyStore`] or [`FailingKeyStore`].

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use core_crypto::{KdfParams, new_recovery_key};
use core_model::{
    Conflict, ConnSettings, Extra, ForwardKind, ForwardRule, Group, Hlc, Host, Id, Identity,
    KnownHost, KnownHostSource, Source, SshKey, Tag, resolve,
};
use core_store::StoreError;
use secrecy::{ExposeSecret, SecretBox, SecretString};
use tempfile::TempDir;

use super::*;
use crate::SaveTarget;

const PASSWORD: &str = "correct horse battery staple";
const CANARY: &str = "CANARY-vault-5d1e";

fn pw(s: &str) -> SecretString {
    SecretString::from(s)
}

fn temp_path() -> (TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("vault.sqlite");
    (dir, path)
}

/// A service with cheap KDF parameters and an in-memory keystore.
fn cheap(path: &Path) -> VaultService {
    VaultService::open_for_tests(path, Box::new(MemoryKeyStore::default())).unwrap()
}

/// A fresh, created (and therefore unlocked) cheap vault.
fn created() -> (TempDir, PathBuf, VaultService, String) {
    let (dir, path) = temp_path();
    let mut v = cheap(&path);
    let recovery = v.create(pw(PASSWORD)).unwrap();
    (dir, path, v, recovery)
}

fn group(name: &str, parent_id: Option<Id>) -> Group {
    Group {
        id: Id::now_v7(),
        parent_id,
        name: name.into(),
        settings: ConnSettings::default(),
        extra: Default::default(),
    }
}

fn host(label: &str, group_id: Option<Id>) -> Host {
    Host {
        id: Id::now_v7(),
        group_id,
        label: label.into(),
        address: "10.0.0.1".into(),
        tag_ids: vec![],
        settings: ConnSettings::default(),
        forward_rule_ids: vec![],
        extra: Default::default(),
    }
}

fn identity(key_id: Option<Id>) -> Identity {
    Identity {
        id: Id::now_v7(),
        label: "prod".into(),
        username: "root".into(),
        password: Some(SecretString::from("hunter2")),
        key_id,
        owner: None,
        extra: Default::default(),
    }
}

fn ssh_key() -> SshKey {
    SshKey {
        id: Id::now_v7(),
        label: "laptop".into(),
        algorithm: "ssh-ed25519".into(),
        private_openssh: SecretString::from("-----BEGIN OPENSSH PRIVATE KEY-----"),
        passphrase: None,
        public_openssh: "ssh-ed25519 AAAA".into(),
        certificate: None,
        extra: Default::default(),
    }
}

fn rule(host_id: Id) -> ForwardRule {
    ForwardRule {
        id: Id::now_v7(),
        host_id,
        label: "db".into(),
        kind: ForwardKind::Local,
        bind_addr: "127.0.0.1".into(),
        bind_port: 15432,
        dest_host: Some("localhost".into()),
        dest_port: Some(5432),
        autostart: false,
        extra: Default::default(),
    }
}

fn sorted(mut ids: Vec<Id>) -> Vec<Id> {
    ids.sort();
    ids
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// Bytes of the database file and its WAL (empty when absent).
fn snapshot(path: &Path) -> (Vec<u8>, Vec<u8>) {
    let wal = path.with_extension("sqlite-wal");
    (
        std::fs::read(path).unwrap(),
        std::fs::read(wal).unwrap_or_default(),
    )
}

/// An in-memory keystore whose `delete` can be made to fail.
#[derive(Clone, Default)]
struct DeleteFailsKeyStore {
    keys: MemoryKeyStore,
    fail_delete: Arc<std::sync::atomic::AtomicBool>,
}

impl DeleteFailsKeyStore {
    fn fail_delete(&self, fail: bool) {
        self.fail_delete
            .store(fail, std::sync::atomic::Ordering::SeqCst);
    }
}

impl DeviceKeyStore for DeleteFailsKeyStore {
    fn get(&self, id: Id) -> Result<Option<SecretBox<[u8; 32]>>, String> {
        self.keys.get(id)
    }
    fn set(&self, id: Id, key: &[u8; 32]) -> Result<(), String> {
        self.keys.set(id, key)
    }
    fn delete(&self, id: Id) -> Result<(), String> {
        if self.fail_delete.load(std::sync::atomic::Ordering::SeqCst) {
            return Err("keychain unavailable".into());
        }
        self.keys.delete(id)
    }
}

/// A keystore whose every call fails, like a machine with no usable keychain.
struct FailingKeyStore;

impl DeviceKeyStore for FailingKeyStore {
    fn get(&self, _: Id) -> Result<Option<SecretBox<[u8; 32]>>, String> {
        Err("keychain unavailable".into())
    }
    fn set(&self, _: Id, _: &[u8; 32]) -> Result<(), String> {
        Err("keychain unavailable".into())
    }
    fn delete(&self, _: Id) -> Result<(), String> {
        Err("keychain unavailable".into())
    }
}

#[test]
fn create_unlock_crud_lock() {
    // Production KDF parameters on purpose (R20): the real create path.
    let (_dir, path) = temp_path();
    let mut v =
        VaultService::open_with_keystore(&path, Box::new(MemoryKeyStore::default())).unwrap();
    assert!(!v.is_initialized());
    assert!(matches!(v.list::<Host>(), Err(VaultError::Locked)));

    let recovery = v.create(pw(PASSWORD)).unwrap();
    assert!(v.is_initialized());
    assert!(v.is_unlocked());
    assert_eq!(recovery.len(), 64);
    assert!(core_crypto::parse_recovery_key(&recovery).is_ok());
    assert!(matches!(
        v.create(pw("again")),
        Err(VaultError::AlreadyInitialized)
    ));
    let account = v.load_account().unwrap();
    assert_eq!(account.kdf.m_kib, KdfParams::DEFAULT_M_KIB);
    assert_eq!(account.kdf.t, KdfParams::DEFAULT_T);
    assert_eq!(account.kdf.p, KdfParams::DEFAULT_P);
    assert_ne!(account.kdf.salt, [0u8; 16]);
    assert_eq!(account.key_version, 1);

    // Create.
    let tag = Tag {
        id: Id::now_v7(),
        name: "prod".into(),
        color: "#ff0000".into(),
        extra: Default::default(),
    };
    let key = ssh_key();
    let ident = identity(Some(key.id));
    let g = group("servers 🚀", None);
    let mut h = host(&format!("db {CANARY}"), Some(g.id));
    h.tag_ids = vec![tag.id];
    h.settings.identity_id = Some(ident.id);
    let r = rule(h.id);
    let kh = KnownHost {
        id: Id::now_v7(),
        host_pattern: "10.0.0.1".into(),
        port: 22,
        algorithm: "ssh-ed25519".into(),
        public_key: "AAAA".into(),
        added_at: 1,
        source: KnownHostSource::Prompt,
        extra: Default::default(),
    };
    v.upsert(tag.clone()).unwrap();
    v.upsert(key.clone()).unwrap();
    v.upsert(ident.clone()).unwrap();
    v.upsert(g.clone()).unwrap();
    v.upsert(h.clone()).unwrap();
    v.upsert(r.clone()).unwrap();
    v.upsert(kh.clone()).unwrap();

    // Read.
    assert_eq!(v.list::<Group>().unwrap(), vec![g.clone()]);
    assert_eq!(v.list::<Tag>().unwrap(), vec![tag.clone()]);
    assert_eq!(v.list::<KnownHost>().unwrap(), vec![kh.clone()]);
    assert_eq!(v.list::<ForwardRule>().unwrap(), vec![r.clone()]);
    let got = v.get::<Host>(h.id).unwrap().unwrap();
    assert_eq!(got.label, h.label);
    assert_eq!(got.forward_rule_ids, vec![r.id], "rule synced onto host");
    let got_ident = v.get::<Identity>(ident.id).unwrap().unwrap();
    assert_eq!(
        got_ident.password.unwrap().expose_secret(),
        ident.password.as_ref().unwrap().expose_secret()
    );
    assert_eq!(
        v.get::<SshKey>(key.id)
            .unwrap()
            .unwrap()
            .private_openssh
            .expose_secret(),
        key.private_openssh.expose_secret()
    );
    assert!(v.get::<Host>(Id::now_v7()).unwrap().is_none());
    // An id of another kind is not a Host.
    assert!(v.get::<Host>(g.id).unwrap().is_none());

    // Update.
    let mut h2 = got.clone();
    h2.label = "renamed".into();
    v.upsert(h2.clone()).unwrap();
    assert_eq!(v.get::<Host>(h.id).unwrap().unwrap().label, "renamed");
    assert_eq!(v.list::<Host>().unwrap().len(), 1);

    // Delete.
    v.delete::<KnownHost>(kh.id).unwrap();
    assert!(v.list::<KnownHost>().unwrap().is_empty());
    assert!(matches!(
        v.delete::<KnownHost>(kh.id),
        Err(VaultError::Store(StoreError::NotFound { .. }))
    ));

    // Lock: keys and access gone.
    v.lock();
    assert!(!v.is_unlocked());
    assert!(matches!(v.list::<Host>(), Err(VaultError::Locked)));

    // Reopen from the file and unlock with the password.
    drop(v);
    let (db, wal) = snapshot(&path);
    assert!(!contains(&db, CANARY.as_bytes()), "plaintext in db file");
    assert!(!contains(&wal, CANARY.as_bytes()), "plaintext in wal");
    assert!(!contains(&db, PASSWORD.as_bytes()));
    let mut v =
        VaultService::open_with_keystore(&path, Box::new(MemoryKeyStore::default())).unwrap();
    assert!(v.is_initialized());
    assert!(!v.is_unlocked());
    v.unlock(pw(PASSWORD)).unwrap();
    assert_eq!(v.get::<Host>(h.id).unwrap().unwrap(), h2);
    assert_eq!(v.list::<Identity>().unwrap().len(), 1);
}

#[test]
fn wrong_password_rejected_without_mutation() {
    let (_dir, path, mut v, _) = created();
    v.upsert(host("a", None)).unwrap();
    drop(v);
    let before = snapshot(&path);

    let mut v = cheap(&path);
    assert!(matches!(
        v.unlock(pw("not the password")),
        Err(VaultError::WrongPassword)
    ));
    assert!(!v.is_unlocked());
    assert!(matches!(v.list::<Host>(), Err(VaultError::Locked)));
    assert_eq!(snapshot(&path), before, "failed unlock wrote to the vault");
    drop(v);
    assert_eq!(snapshot(&path), before, "file changed after failed unlock");

    // The right password still works afterwards.
    let mut v = cheap(&path);
    v.unlock(pw(PASSWORD)).unwrap();
    assert_eq!(v.list::<Host>().unwrap().len(), 1);
}

/// A stored KDF record above the caps (1 GiB, t = 16) is a clear error
/// before any Argon2 work, so a flipped bit cannot ask for gigabytes.
#[test]
fn over_cap_kdf_params_rejected_before_derivation() {
    use super::service::{MAX_M_KIB, MAX_T, keys};

    let (_dir, path, mut v, _) = created();
    v.lock();
    let good = v.store.account_get(keys::KDF).unwrap().unwrap();
    let with = |m: u32, t: u32| {
        let mut b = good.clone();
        b[0..4].copy_from_slice(&m.to_le_bytes());
        b[4..8].copy_from_slice(&t.to_le_bytes());
        b
    };
    // 2 GiB (within the old 4 GiB cap), one KiB over the cap, and t over it.
    for bad in [
        with(2 * 1024 * 1024, 1),
        with(MAX_M_KIB + 1, 1),
        with(64, MAX_T + 1),
    ] {
        v.store.account_put(keys::KDF, &bad).unwrap();
        let before = snapshot(&path);
        let started = std::time::Instant::now();
        assert!(matches!(
            v.unlock(pw(PASSWORD)),
            Err(VaultError::BadAccount("kdf parameters out of range"))
        ));
        assert!(
            started.elapsed() < std::time::Duration::from_millis(500),
            "rejected only after deriving"
        );
        assert!(!v.is_unlocked());
        assert_eq!(snapshot(&path), before, "failed unlock wrote to the vault");
    }

    // The real record still unlocks.
    v.store.account_put(keys::KDF, &good).unwrap();
    v.unlock(pw(PASSWORD)).unwrap();
}

#[test]
fn recovery_unlock_resets_password() {
    let (_dir, _path, mut v, recovery) = created();
    let h = host("kept", None);
    v.upsert(h.clone()).unwrap();
    let old_salt = v.load_account().unwrap().kdf.salt;
    v.lock();

    assert!(matches!(
        v.unlock_with_recovery("not-a-key", pw("x")),
        Err(VaultError::WrongRecoveryKey)
    ));
    let (_, other) = new_recovery_key();
    assert!(matches!(
        v.unlock_with_recovery(&other, pw("x")),
        Err(VaultError::WrongRecoveryKey)
    ));
    assert!(!v.is_unlocked());

    // Lower case and stray whitespace are accepted.
    let typed = format!("  {}\n", recovery.to_lowercase());
    v.unlock_with_recovery(&typed, pw("new password")).unwrap();
    assert!(v.is_unlocked());
    assert_eq!(v.get::<Host>(h.id).unwrap().unwrap(), h);
    assert_ne!(v.load_account().unwrap().kdf.salt, old_salt, "new salt");

    v.lock();
    assert!(matches!(
        v.unlock(pw(PASSWORD)),
        Err(VaultError::WrongPassword)
    ));
    v.unlock(pw("new password")).unwrap();
    v.lock();
    // The recovery wrap is kept: the same recovery key works again.
    v.unlock_with_recovery(&recovery, pw("third")).unwrap();
    v.lock();
    v.unlock(pw("third")).unwrap();
}

#[test]
fn locked_service_refuses_reads() {
    // Not initialized yet.
    let (_dir, path) = temp_path();
    let mut v = cheap(&path);
    assert!(matches!(
        v.unlock(pw(PASSWORD)),
        Err(VaultError::NotInitialized)
    ));
    assert!(matches!(v.quick_unlock(), Err(VaultError::NotInitialized)));
    assert!(matches!(v.list::<Host>(), Err(VaultError::Locked)));

    v.create(pw(PASSWORD)).unwrap();
    let h = host("h", None);
    v.upsert(h.clone()).unwrap();
    v.lock();
    assert!(matches!(v.list::<Host>(), Err(VaultError::Locked)));
    assert!(matches!(v.get::<Host>(h.id), Err(VaultError::Locked)));
    assert!(matches!(v.upsert(host("x", None)), Err(VaultError::Locked)));
    assert!(matches!(v.delete::<Host>(h.id), Err(VaultError::Locked)));
    assert!(matches!(
        v.delete_group(Id::now_v7(), ChildPolicy::MoveToParent),
        Err(VaultError::Locked)
    ));
    assert!(matches!(
        v.effective_settings(h.id),
        Err(VaultError::Locked)
    ));
    assert!(matches!(v.enable_quick_unlock(), Err(VaultError::Locked)));
    // Locking twice is harmless.
    v.lock();
}

#[test]
fn delete_group_requires_child_policy() {
    let (_dir, _path, mut v, _) = created();
    let root = group("root", None);
    let g = group("g", Some(root.id));
    let sub = group("sub", Some(g.id));
    let h1 = host("h1", Some(g.id));
    let h2 = host("h2", Some(sub.id));
    for x in [&root, &g, &sub] {
        v.upsert(x.clone()).unwrap();
    }
    v.upsert(h1.clone()).unwrap();
    v.upsert(h2.clone()).unwrap();
    let r2 = rule(h2.id);
    v.upsert(r2.clone()).unwrap();

    // Without a policy: blocked, listing the children.
    match v.delete::<Group>(g.id) {
        Err(VaultError::Referenced(ids)) => assert_eq!(sorted(ids), sorted(vec![sub.id, h1.id])),
        other => panic!("expected Referenced, got {other:?}"),
    }
    assert!(v.get::<Group>(g.id).unwrap().is_some());

    // MoveToParent: the subgroup and the host move up to `root`.
    v.delete_group(g.id, ChildPolicy::MoveToParent).unwrap();
    assert!(v.get::<Group>(g.id).unwrap().is_none());
    assert_eq!(
        v.get::<Group>(sub.id).unwrap().unwrap().parent_id,
        Some(root.id)
    );
    assert_eq!(
        v.get::<Host>(h1.id).unwrap().unwrap().group_id,
        Some(root.id)
    );
    assert_eq!(
        v.get::<Host>(h2.id).unwrap().unwrap().group_id,
        Some(sub.id)
    );

    // MoveToParent on a top-level group moves children to the root level.
    v.delete_group(root.id, ChildPolicy::MoveToParent).unwrap();
    assert_eq!(v.get::<Group>(sub.id).unwrap().unwrap().parent_id, None);
    assert_eq!(v.get::<Host>(h1.id).unwrap().unwrap().group_id, None);

    // DeleteRecursive: the subtree, its hosts and their rules all go.
    let top = group("top", None);
    let mid = group("mid", Some(top.id));
    let h3 = host("h3", Some(mid.id));
    v.upsert(top.clone()).unwrap();
    v.upsert(mid.clone()).unwrap();
    v.upsert(h3.clone()).unwrap();
    let r3 = rule(h3.id);
    v.upsert(r3.clone()).unwrap();
    v.delete_group(top.id, ChildPolicy::DeleteRecursive)
        .unwrap();
    assert!(v.get::<Group>(top.id).unwrap().is_none());
    assert!(v.get::<Group>(mid.id).unwrap().is_none());
    assert!(v.get::<Host>(h3.id).unwrap().is_none());
    assert!(v.get::<ForwardRule>(r3.id).unwrap().is_none());
    // Unrelated data untouched.
    assert!(v.get::<Host>(h2.id).unwrap().is_some());
    assert!(v.get::<ForwardRule>(r2.id).unwrap().is_some());

    // A group without children deletes directly; a missing one is NotFound.
    v.delete::<Group>(sub.id).unwrap_err(); // still holds h2
    v.delete::<Host>(h2.id).unwrap();
    v.delete::<Group>(sub.id).unwrap();
    assert!(matches!(
        v.delete_group(sub.id, ChildPolicy::MoveToParent),
        Err(VaultError::Store(StoreError::NotFound { .. }))
    ));
}

#[test]
fn delete_referenced_identity_blocked() {
    let (_dir, _path, mut v, _) = created();
    let key = ssh_key();
    let ident = identity(Some(key.id));
    v.upsert(key.clone()).unwrap();
    v.upsert(ident.clone()).unwrap();
    let mut h1 = host("h1", None);
    let mut h2 = host("h2", None);
    let mut g = group("g", None);
    h1.settings.identity_id = Some(ident.id);
    h2.settings.identity_id = Some(ident.id);
    g.settings.identity_id = Some(ident.id);
    v.upsert(h1.clone()).unwrap();
    v.upsert(h2.clone()).unwrap();
    v.upsert(g.clone()).unwrap();
    let unrelated = host("h3", None);
    v.upsert(unrelated).unwrap();

    match v.delete::<Identity>(ident.id) {
        Err(VaultError::Referenced(ids)) => {
            assert_eq!(sorted(ids), sorted(vec![h1.id, h2.id, g.id]))
        }
        other => panic!("expected Referenced, got {other:?}"),
    }
    match v.delete::<SshKey>(key.id) {
        Err(VaultError::Referenced(ids)) => assert_eq!(ids, vec![ident.id]),
        other => panic!("expected Referenced, got {other:?}"),
    }
    assert!(v.get::<Identity>(ident.id).unwrap().is_some());
    assert!(v.get::<SshKey>(key.id).unwrap().is_some());

    // Once nothing refers to them they can go.
    for mut x in [h1, h2] {
        x.settings.identity_id = None;
        v.upsert(x).unwrap();
    }
    g.settings.identity_id = None;
    v.upsert(g).unwrap();
    v.delete::<Identity>(ident.id).unwrap();
    v.delete::<SshKey>(key.id).unwrap();
}

#[test]
fn references_stay_consistent() {
    let (_dir, _path, mut v, _) = created();

    // Dangling references are rejected, not stored.
    let missing = Id::now_v7();
    let mut h = host("h", Some(missing));
    assert!(matches!(v.upsert(h.clone()), Err(VaultError::MissingRef(id)) if id == missing));
    h.group_id = None;
    h.settings.identity_id = Some(missing);
    assert!(matches!(
        v.upsert(h.clone()),
        Err(VaultError::MissingRef(_))
    ));
    h.settings.identity_id = None;
    h.tag_ids = vec![missing];
    assert!(matches!(
        v.upsert(h.clone()),
        Err(VaultError::MissingRef(_))
    ));
    h.tag_ids = vec![];
    assert!(matches!(
        v.upsert(identity(Some(missing))),
        Err(VaultError::MissingRef(_))
    ));
    assert!(matches!(
        v.upsert(group("g", Some(missing))),
        Err(VaultError::MissingRef(_))
    ));
    assert!(matches!(
        v.upsert(rule(missing)),
        Err(VaultError::MissingRef(_))
    ));
    assert!(v.list::<Host>().unwrap().is_empty());

    // An id already used by another kind is refused.
    let t = Tag {
        id: Id::now_v7(),
        name: "t".into(),
        color: "#000".into(),
        extra: Default::default(),
    };
    v.upsert(t.clone()).unwrap();
    let mut clash = host("clash", None);
    clash.id = t.id;
    assert!(matches!(v.upsert(clash), Err(VaultError::IdConflict(id)) if id == t.id));

    // Tags: deleting strips them from hosts.
    h.tag_ids = vec![t.id];
    v.upsert(h.clone()).unwrap();
    v.delete::<Tag>(t.id).unwrap();
    assert!(v.get::<Host>(h.id).unwrap().unwrap().tag_ids.is_empty());

    // Forward rules: host_id is authoritative, the host's list follows it.
    let h2 = host("h2", None);
    v.upsert(h2.clone()).unwrap();
    let mut r = rule(h.id);
    v.upsert(r.clone()).unwrap();
    assert_eq!(
        v.get::<Host>(h.id).unwrap().unwrap().forward_rule_ids,
        vec![r.id]
    );
    // A host upsert with a stale list is corrected from the rules.
    let mut stale = v.get::<Host>(h.id).unwrap().unwrap();
    stale.forward_rule_ids = vec![missing];
    v.upsert(stale).unwrap();
    assert_eq!(
        v.get::<Host>(h.id).unwrap().unwrap().forward_rule_ids,
        vec![r.id]
    );
    // Moving the rule to another host updates both lists.
    r.host_id = h2.id;
    v.upsert(r.clone()).unwrap();
    assert!(
        v.get::<Host>(h.id)
            .unwrap()
            .unwrap()
            .forward_rule_ids
            .is_empty()
    );
    assert_eq!(
        v.get::<Host>(h2.id).unwrap().unwrap().forward_rule_ids,
        vec![r.id]
    );
    v.delete::<ForwardRule>(r.id).unwrap();
    assert!(
        v.get::<Host>(h2.id)
            .unwrap()
            .unwrap()
            .forward_rule_ids
            .is_empty()
    );

    // Deleting a host cascades to its rules.
    let r2 = rule(h2.id);
    v.upsert(r2.clone()).unwrap();
    v.delete::<Host>(h2.id).unwrap();
    assert!(v.get::<ForwardRule>(r2.id).unwrap().is_none());
    assert!(v.list::<ForwardRule>().unwrap().is_empty());
}

#[test]
fn group_move_creating_cycle_rejected() {
    let (_dir, _path, mut v, _) = created();
    let a = group("a", None);
    let b = group("b", Some(a.id));
    let c = group("c", Some(b.id));
    for x in [&a, &b, &c] {
        v.upsert(x.clone()).unwrap();
    }

    let mut a2 = a.clone();
    a2.parent_id = Some(c.id);
    assert!(matches!(v.upsert(a2), Err(VaultError::Cycle)));
    let mut self_parent = b.clone();
    self_parent.parent_id = Some(b.id);
    assert!(matches!(v.upsert(self_parent), Err(VaultError::Cycle)));
    // Nothing was written.
    assert_eq!(v.get::<Group>(a.id).unwrap().unwrap(), a);
    assert_eq!(v.get::<Group>(b.id).unwrap().unwrap(), b);

    // A legal move is fine.
    let mut c2 = c.clone();
    c2.parent_id = Some(a.id);
    v.upsert(c2.clone()).unwrap();
    assert_eq!(v.get::<Group>(c.id).unwrap().unwrap(), c2);
}

/// `upsert_all` (the known_hosts import's write) is all or nothing: an entry
/// failing part-way leaves none of the batch stored.
#[test]
fn upsert_all_failure_part_way_writes_nothing() {
    let (_dir, path, mut v, _) = created();
    let known = |n: u8| KnownHost {
        id: Id::now_v7(),
        host_pattern: format!("10.0.0.{n}"),
        port: 22,
        algorithm: "ssh-ed25519".into(),
        public_key: format!("AAAA{n}"),
        added_at: 1,
        source: KnownHostSource::Import,
        extra: Default::default(),
    };
    let taken = host("taken", None);
    v.upsert(taken.clone()).unwrap();
    let before = snapshot(&path);

    // The third of four entries reuses a host's id.
    let mut clash = known(3);
    clash.id = taken.id;
    let batch = vec![known(1), known(2), clash, known(4)];
    assert!(matches!(
        v.upsert_all(batch),
        Err(VaultError::IdConflict(id)) if id == taken.id
    ));
    assert!(v.list::<KnownHost>().unwrap().is_empty());
    assert_eq!(snapshot(&path), before, "a failed batch wrote to the vault");

    // A good batch lands whole.
    v.upsert_all(vec![known(1), known(2)]).unwrap();
    assert_eq!(v.list::<KnownHost>().unwrap().len(), 2);
}

#[test]
fn upsert_refuses_to_resurrect_a_deleted_record() {
    let (_dir, _path, mut v, _) = created();
    let h = host("gone", None);
    v.upsert(h.clone()).unwrap();
    let r = rule(h.id);
    v.upsert(r.clone()).unwrap();
    v.delete::<ForwardRule>(r.id).unwrap();
    v.delete::<Host>(h.id).unwrap();

    // A stale editor saving the deleted records gets NotFound, and they stay
    // deleted (tombstones in the store).
    assert!(matches!(
        v.upsert(h.clone()),
        Err(VaultError::Store(StoreError::NotFound { id })) if id == h.id
    ));
    let live = host("live", None);
    v.upsert(live.clone()).unwrap();
    let mut moved = r.clone();
    moved.host_id = live.id;
    assert!(matches!(
        v.upsert(moved),
        Err(VaultError::Store(StoreError::NotFound { id })) if id == r.id
    ));
    assert!(v.get::<Host>(h.id).unwrap().is_none());
    assert!(v.get::<ForwardRule>(r.id).unwrap().is_none());
    assert!(v.store.get(&h.id).unwrap().unwrap().deleted);
    assert!(v.store.get(&r.id).unwrap().unwrap().deleted);

    // A tombstone of another kind is still an id conflict.
    let mut clash = group("clash", None);
    clash.id = h.id;
    assert!(matches!(v.upsert(clash), Err(VaultError::IdConflict(id)) if id == h.id));
}

#[test]
fn effective_settings_matches_core_model() {
    let (_dir, _path, mut v, _) = created();
    let ident = identity(None);
    v.upsert(ident.clone()).unwrap();
    let mut parent = group("parent", None);
    parent.settings.port = Some(2200);
    parent.settings.identity_id = Some(ident.id);
    let mut child = group("child", Some(parent.id));
    child.settings.font_size = Some(14);
    child.settings.keepalive_secs = Some(60);
    let mut h = host("h", Some(child.id));
    h.settings.font_size = Some(16);
    v.upsert(parent.clone()).unwrap();
    v.upsert(child.clone()).unwrap();
    v.upsert(h.clone()).unwrap();

    let eff = v.effective_settings(h.id).unwrap();
    let groups: HashMap<Id, Group> = [(parent.id, parent.clone()), (child.id, child.clone())]
        .into_iter()
        .collect();
    let stored = v.get::<Host>(h.id).unwrap().unwrap();
    assert_eq!(eff, resolve(&app_defaults(), &groups, &stored).unwrap());

    assert_eq!(eff.port.value, 2200);
    assert_eq!(eff.port.source, core_model::Source::Group(parent.id));
    assert_eq!(eff.identity_id.value, Some(ident.id));
    assert_eq!(eff.keepalive_secs.value, Some(60));
    assert_eq!(eff.font_size.value, Some(16));
    assert_eq!(eff.font_size.source, core_model::Source::Host);
    assert_eq!(eff.term_type.value.as_deref(), Some("xterm-256color"));
    assert_eq!(eff.term_type.source, core_model::Source::Default);
    assert_eq!(eff.charset.value.as_deref(), Some("UTF-8"));

    // App defaults per R19.
    let d = app_defaults();
    assert_eq!(d.port, Some(22));
    assert_eq!(d.keepalive_secs, Some(30));
    assert_eq!(
        (
            d.identity_id,
            d.color_scheme,
            d.font_family,
            d.font_size,
            d.env
        ),
        (None, None, None, None, None)
    );

    assert!(matches!(
        v.effective_settings(Id::now_v7()),
        Err(VaultError::Store(StoreError::NotFound { .. }))
    ));
}

#[test]
fn quick_unlock_when_keychain_missing_falls_back() {
    // No usable keychain at all.
    let (_dir, path) = temp_path();
    let mut v = VaultService::open_for_tests(&path, Box::new(FailingKeyStore)).unwrap();
    v.create(pw(PASSWORD)).unwrap();
    v.upsert(host("h", None)).unwrap();
    assert!(matches!(
        v.enable_quick_unlock(),
        Err(VaultError::Keychain(_))
    ));
    assert!(!v.is_quick_unlock_enabled());
    v.lock();
    assert!(matches!(
        v.quick_unlock(),
        Err(VaultError::QuickUnlockNotEnabled)
    ));
    v.unlock(pw(PASSWORD)).unwrap();
    assert_eq!(v.list::<Host>().unwrap().len(), 1);
    drop(v);

    // Enabled earlier, but the keychain now fails or has lost the entry.
    let (_dir2, path2) = temp_path();
    let keys = MemoryKeyStore::default();
    let mut v = VaultService::open_for_tests(&path2, Box::new(keys.clone())).unwrap();
    v.create(pw(PASSWORD)).unwrap();
    v.upsert(host("h", None)).unwrap();
    v.enable_quick_unlock().unwrap();
    assert!(v.is_quick_unlock_enabled());
    v.lock();
    v.quick_unlock().unwrap();
    assert_eq!(v.list::<Host>().unwrap().len(), 1);
    v.lock();
    drop(v);

    let mut v = VaultService::open_for_tests(&path2, Box::new(FailingKeyStore)).unwrap();
    assert!(matches!(v.quick_unlock(), Err(VaultError::Keychain(_))));
    assert!(!v.is_unlocked());
    v.unlock(pw(PASSWORD)).unwrap();
    drop(v);

    keys.clear();
    let mut v = VaultService::open_for_tests(&path2, Box::new(keys.clone())).unwrap();
    assert!(matches!(v.quick_unlock(), Err(VaultError::Keychain(_))));
    v.unlock(pw(PASSWORD)).unwrap();
    assert_eq!(v.list::<Host>().unwrap().len(), 1);
}

#[test]
fn quick_unlock_disable_removes_device_key() {
    let (_dir, path) = temp_path();
    let keys = MemoryKeyStore::default();
    let mut v = VaultService::open_for_tests(&path, Box::new(keys.clone())).unwrap();
    v.create(pw(PASSWORD)).unwrap();
    v.enable_quick_unlock().unwrap();
    assert_eq!(keys.len(), 1);
    v.disable_quick_unlock().unwrap();
    assert_eq!(keys.len(), 0);
    assert!(!v.is_quick_unlock_enabled());
    v.lock();
    assert!(matches!(
        v.quick_unlock(),
        Err(VaultError::QuickUnlockNotEnabled)
    ));
}

#[test]
fn open_garbage_or_damaged_vault_errs_without_modifying_it() {
    let dir = tempfile::tempdir().unwrap();
    let garbage = dir.path().join("garbage.sqlite");
    let bytes = b"definitely not a vault".repeat(300);
    std::fs::write(&garbage, &bytes).unwrap();
    assert!(VaultService::open(&garbage).is_err());
    assert_eq!(std::fs::read(&garbage).unwrap(), bytes);

    // A real vault, then truncated and (separately) with damaged pages.
    let path = dir.path().join("vault.sqlite");
    let mut v = cheap(&path);
    v.create(pw(PASSWORD)).unwrap();
    for i in 0..200 {
        v.upsert(host(&format!("h{i}"), None)).unwrap();
    }
    drop(v);
    let full = std::fs::read(&path).unwrap();
    let page = 4096;
    assert!(full.len() > 16 * page);

    let cut = dir.path().join("cut.sqlite");
    let truncated = full[..full.len() / 2].to_vec();
    std::fs::write(&cut, &truncated).unwrap();
    assert!(VaultService::open_for_tests(&cut, Box::new(MemoryKeyStore::default())).is_err());
    assert_eq!(std::fs::read(&cut).unwrap(), truncated);

    let bad = dir.path().join("damaged.sqlite");
    let mut damaged = full.clone();
    damaged[8 * page..].fill(0xAB);
    std::fs::write(&bad, &damaged).unwrap();
    assert!(VaultService::open_for_tests(&bad, Box::new(MemoryKeyStore::default())).is_err());
    assert_eq!(std::fs::read(&bad).unwrap(), damaged);
}

#[test]
fn corrupt_record_is_reported_by_id() {
    let (_dir, _path, mut v, _) = created();
    let good = host("good", None);
    let bad = host("bad", None);
    v.upsert(good.clone()).unwrap();
    v.upsert(bad.clone()).unwrap();
    let mut row = v.store.get(&bad.id).unwrap().unwrap();
    let last = row.envelope.len() - 1;
    row.envelope[last] ^= 0x01;
    v.store.put(&row).unwrap();

    for res in [
        v.list::<Host>().map(|_| ()),
        v.get::<Host>(bad.id).map(|_| ()),
    ] {
        match res {
            Err(VaultError::Store(StoreError::Corrupt { id })) => assert_eq!(id, bad.id),
            other => panic!("expected Corrupt naming the record, got {other:?}"),
        }
    }
    assert_eq!(v.get::<Host>(good.id).unwrap().unwrap(), good);
}

#[test]
fn shared_vault_is_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<SharedVault>();
    let (_dir, _path, v, _) = created();
    let shared: SharedVault = Arc::new(std::sync::Mutex::new(v));
    let t = std::thread::spawn(move || shared.lock().unwrap().list::<Host>().unwrap().len());
    assert_eq!(t.join().unwrap(), 0);
}

#[test]
fn records_carry_vault_metadata() {
    let (_dir, _path, mut v, _) = created();
    let a = host("a", None);
    let b = host("b", None);
    v.upsert(a.clone()).unwrap();
    v.upsert(b.clone()).unwrap();
    let vault_id = v.load_account().unwrap().vault_id;
    let ra = v.store.get(&a.id).unwrap().unwrap();
    let rb = v.store.get(&b.id).unwrap().unwrap();
    assert_eq!(ra.vault_id, vault_id);
    assert_eq!(ra.kind, core_model::Kind::Host);
    assert_eq!(ra.key_version, 1);
    assert_eq!(ra.rev, None);
    assert!(rb.hlc > ra.hlc, "hlc increases per write");
    let ids: HashSet<Id> = v.list::<Host>().unwrap().iter().map(|h| h.id).collect();
    assert_eq!(ids, HashSet::from([a.id, b.id]));
}

/// A vault created with `keys` and a Touch ID-like `gate`.
fn gated(path: &Path, keys: &MemoryKeyStore, gate: &FakeGate) -> VaultService {
    VaultService::open_for_tests(path, Box::new(keys.clone()))
        .unwrap()
        .with_gate(Box::new(gate.clone()))
}

#[test]
fn enable_requires_presence() {
    let (_dir, path) = temp_path();
    let keys = MemoryKeyStore::default();
    let gate = FakeGate::new(b"fingers-1");
    let mut v = gated(&path, &keys, &gate);
    assert_eq!(v.quick_unlock_kind(), GateKind::TouchId);
    v.create(pw(PASSWORD)).unwrap();

    gate.answer(false);
    assert!(matches!(
        v.enable_quick_unlock(),
        Err(VaultError::PresenceDenied)
    ));
    assert!(!v.is_quick_unlock_enabled());
    assert!(keys.is_empty());

    gate.answer(true);
    v.enable_quick_unlock().unwrap();
    assert!(v.is_quick_unlock_enabled());
    assert_eq!(gate.confirms(), 2);
}

#[test]
fn quick_unlock_denied_keeps_locked() {
    let (_dir, path) = temp_path();
    let keys = MemoryKeyStore::default();
    let gate = FakeGate::new(b"fingers-1");
    let mut v = gated(&path, &keys, &gate);
    v.create(pw(PASSWORD)).unwrap();
    v.enable_quick_unlock().unwrap();
    v.lock();

    gate.answer(false);
    assert!(matches!(v.quick_unlock(), Err(VaultError::PresenceDenied)));
    assert!(!v.is_unlocked());
    assert!(v.is_quick_unlock_enabled());
    v.unlock(pw(PASSWORD)).unwrap();
    v.lock();

    gate.answer(true);
    v.quick_unlock().unwrap();
    assert!(v.is_unlocked());
}

#[test]
fn quick_unlock_biometry_changed_disables() {
    let (_dir, path) = temp_path();
    let keys = MemoryKeyStore::default();
    let gate = FakeGate::new(b"fingers-1");
    let mut v = gated(&path, &keys, &gate);
    v.create(pw(PASSWORD)).unwrap();
    v.enable_quick_unlock().unwrap();
    v.lock();

    gate.set_state(Ok(b"fingers-2".to_vec()));
    let confirms = gate.confirms();
    assert!(matches!(v.quick_unlock(), Err(VaultError::BiometryChanged)));
    assert_eq!(
        gate.confirms(),
        confirms,
        "no Touch ID prompt for a changed enrollment"
    );
    assert!(!v.is_unlocked());
    assert!(!v.is_quick_unlock_enabled());
    assert!(keys.is_empty());
    v.unlock(pw(PASSWORD)).unwrap();
}

#[test]
fn quick_unlock_unavailable_errors() {
    let (_dir, path) = temp_path();
    let keys = MemoryKeyStore::default();
    let gate = FakeGate::new(b"fingers-1");
    let mut v = gated(&path, &keys, &gate);
    v.create(pw(PASSWORD)).unwrap();

    gate.set_state(Err("no biometry".into()));
    assert!(matches!(
        v.enable_quick_unlock(),
        Err(VaultError::BiometryUnavailable(_))
    ));
    assert!(!v.is_quick_unlock_enabled());

    gate.set_state(Ok(b"fingers-1".to_vec()));
    v.enable_quick_unlock().unwrap();
    v.lock();
    gate.set_state(Err("lid closed".into()));
    assert!(matches!(
        v.quick_unlock(),
        Err(VaultError::BiometryUnavailable(_))
    ));
    assert!(
        v.is_quick_unlock_enabled(),
        "a closed lid does not turn quick unlock off"
    );
}

#[test]
fn legacy_enable_records_state_on_first_unlock() {
    // Enabled by a build without the gate: no enrollment state stored.
    let (_dir, path) = temp_path();
    let keys = MemoryKeyStore::default();
    let mut v = VaultService::open_for_tests(&path, Box::new(keys.clone())).unwrap();
    assert_eq!(v.quick_unlock_kind(), GateKind::None);
    v.create(pw(PASSWORD)).unwrap();
    v.enable_quick_unlock().unwrap();
    v.lock();
    drop(v);

    let gate = FakeGate::new(b"fingers-1");
    let mut v = gated(&path, &keys, &gate);
    v.quick_unlock().unwrap();
    assert_eq!(gate.confirms(), 1);
    v.lock();

    // The first gated unlock bound the wrap to the state: deleting the row
    // no longer lets changed fingers through.
    delete_biometry_row(&v);
    gate.set_state(Ok(b"fingers-2".to_vec()));
    assert!(v.quick_unlock().is_err());
    assert!(!v.is_unlocked());

    gate.set_state(Ok(b"fingers-1".to_vec()));
    v.quick_unlock().unwrap();
    v.lock();
    gate.set_state(Ok(b"fingers-2".to_vec()));
    assert!(matches!(v.quick_unlock(), Err(VaultError::BiometryChanged)));
}

fn delete_biometry_row(v: &VaultService) {
    v.store
        .account_delete(super::service::keys::BIOMETRY_STATE)
        .unwrap();
}

#[test]
fn deleted_biometry_row_does_not_skip_the_changed_check() {
    let (_dir, path) = temp_path();
    let keys = MemoryKeyStore::default();
    let gate = FakeGate::new(b"fingers-1");
    let mut v = gated(&path, &keys, &gate);
    v.create(pw(PASSWORD)).unwrap();
    v.enable_quick_unlock().unwrap();
    v.lock();

    delete_biometry_row(&v);
    gate.set_state(Ok(b"fingers-2".to_vec()));
    assert!(matches!(v.quick_unlock(), Err(VaultError::Keychain(_))));
    assert!(!v.is_unlocked());

    // The enrolled fingers still open it, and the row comes back.
    gate.set_state(Ok(b"fingers-1".to_vec()));
    v.quick_unlock().unwrap();
    v.lock();
    gate.set_state(Ok(b"fingers-2".to_vec()));
    assert!(matches!(v.quick_unlock(), Err(VaultError::BiometryChanged)));
}

/// The stored quick-unlock challenge, read straight from the account table.
fn hello_challenge(v: &VaultService) -> Option<Vec<u8>> {
    v.store
        .account_get(super::service::keys::HELLO_CHALLENGE)
        .unwrap()
}

#[test]
fn gate_kind_wire_names() {
    let name = |k: GateKind| serde_json::to_string(&k).unwrap();
    assert_eq!(name(GateKind::None), r#""none""#);
    assert_eq!(name(GateKind::TouchId), r#""touchId""#);
    assert_eq!(name(GateKind::WindowsHello), r#""windowsHello""#);
    assert_eq!(name(GateKind::FaceId), r#""faceId""#);
    assert_eq!(name(GateKind::AndroidBiometric), r#""androidBiometric""#);
}

#[test]
fn quick_unlock_words_follow_the_gate_kind() {
    use super::copy::{changed, denied, unavailable};
    assert_eq!(unavailable(GateKind::FaceId), "Face ID is not available: ");
    assert_eq!(
        changed(GateKind::FaceId),
        "Face ID changed; quick unlock was turned off"
    );
    assert_eq!(denied(GateKind::FaceId), "Face ID was not confirmed");
    // Touch ID (macOS, and iPads and iPhones with a home button).
    assert_eq!(
        unavailable(GateKind::TouchId),
        "Touch ID is not available: "
    );
    assert_eq!(
        changed(GateKind::TouchId),
        "Touch ID fingers changed; quick unlock was turned off"
    );
    assert_eq!(denied(GateKind::TouchId), "Touch ID was not confirmed");
    // Windows Hello's reasons are whole sentences.
    assert_eq!(unavailable(GateKind::WindowsHello), "");
    assert_eq!(
        changed(GateKind::WindowsHello),
        "Windows Hello changed; quick unlock was turned off"
    );
    assert_eq!(
        denied(GateKind::WindowsHello),
        "Windows Hello was not confirmed"
    );
    // Android's reasons are whole sentences too (from the system).
    assert_eq!(unavailable(GateKind::AndroidBiometric), "");
    assert_eq!(
        changed(GateKind::AndroidBiometric),
        "Biometrics changed; quick unlock was turned off"
    );
    assert_eq!(
        denied(GateKind::AndroidBiometric),
        "Biometric unlock was not confirmed"
    );
    assert_eq!(
        unavailable(GateKind::None),
        "quick unlock is not available: "
    );
}

#[test]
fn a_presence_gate_of_unknown_kind_still_asks() {
    // An iOS gate on a device whose biometry type it does not know reports
    // no kind, but quick unlock must never skip the check for it.
    let (_dir, path) = temp_path();
    let keys = MemoryKeyStore::default();
    let gate = FakeGate::new(b"faces-1");
    gate.set_kind(GateKind::None);
    let mut v = gated(&path, &keys, &gate);
    assert_eq!(v.quick_unlock_kind(), GateKind::None);
    v.create(pw(PASSWORD)).unwrap();

    gate.answer(false);
    assert!(matches!(
        v.enable_quick_unlock(),
        Err(VaultError::PresenceDenied)
    ));
    assert!(keys.is_empty());
    gate.answer(true);
    v.enable_quick_unlock().unwrap();
    v.lock();

    gate.answer(false);
    assert!(matches!(v.quick_unlock(), Err(VaultError::PresenceDenied)));
    gate.set_state(Ok(b"faces-2".to_vec()));
    assert!(matches!(v.quick_unlock(), Err(VaultError::BiometryChanged)));
    assert!(!v.is_quick_unlock_enabled());
}

#[test]
fn binding_gate_enable_then_quick_unlock_round_trips() {
    let (_dir, path) = temp_path();
    let keys = MemoryKeyStore::default();
    let gate = FakeGate::binding(b"hello-seed-1");
    let mut v = gated(&path, &keys, &gate);
    assert_eq!(v.quick_unlock_kind(), GateKind::WindowsHello);
    v.create(pw(PASSWORD)).unwrap();
    v.upsert(host("h", None)).unwrap();

    v.enable_quick_unlock().unwrap();
    assert!(v.is_quick_unlock_enabled());
    assert_eq!(hello_challenge(&v).map(|c| c.len()), Some(32));
    v.lock();

    v.quick_unlock().unwrap();
    assert!(v.is_unlocked());
    assert_eq!(v.list::<Host>().unwrap().len(), 1);
    assert_eq!(gate.confirms(), 2, "one prompt to enable, one to unlock");
    assert!(
        keys.is_empty(),
        "a key-binding gate keeps nothing in the keychain"
    );

    v.disable_quick_unlock().unwrap();
    assert!(!v.is_quick_unlock_enabled());
    assert_eq!(hello_challenge(&v), None);
}

#[test]
fn hello_reset_turns_quick_unlock_off() {
    let (_dir, path) = temp_path();
    let keys = MemoryKeyStore::default();
    let gate = FakeGate::binding(b"hello-seed-1");
    let mut v = gated(&path, &keys, &gate);
    v.create(pw(PASSWORD)).unwrap();
    v.upsert(host("h", None)).unwrap();
    v.enable_quick_unlock().unwrap();
    v.lock();

    // Windows Hello was reset or removed: the same challenge now signs to a
    // different key.
    gate.set_seed(b"other");
    assert!(matches!(v.quick_unlock(), Err(VaultError::BiometryChanged)));
    assert!(!v.is_unlocked());
    assert!(!v.is_quick_unlock_enabled());
    assert_eq!(hello_challenge(&v), None);

    let confirms = gate.confirms();
    assert!(matches!(
        v.quick_unlock(),
        Err(VaultError::QuickUnlockNotEnabled)
    ));
    assert_eq!(gate.confirms(), confirms, "no second Hello prompt");

    v.unlock(pw(PASSWORD)).unwrap();
    assert_eq!(v.list::<Host>().unwrap().len(), 1);
}

#[test]
fn binding_gate_cancel_keeps_quick_unlock_on() {
    let (_dir, path) = temp_path();
    let keys = MemoryKeyStore::default();
    let gate = FakeGate::binding(b"hello-seed-1");
    let mut v = gated(&path, &keys, &gate);
    v.create(pw(PASSWORD)).unwrap();

    gate.answer(false);
    assert!(matches!(
        v.enable_quick_unlock(),
        Err(VaultError::PresenceDenied)
    ));
    assert!(!v.is_quick_unlock_enabled());
    assert_eq!(hello_challenge(&v), None);

    gate.answer(true);
    v.enable_quick_unlock().unwrap();
    v.lock();

    gate.answer(false);
    assert!(matches!(v.quick_unlock(), Err(VaultError::PresenceDenied)));
    assert!(!v.is_unlocked());
    assert!(v.is_quick_unlock_enabled(), "a cancel does not turn it off");

    gate.answer(true);
    v.quick_unlock().unwrap();
    assert!(v.is_unlocked());
}

#[test]
fn binding_gate_unavailable_keeps_quick_unlock_on() {
    let (_dir, path) = temp_path();
    let keys = MemoryKeyStore::default();
    let gate = FakeGate::binding(b"hello-seed-1");
    let mut v = gated(&path, &keys, &gate);
    v.create(pw(PASSWORD)).unwrap();
    v.enable_quick_unlock().unwrap();
    v.lock();

    let confirms = gate.confirms();
    gate.set_state(Err("Windows Hello is not set up".into()));
    assert!(matches!(
        v.quick_unlock(),
        Err(VaultError::BiometryUnavailable(_))
    ));
    assert_eq!(gate.confirms(), confirms, "no prompt while unavailable");
    assert!(v.is_quick_unlock_enabled());

    gate.set_state(Ok(b"available".to_vec()));
    v.quick_unlock().unwrap();
    assert!(v.is_unlocked());
}

#[test]
fn missing_challenge_turns_quick_unlock_off() {
    // The challenge row is gone (damaged or hand-edited account table).
    let (_dir, path) = temp_path();
    let keys = MemoryKeyStore::default();
    let gate = FakeGate::binding(b"hello-seed-1");
    let mut v = gated(&path, &keys, &gate);
    v.create(pw(PASSWORD)).unwrap();
    v.enable_quick_unlock().unwrap();
    v.lock();
    v.store
        .account_delete(super::service::keys::HELLO_CHALLENGE)
        .unwrap();

    let confirms = gate.confirms();
    assert!(matches!(v.quick_unlock(), Err(VaultError::BiometryChanged)));
    assert_eq!(gate.confirms(), confirms, "no prompt without a challenge");
    assert!(!v.is_quick_unlock_enabled());
    v.unlock(pw(PASSWORD)).unwrap();
    v.lock();
    drop(v);

    // Enabled by a build with a keychain device key (no challenge), then
    // opened behind a key-binding gate: turned off once, keychain cleared.
    let (_dir2, path2) = temp_path();
    let mut v = VaultService::open_for_tests(&path2, Box::new(keys.clone())).unwrap();
    v.create(pw(PASSWORD)).unwrap();
    v.enable_quick_unlock().unwrap();
    assert_eq!(keys.len(), 1);
    v.lock();
    drop(v);

    let mut v = gated(&path2, &keys, &gate);
    let confirms = gate.confirms();
    assert!(matches!(v.quick_unlock(), Err(VaultError::BiometryChanged)));
    assert_eq!(gate.confirms(), confirms);
    assert!(!v.is_quick_unlock_enabled());
    assert!(keys.is_empty());
    v.unlock(pw(PASSWORD)).unwrap();
}

#[test]
fn save_host_password_sets_identity_password() {
    let (_dir, _path, mut v, _) = created();
    let ident = Identity {
        password: None,
        ..identity(None)
    };
    let mut h = host("web", None);
    h.settings.identity_id = Some(ident.id);
    v.upsert(ident.clone()).unwrap();
    v.upsert(h.clone()).unwrap();

    assert_eq!(
        v.password_save_target(h.id).unwrap(),
        Some(SaveTarget {
            identity_label: "prod".into(),
            is_new: false
        })
    );
    let saved = v.save_host_password(h.id, "root", pw("s3cret")).unwrap();
    assert_eq!(saved, Some(ident.id));
    let stored = v.get::<Identity>(ident.id).unwrap().unwrap();
    assert_eq!(stored.password.unwrap().expose_secret(), "s3cret");
    assert_eq!(v.list::<Identity>().unwrap().len(), 1);
}

#[test]
fn save_host_password_creates_identity_when_none() {
    let (_dir, _path, mut v, _) = created();
    let h = host("web", None);
    v.upsert(h.clone()).unwrap();

    assert_eq!(
        v.password_save_target(h.id).unwrap(),
        Some(SaveTarget {
            identity_label: "web".into(),
            is_new: true
        })
    );
    let id = v
        .save_host_password(h.id, "alice", pw("s3cret"))
        .unwrap()
        .unwrap();
    let ident = v.get::<Identity>(id).unwrap().unwrap();
    assert_eq!(
        (ident.label.as_str(), ident.username.as_str()),
        ("web", "alice")
    );
    assert_eq!(ident.password.unwrap().expose_secret(), "s3cret");
    assert_eq!(ident.key_id, None);
    let h = v.get::<Host>(h.id).unwrap().unwrap();
    assert_eq!(h.settings.identity_id, Some(id));
}

#[test]
fn save_host_password_refuses_key_identity() {
    let (_dir, _path, mut v, _) = created();
    let key = ssh_key();
    let ident = Identity {
        password: None,
        ..identity(Some(key.id))
    };
    let mut h = host("web", None);
    h.settings.identity_id = Some(ident.id);
    v.upsert(key).unwrap();
    v.upsert(ident.clone()).unwrap();
    v.upsert(h.clone()).unwrap();

    assert_eq!(v.password_save_target(h.id).unwrap(), None);
    assert_eq!(
        v.save_host_password(h.id, "root", pw("s3cret")).unwrap(),
        None
    );
    assert!(
        v.get::<Identity>(ident.id)
            .unwrap()
            .unwrap()
            .password
            .is_none()
    );
}

#[test]
fn identity_without_owner_decodes() {
    // Records written before own logins existed have no `owner`.
    let old = serde_json::json!({
        "id": Id::now_v7(), "label": "prod", "username": "root",
        "password": null, "key_id": null
    });
    let ident: Identity = serde_json::from_value(old).unwrap();
    assert_eq!(ident.owner, None);
}

#[test]
fn set_own_login_creates_links_and_updates() {
    let (_dir, _path, mut v, _) = created();
    let h = host("web", None);
    v.upsert(h.clone()).unwrap();

    let first = v
        .set_own_login(h.id, "alice".into(), PasswordChange::Set(pw("one")))
        .unwrap();
    assert_eq!(first.owner, Some(h.id));
    assert_eq!(
        (first.label.as_str(), first.username.as_str()),
        ("web", "alice")
    );
    assert_eq!(
        v.get::<Host>(h.id).unwrap().unwrap().settings.identity_id,
        Some(first.id)
    );

    // Same identity updated; Keep leaves the password, Clear removes it.
    let again = v
        .set_own_login(h.id, "bob".into(), PasswordChange::Keep)
        .unwrap();
    assert_eq!(again.id, first.id);
    let stored = v.get::<Identity>(first.id).unwrap().unwrap();
    assert_eq!(stored.username, "bob");
    assert_eq!(stored.password.unwrap().expose_secret(), "one");
    v.set_own_login(h.id, "bob".into(), PasswordChange::Clear)
        .unwrap();
    assert!(
        v.get::<Identity>(first.id)
            .unwrap()
            .unwrap()
            .password
            .is_none()
    );
    assert_eq!(v.list::<Identity>().unwrap().len(), 1);
}

#[test]
fn set_own_login_on_group_is_inherited() {
    let (_dir, _path, mut v, _) = created();
    let g = group("Production", None);
    let h = host("web", Some(g.id));
    v.upsert(g.clone()).unwrap();
    v.upsert(h.clone()).unwrap();
    let ident = v
        .set_own_login(g.id, "alice".into(), PasswordChange::Set(pw("pw")))
        .unwrap();
    assert_eq!(ident.label, "Production");
    let eff = v.effective_settings(h.id).unwrap();
    assert_eq!(eff.identity_id.value, Some(ident.id));
    assert_eq!(eff.identity_id.source, Source::Group(g.id));
}

#[test]
fn set_own_login_refuses_unknown_owner() {
    let (_dir, _path, mut v, _) = created();
    let missing = Id::now_v7();
    assert!(matches!(
        v.set_own_login(missing, "x".into(), PasswordChange::Keep),
        Err(VaultError::MissingRef(id)) if id == missing
    ));
}

#[test]
fn prune_removes_unlinked_own_login() {
    let (_dir, _path, mut v, _) = created();
    let h = host("web", None);
    v.upsert(h.clone()).unwrap();
    let own = v
        .set_own_login(h.id, "alice".into(), PasswordChange::Keep)
        .unwrap();
    let shared = identity(None);
    v.upsert(shared.clone()).unwrap();
    assert_eq!(v.tidy_own_logins().unwrap(), 0);

    // The host switches to a Keychain identity: its own login goes.
    let mut h = v.get::<Host>(h.id).unwrap().unwrap();
    h.settings.identity_id = Some(shared.id);
    v.upsert(h.clone()).unwrap();
    assert_eq!(v.tidy_own_logins().unwrap(), 1);
    assert!(v.get::<Identity>(own.id).unwrap().is_none());
    assert!(
        v.get::<Identity>(shared.id).unwrap().is_some(),
        "Keychain identities stay"
    );
}

#[test]
fn prune_after_recursive_group_delete() {
    let (_dir, _path, mut v, _) = created();
    let g = group("Production", None);
    let h = host("web", Some(g.id));
    v.upsert(g.clone()).unwrap();
    v.upsert(h.clone()).unwrap();
    v.set_own_login(g.id, "alice".into(), PasswordChange::Keep)
        .unwrap();
    v.set_own_login(h.id, "root".into(), PasswordChange::Keep)
        .unwrap();
    v.delete_group(g.id, ChildPolicy::DeleteRecursive).unwrap();
    assert_eq!(v.tidy_own_logins().unwrap(), 2);
    assert!(v.list::<Identity>().unwrap().is_empty());
}

#[test]
fn save_host_password_creates_own_login() {
    let (_dir, _path, mut v, _) = created();
    let h = host("web", None);
    v.upsert(h.clone()).unwrap();
    let id = v
        .save_host_password(h.id, "alice", pw("s3cret"))
        .unwrap()
        .unwrap();
    let ident = v.get::<Identity>(id).unwrap().unwrap();
    assert_eq!(ident.owner, Some(h.id));
    assert_eq!(ident.password.unwrap().expose_secret(), "s3cret");
}

#[test]
fn another_records_own_login_cannot_be_linked() {
    let (_dir, _path, mut v, _) = created();
    let g = group("Production", None);
    let h = host("web", Some(g.id));
    v.upsert(g.clone()).unwrap();
    v.upsert(h.clone()).unwrap();
    let own = v
        .set_own_login(g.id, "alice".into(), PasswordChange::Keep)
        .unwrap();

    // A host (or another group) cannot point at the group's own login; it inherits it.
    let mut linked = h.clone();
    linked.settings.identity_id = Some(own.id);
    assert!(matches!(v.upsert(linked), Err(VaultError::ForeignOwnLogin(id)) if id == own.id));
    let mut sub = group("Web", Some(g.id));
    sub.settings.identity_id = Some(own.id);
    assert!(matches!(v.upsert(sub), Err(VaultError::ForeignOwnLogin(_))));
    // The owner itself can.
    let g = v.get::<Group>(g.id).unwrap().unwrap();
    v.upsert(g).unwrap();
}

/// Seals `plaintext` as the stored envelope of host `id`, as a newer app
/// version (with fields this one does not know) would have written it.
fn put_raw_host(v: &VaultService, id: Id, plaintext: &serde_json::Value) {
    let keys = v.unlocked.as_ref().unwrap();
    let row = core_store::seal_row(
        keys.vault_key.expose_secret(),
        keys.vault_id,
        core_model::Kind::Host,
        id,
        keys.key_version,
        Hlc::now(&keys.last_hlc.get(), keys.node),
        &serde_json::to_vec(plaintext).unwrap(),
    );
    v.store.put(&row).unwrap();
}

#[test]
fn upsert_keeps_unknown_fields() {
    let (_dir, _path, mut v, _) = created();
    let h = host("web", None);
    let mut raw = serde_json::to_value(&h).unwrap();
    raw["future_field"] = serde_json::json!({"a": 1});
    put_raw_host(&v, h.id, &raw);

    // An editor that never saw `future_field` saves the host back.
    let mut edited = h.clone();
    edited.label = "web2".into();
    assert!(edited.extra.0.is_empty());
    v.upsert(edited).unwrap();

    let back = v.get::<Host>(h.id).unwrap().unwrap();
    assert_eq!(back.label, "web2");
    assert_eq!(back.extra.0["future_field"], serde_json::json!({"a": 1}));
}

#[test]
fn upsert_prefers_the_callers_unknown_fields() {
    let (_dir, _path, mut v, _) = created();
    let h = host("web", None);
    let mut raw = serde_json::to_value(&h).unwrap();
    raw["future_field"] = serde_json::json!(1);
    put_raw_host(&v, h.id, &raw);

    let mut edited = h.clone();
    edited.extra.0.insert("future_field".into(), 2.into());
    v.upsert(edited).unwrap();
    let back = v.get::<Host>(h.id).unwrap().unwrap();
    assert_eq!(back.extra.0["future_field"], serde_json::json!(2));
}

#[test]
fn conflict_records_store_but_stay_out_of_other_lists() {
    let (_dir, _path, mut v, _) = created();
    let h = host("web", None);
    v.upsert(h.clone()).unwrap();
    let conflict = Conflict {
        id: Id::now_v7(),
        record_id: h.id,
        record_kind: core_model::Kind::Host,
        key_version: 1,
        hlc: Hlc {
            wall_ms: 5,
            counter: 0,
            node: 9,
        },
        deleted: false,
        envelope: vec![1, 2, 3],
        detected_at: 1_700_000_000,
        extra: Extra::default(),
    };
    v.upsert(conflict.clone()).unwrap();
    assert_eq!(v.list::<Conflict>().unwrap(), vec![conflict.clone()]);
    assert_eq!(v.list::<Host>().unwrap(), vec![h]);
    assert!(v.list::<Identity>().unwrap().is_empty());
    v.delete::<Conflict>(conflict.id).unwrap();
    assert!(v.list::<Conflict>().unwrap().is_empty());
}

/// Makes every later insert or delete of account row `k` fail, through a
/// second connection to the vault file.
fn fail_account_writes_of(path: &Path, k: &str) {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch(&format!(
        "CREATE TRIGGER fail_put BEFORE INSERT ON account WHEN NEW.k = '{k}' \
             BEGIN SELECT RAISE(ABORT, 'injected'); END; \
         CREATE TRIGGER fail_delete BEFORE DELETE ON account WHEN OLD.k = '{k}' \
             BEGIN SELECT RAISE(ABORT, 'injected'); END;"
    ))
    .unwrap();
}

fn allow_account_writes(path: &Path) {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch("DROP TRIGGER fail_put; DROP TRIGGER fail_delete;")
        .unwrap();
}

#[test]
fn binding_enable_and_disable_write_all_rows_or_none() {
    let (_dir, path) = temp_path();
    let keys = MemoryKeyStore::default();
    let gate = FakeGate::binding(b"hello-seed-1");
    let mut v = gated(&path, &keys, &gate);
    v.create(pw(PASSWORD)).unwrap();

    // The wrapped secret is written after the challenge: its failure must
    // take the challenge with it.
    fail_account_writes_of(&path, "account_secret.device");
    assert!(matches!(v.enable_quick_unlock(), Err(VaultError::Store(_))));
    allow_account_writes(&path);
    assert!(!v.is_quick_unlock_enabled());
    assert_eq!(hello_challenge(&v), None, "a half-enabled quick unlock");

    v.enable_quick_unlock().unwrap();
    // The challenge is deleted after the wrapped secret: its failure must
    // keep the wrapped secret.
    fail_account_writes_of(&path, "quick_unlock.hello_challenge");
    assert!(matches!(
        v.disable_quick_unlock(),
        Err(VaultError::Store(_))
    ));
    allow_account_writes(&path);
    assert!(v.is_quick_unlock_enabled(), "a half-disabled quick unlock");
    v.lock();
    v.quick_unlock().unwrap();
}

#[test]
fn binding_enable_deletes_the_keychain_key_first() {
    // Enabled with a plain keychain device key, then turned on again
    // behind a key-binding gate.
    let (_dir, path) = temp_path();
    let keys = DeleteFailsKeyStore::default();
    let mut v = VaultService::open_for_tests(&path, Box::new(keys.clone())).unwrap();
    v.create(pw(PASSWORD)).unwrap();
    v.enable_quick_unlock().unwrap();
    assert_eq!(keys.keys.len(), 1);
    v.lock();
    drop(v);

    let gate = FakeGate::binding(b"hello-seed-1");
    let mut v = VaultService::open_for_tests(&path, Box::new(keys.clone()))
        .unwrap()
        .with_gate(Box::new(gate.clone()));
    v.unlock(pw(PASSWORD)).unwrap();
    keys.fail_delete(true);
    assert!(matches!(
        v.enable_quick_unlock(),
        Err(VaultError::Keychain(_))
    ));
    assert_eq!(keys.keys.len(), 1, "the plain key is still there");
    assert_eq!(hello_challenge(&v), None, "nothing written");
    assert!(v.is_quick_unlock_enabled(), "the prior setup is untouched");

    keys.fail_delete(false);
    v.enable_quick_unlock().unwrap();
    assert!(keys.keys.is_empty(), "no plain key once enabled");
    v.lock();
    v.quick_unlock().unwrap();
}

#[test]
fn hello_reset_reason_survives_a_failing_keychain_delete() {
    let (_dir, path) = temp_path();
    let keys = DeleteFailsKeyStore::default();
    let gate = FakeGate::binding(b"hello-seed-1");
    let mut v = VaultService::open_for_tests(&path, Box::new(keys.clone()))
        .unwrap()
        .with_gate(Box::new(gate.clone()));
    v.create(pw(PASSWORD)).unwrap();
    v.enable_quick_unlock().unwrap();
    v.lock();

    // Unwrap fails (Hello reset).
    keys.fail_delete(true);
    gate.set_seed(b"other");
    assert!(matches!(v.quick_unlock(), Err(VaultError::BiometryChanged)));
    assert!(!v.is_quick_unlock_enabled());
    assert_eq!(hello_challenge(&v), None);
    assert!(matches!(
        v.quick_unlock(),
        Err(VaultError::QuickUnlockNotEnabled)
    ));

    // Challenge missing.
    v.unlock(pw(PASSWORD)).unwrap();
    keys.fail_delete(false);
    v.enable_quick_unlock().unwrap();
    v.lock();
    v.store
        .account_delete(super::service::keys::HELLO_CHALLENGE)
        .unwrap();
    keys.fail_delete(true);
    assert!(matches!(v.quick_unlock(), Err(VaultError::BiometryChanged)));
    assert!(!v.is_quick_unlock_enabled());
    v.unlock(pw(PASSWORD)).unwrap();
}

#[test]
fn bound_vault_behind_a_plain_gate_turns_quick_unlock_off() {
    // Enabled behind a key-binding gate, then opened behind Touch ID or no
    // gate: there is no keychain key to read, so prompting would fail
    // forever.
    for plain in [Some(FakeGate::new(b"fingers-1")), None] {
        let (_dir, path) = temp_path();
        let keys = MemoryKeyStore::default();
        let mut v = gated(&path, &keys, &FakeGate::binding(b"hello-seed-1"));
        v.create(pw(PASSWORD)).unwrap();
        v.enable_quick_unlock().unwrap();
        v.lock();
        drop(v);

        let mut v = VaultService::open_for_tests(&path, Box::new(keys.clone())).unwrap();
        if let Some(g) = &plain {
            v = v.with_gate(Box::new(g.clone()));
        }
        assert!(matches!(v.quick_unlock(), Err(VaultError::BiometryChanged)));
        if let Some(g) = &plain {
            assert_eq!(g.confirms(), 0, "no prompt");
        }
        assert!(!v.is_quick_unlock_enabled());
        assert_eq!(hello_challenge(&v), None);
        assert!(matches!(
            v.quick_unlock(),
            Err(VaultError::QuickUnlockNotEnabled)
        ));
        v.unlock(pw(PASSWORD)).unwrap();
    }
}

/// The quick unlock errors the app shows use this platform's words (the Mac
/// keeps its Touch ID copy).
#[test]
fn quick_unlock_errors_name_the_platform() {
    let unavailable = VaultError::BiometryUnavailable("no sensor".into()).to_string();
    let changed = VaultError::BiometryChanged.to_string();
    let denied = VaultError::PresenceDenied.to_string();
    let keychain = VaultError::Keychain("locked".into()).to_string();
    if cfg!(target_os = "macos") {
        assert_eq!(unavailable, "Touch ID is not available: no sensor");
        assert_eq!(
            changed,
            "Touch ID fingers changed; quick unlock was turned off"
        );
        assert_eq!(denied, "Touch ID was not confirmed");
        assert_eq!(keychain, "keychain: locked");
    } else if cfg!(windows) {
        assert_eq!(unavailable, "no sensor");
        assert_eq!(
            changed,
            "Windows Hello changed; quick unlock was turned off"
        );
        assert_eq!(denied, "Windows Hello was not confirmed");
        assert_eq!(keychain, "Credential Manager: locked");
    } else {
        assert_eq!(unavailable, "quick unlock is not available: no sensor");
        assert!(changed.starts_with("quick unlock was turned off"));
        assert_eq!(denied, "quick unlock was not confirmed");
        assert_eq!(keychain, "keychain: locked");
    }
}

/// A Hello credential removed since enabling: unlocking must not create a
/// new one (a useless create prompt at app start). Quick unlock turns off
/// with `BiometryChanged` and nobody is asked; enabling again creates it.
#[test]
fn missing_hello_credential_turns_quick_unlock_off_without_a_prompt() {
    let (_dir, path) = temp_path();
    let keys = MemoryKeyStore::default();
    let gate = FakeGate::binding(b"hello-seed-1");
    let mut v = gated(&path, &keys, &gate);
    v.create(pw(PASSWORD)).unwrap();
    v.enable_quick_unlock().unwrap();
    v.lock();

    gate.remove_credential();
    let asked = gate.confirms();
    assert!(matches!(v.quick_unlock(), Err(VaultError::BiometryChanged)));
    assert_eq!(gate.confirms(), asked, "no prompt");
    assert!(!v.is_quick_unlock_enabled());
    assert_eq!(hello_challenge(&v), None);
    assert!(matches!(
        v.quick_unlock(),
        Err(VaultError::QuickUnlockNotEnabled)
    ));

    v.unlock(pw(PASSWORD)).unwrap();
    v.enable_quick_unlock().unwrap();
    assert!(gate.confirms() > asked, "enabling creates (and is asked)");
    v.lock();
    v.quick_unlock().unwrap();
}

#[test]
fn snippets_crud() {
    let (_dir, _path, mut v, _) = created();
    let s = core_model::Snippet {
        id: Id::now_v7(),
        label: "Disk usage".into(),
        text: "df -h".into(),
        run: true,
        extra: Default::default(),
    };
    v.upsert(s.clone()).unwrap();
    assert_eq!(v.list::<core_model::Snippet>().unwrap(), vec![s.clone()]);
    v.delete::<core_model::Snippet>(s.id).unwrap();
    assert!(v.list::<core_model::Snippet>().unwrap().is_empty());
}

/// Scripted Android Keystore for [`KeystoreGate`]: SHA-256(generation ‖
/// challenge) stands in for the Keystore key's HMAC. Clones share state.
#[derive(Clone, Default)]
struct FakeKeystore(Arc<std::sync::Mutex<FakeKeystoreState>>);

#[derive(Default)]
struct FakeKeystoreState {
    /// Which key exists: 0 = none yet; each new key signs differently.
    generation: u32,
    /// Biometrics were enrolled or removed since the key was made.
    invalidated: bool,
    /// What the prompt answers instead of a MAC (cancel, lockout).
    fail: Option<KeystoreError>,
    prompts: usize,
}

impl FakeKeystore {
    fn lock(&self) -> std::sync::MutexGuard<'_, FakeKeystoreState> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl KeystoreBackend for FakeKeystore {
    fn hmac(
        &self,
        challenge: &[u8],
        _reason: &str,
        create: bool,
    ) -> Result<zeroize::Zeroizing<Vec<u8>>, KeystoreError> {
        use sha2::{Digest, Sha256};
        let mut s = self.lock();
        // Found before any prompt, as `Mac.init` does.
        if s.invalidated || s.generation == 0 {
            match (create, s.invalidated) {
                (false, true) => return Err(KeystoreError::Invalidated),
                (false, false) => return Err(KeystoreError::Missing),
                // Enabling replaces the key.
                (true, _) => {
                    s.generation += 1;
                    s.invalidated = false;
                }
            }
        }
        s.prompts += 1;
        if let Some(e) = s.fail.clone() {
            return Err(e);
        }
        let mut h = Sha256::new();
        h.update(s.generation.to_le_bytes());
        h.update(challenge);
        Ok(zeroize::Zeroizing::new(h.finalize().to_vec()))
    }
}

fn keystore_gated(path: &Path, keys: &MemoryKeyStore, ks: &FakeKeystore) -> VaultService {
    VaultService::open_for_tests(path, Box::new(keys.clone()))
        .unwrap()
        .with_gate(Box::new(KeystoreGate(ks.clone())))
}

#[test]
fn keystore_gate_enable_then_unlock() {
    let (_dir, path) = temp_path();
    let keys = MemoryKeyStore::default();
    let ks = FakeKeystore::default();
    let mut v = keystore_gated(&path, &keys, &ks);
    assert_eq!(v.quick_unlock_kind(), GateKind::AndroidBiometric);
    v.create(pw(PASSWORD)).unwrap();
    v.upsert(host("h", None)).unwrap();

    v.enable_quick_unlock().unwrap();
    assert_eq!(hello_challenge(&v).map(|c| c.len()), Some(32));
    v.lock();
    v.quick_unlock().unwrap();
    assert_eq!(v.list::<Host>().unwrap().len(), 1);
    assert_eq!(ks.lock().prompts, 2, "one prompt to enable, one to unlock");
    // A key-binding gate: no device key, ever, for anyone holding the
    // unlocked phone to read.
    assert!(
        keys.is_empty(),
        "the Keystore gate keeps nothing in the keychain"
    );
}

#[test]
fn keystore_gate_invalidated_turns_off() {
    let (_dir, path) = temp_path();
    let keys = MemoryKeyStore::default();
    let ks = FakeKeystore::default();
    let mut v = keystore_gated(&path, &keys, &ks);
    v.create(pw(PASSWORD)).unwrap();
    v.enable_quick_unlock().unwrap();
    v.lock();

    // A fingerprint was added: Android invalidated the key.
    ks.lock().invalidated = true;
    let prompts = ks.lock().prompts;
    assert!(matches!(v.quick_unlock(), Err(VaultError::BiometryChanged)));
    assert_eq!(ks.lock().prompts, prompts, "no prompt");
    assert!(!v.is_quick_unlock_enabled());
    assert_eq!(hello_challenge(&v), None);
    assert!(
        matches!(v.quick_unlock(), Err(VaultError::QuickUnlockNotEnabled)),
        "reported once"
    );

    v.unlock(pw(PASSWORD)).unwrap();
    v.enable_quick_unlock().unwrap();
    v.lock();
    v.quick_unlock().unwrap();
    assert!(keys.is_empty());
}

#[test]
fn keystore_gate_cancel_stays_on() {
    let (_dir, path) = temp_path();
    let keys = MemoryKeyStore::default();
    let ks = FakeKeystore::default();
    let mut v = keystore_gated(&path, &keys, &ks);
    v.create(pw(PASSWORD)).unwrap();
    v.enable_quick_unlock().unwrap();
    v.lock();

    ks.lock().fail = Some(KeystoreError::Cancelled);
    assert!(matches!(v.quick_unlock(), Err(VaultError::PresenceDenied)));
    assert!(v.is_quick_unlock_enabled(), "a cancel does not turn it off");

    // Too many attempts, or no biometrics right now: also stays on.
    ks.lock().fail = Some(KeystoreError::Unavailable("Too many attempts.".into()));
    assert!(matches!(
        v.quick_unlock(),
        Err(VaultError::BiometryUnavailable(m)) if m == "Too many attempts."
    ));
    assert!(v.is_quick_unlock_enabled());

    ks.lock().fail = None;
    v.quick_unlock().unwrap();
    assert!(v.is_unlocked());
}

#[test]
fn keystore_gate_missing_without_create_is_changed() {
    let (_dir, path) = temp_path();
    let keys = MemoryKeyStore::default();
    let ks = FakeKeystore::default();
    let mut v = keystore_gated(&path, &keys, &ks);
    v.create(pw(PASSWORD)).unwrap();
    v.enable_quick_unlock().unwrap();
    v.lock();

    // The Keystore key is gone (app data cleared on a restored vault file).
    ks.lock().generation = 0;
    let prompts = ks.lock().prompts;
    assert!(matches!(v.quick_unlock(), Err(VaultError::BiometryChanged)));
    assert_eq!(ks.lock().prompts, prompts, "unlocking never creates a key");
    assert!(!v.is_quick_unlock_enabled());

    v.unlock(pw(PASSWORD)).unwrap();
    v.enable_quick_unlock().unwrap();
    v.lock();
    v.quick_unlock().unwrap();
}

/// Android's gate when the Keystore plugin cannot be reached: quick unlock
/// is unavailable, never an ungated keychain key.
#[test]
fn unreachable_keystore_never_falls_back_to_a_keychain_key() {
    let (_dir, path) = temp_path();
    let keys = MemoryKeyStore::default();
    let mut v = VaultService::open_for_tests(&path, Box::new(keys.clone()))
        .unwrap()
        .with_gate(Box::new(KeystoreGate(UnreachableKeystore)));
    assert_eq!(v.quick_unlock_kind(), GateKind::AndroidBiometric);
    v.create(pw(PASSWORD)).unwrap();
    assert!(matches!(
        v.enable_quick_unlock(),
        Err(VaultError::BiometryUnavailable(_))
    ));
    assert!(!v.is_quick_unlock_enabled());
    assert!(keys.is_empty());
}

#[test]
fn verify_password_checks_without_changing_the_session() {
    let (_dir, _path, mut v, _) = created();
    v.verify_password(&pw(PASSWORD)).unwrap();
    assert!(matches!(
        // Made at run time: a literal trips CodeQL's hard-coded credential rule.
        v.verify_password(&pw(&Id::now_v7().to_string())),
        Err(VaultError::WrongPassword)
    ));
    assert!(v.is_unlocked());
    v.lock();
    assert!(matches!(
        v.verify_password(&pw(PASSWORD)),
        Err(VaultError::Locked)
    ));
    assert!(!v.is_unlocked());
}
