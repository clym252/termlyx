//! The vault's side of the sync account flows: key material out and in,
//! password change, joining from server material, recovery, and the resync
//! merge rule for clean rows without a revision.

use core_api::vault::{
    MemoryKeyStore, VaultError, VaultService, recovery_reset_for, validate_material,
};
use core_model::{ConnSettings, Hlc, Host, Id, Kind};
use core_store::InboundRow;
use secrecy::{ExposeSecret, SecretString};
use tempfile::TempDir;

fn pw(s: &str) -> SecretString {
    SecretString::from(s)
}

fn open(dir: &TempDir, name: &str) -> VaultService {
    VaultService::open_for_tests(&dir.path().join(name), Box::new(MemoryKeyStore::default()))
        .unwrap()
}

/// A created, unlocked vault and its recovery key text.
fn created(dir: &TempDir) -> (VaultService, String) {
    let mut v = open(dir, "a.db");
    let recovery = v.create(pw("old")).unwrap();
    (v, recovery)
}

fn host(label: &str) -> Host {
    Host {
        id: Id::now_v7(),
        group_id: None,
        label: label.into(),
        address: "10.0.0.1".into(),
        tag_ids: vec![],
        settings: ConnSettings::default(),
        forward_rule_ids: vec![],
        extra: Default::default(),
    }
}

#[test]
fn account_material_matches_the_local_account() {
    let dir = tempfile::tempdir().unwrap();
    let (v, _) = created(&dir);
    let m = v.account_material().unwrap();
    assert_eq!(m.vault_id, v.vault_id().unwrap().to_string());
    assert_eq!(m.key_version, 1);
    assert_eq!(m.kdf.salt.len(), 16);
    assert_eq!(m.account_public.len(), 32);
    assert_eq!(m.secret_by_kek.len(), 73);
    assert_eq!(m.secret_by_recovery.len(), 73);
    assert_eq!(m.sealed_vault_key.len(), 81);
    validate_material(&m, &v.kdf_floor().unwrap()).unwrap();
}

#[test]
fn auth_key_checks_the_password_and_is_stable() {
    let dir = tempfile::tempdir().unwrap();
    let (mut v, _) = created(&dir);
    let a = v.auth_key(&pw("old")).unwrap();
    v.lock();
    let b = v.auth_key(&pw("old")).unwrap();
    assert_eq!(a.expose_secret(), b.expose_secret());
    assert!(matches!(
        v.auth_key(&pw("nope")),
        Err(VaultError::WrongPassword)
    ));
}

#[test]
fn change_password_rewraps_with_a_fresh_salt_and_keeps_records() {
    let dir = tempfile::tempdir().unwrap();
    let (mut v, _) = created(&dir);
    let h = host("web");
    v.upsert(h.clone()).unwrap();
    let before = v.account_material().unwrap();
    let old_auth = v.auth_key(&pw("old")).unwrap();

    assert!(matches!(
        v.change_password(pw("wrong"), pw("new")),
        Err(VaultError::WrongPassword)
    ));
    let changed = v.change_password(pw("old"), pw("new")).unwrap();
    assert_eq!(changed.old_auth.expose_secret(), old_auth.expose_secret());
    assert_ne!(changed.new_auth.expose_secret(), old_auth.expose_secret());
    assert_ne!(changed.material.kdf.salt, before.kdf.salt);
    assert_ne!(changed.material.secret_by_kek, before.secret_by_kek);
    // Only the KEK wrap and the KDF change.
    assert_eq!(
        changed.material.secret_by_recovery,
        before.secret_by_recovery
    );
    assert_eq!(changed.material.sealed_vault_key, before.sealed_vault_key);
    assert!(v.account_material().unwrap() == changed.material);
    assert!(v.is_unlocked(), "the device that changed it stays unlocked");

    v.lock();
    assert!(matches!(
        v.unlock(pw("old")),
        Err(VaultError::WrongPassword)
    ));
    v.unlock(pw("new")).unwrap();
    assert_eq!(v.get::<Host>(h.id).unwrap().unwrap().label, "web");
}

#[test]
fn prepare_password_change_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (mut v, _) = created(&dir);
    let before = v.account_material().unwrap();
    let change = v.prepare_password_change(&pw("old"), &pw("new")).unwrap();
    assert!(v.account_material().unwrap() == before);
    v.commit_password_change(&change, Some(7)).unwrap();
    assert!(v.account_material().unwrap() == change.material);
    assert_eq!(v.account_rev().unwrap(), Some(7));
}

#[test]
fn install_account_material_swaps_the_wrap_and_locks() {
    let dir = tempfile::tempdir().unwrap();
    let (mut a, _) = created(&dir);
    let mut b = a.clone_for_device(&dir.path().join("b.db"), 2).unwrap();
    b.unlock(pw("old")).unwrap();
    let change = a.change_password(pw("old"), pw("new")).unwrap();

    assert!(b.install_account_material(&change.material, 2).unwrap());
    assert!(!b.is_unlocked(), "an unlocked device locks itself");
    assert_eq!(b.account_rev().unwrap(), Some(2));
    // The same or an older revision changes nothing.
    assert!(!b.install_account_material(&change.material, 2).unwrap());
    // The new wrap is unproven until the new password opens it; then it
    // replaces the old one, and the old password stops working.
    b.unlock(pw("new")).unwrap();
    assert!(b.account_material().unwrap() == change.material);
    b.lock();
    assert!(matches!(
        b.unlock(pw("old")),
        Err(VaultError::WrongPassword)
    ));
    b.unlock(pw("new")).unwrap();
    // Same material at a higher revision while unlocked: nothing to swap, so
    // the session stays.
    assert!(!b.install_account_material(&change.material, 3).unwrap());
    assert!(b.is_unlocked());
    assert_eq!(b.account_rev().unwrap(), Some(3));
}

#[test]
fn bad_server_wraps_keep_the_old_password_and_recovery_key() {
    let dir = tempfile::tempdir().unwrap();
    let (mut a, recovery) = created(&dir);
    // Right vault id and account key, garbage wraps, a higher revision.
    let mut m = a.account_material().unwrap();
    m.kdf.salt = vec![9; 16];
    m.secret_by_kek = vec![0x42; 73];
    m.secret_by_recovery = vec![0x43; 73];
    a.install_account_material(&m, 9).unwrap();
    assert!(!a.is_unlocked());
    a.unlock(pw("old")).unwrap();
    a.recovery_auth(&recovery).unwrap();
    a.lock();
    assert!(matches!(
        a.unlock(pw("other")),
        Err(VaultError::WrongPassword)
    ));
    a.unlock_with_recovery(&recovery, pw("again")).unwrap();
    // The garbage never became the account's material.
    let now = a.account_material().unwrap();
    assert_ne!(now.secret_by_kek, m.secret_by_kek);
    assert_ne!(now.secret_by_recovery, m.secret_by_recovery);
}

#[test]
fn a_recovery_key_rotated_elsewhere_installs_once_it_opens() {
    let dir = tempfile::tempdir().unwrap();
    let (mut a, old) = created(&dir);
    let mut b = a.clone_for_device(&dir.path().join("b.db"), 2).unwrap();
    let (new, _) = a.rotate_recovery_key().unwrap();

    assert!(
        b.install_account_material(&a.account_material().unwrap(), 2)
            .unwrap()
    );
    // The new key opens the pending wrap and makes it the stored one; the
    // old key is then refused.
    b.recovery_auth(&new).unwrap();
    assert!(b.account_material().unwrap() == a.account_material().unwrap());
    assert!(matches!(
        b.recovery_auth(&old),
        Err(VaultError::WrongRecoveryKey)
    ));
    b.unlock_with_recovery(&new, pw("fresh")).unwrap();
}

#[test]
fn a_pending_wrap_that_does_not_open_the_vault_stays_unused() {
    let dir = tempfile::tempdir().unwrap();
    let (mut a, _) = created(&dir);
    // A wrap that opens under "new" but holds another account's secret.
    let mut m = a.account_material().unwrap();
    let kdf = core_crypto::KdfParams {
        m_kib: m.kdf.m_kib,
        t: m.kdf.t,
        p: m.kdf.p,
        salt: [8; 16],
    };
    let master = core_crypto::derive_master(&pw("new"), &kdf).unwrap();
    m.kdf.salt = kdf.salt.to_vec();
    m.secret_by_kek = core_crypto::wrap_secret(
        master.kek.expose_secret(),
        &[7; 32],
        &format!("account-secret/kek/{}", m.vault_id),
    );
    let before = a.account_material().unwrap();
    a.install_account_material(&m, 4).unwrap();
    for _ in 0..2 {
        assert!(matches!(
            a.unlock(pw("new")),
            Err(VaultError::WrongPassword)
        ));
        assert!(a.account_material().unwrap() == before, "not adopted");
    }
    a.unlock(pw("old")).unwrap();
}

#[test]
fn a_local_password_change_or_reset_drops_the_pending_wrap() {
    let dir = tempfile::tempdir().unwrap();
    let (mut a, recovery) = created(&dir);
    let mut b = a.clone_for_device(&dir.path().join("b.db"), 2).unwrap();
    let mut c = a.clone_for_device(&dir.path().join("c.db"), 3).unwrap();
    let change = a.change_password(pw("old"), pw("new")).unwrap();

    b.install_account_material(&change.material, 2).unwrap();
    b.change_password(pw("old"), pw("mine")).unwrap();
    b.lock();
    assert!(matches!(
        b.unlock(pw("new")),
        Err(VaultError::WrongPassword)
    ));
    b.unlock(pw("mine")).unwrap();

    c.install_account_material(&change.material, 2).unwrap();
    let local = c.account_material().unwrap();
    let reset = c
        .prepare_recovery_reset(&local, &recovery, &pw("reset"))
        .unwrap();
    c.commit_recovery_reset(&reset, 3).unwrap();
    c.lock();
    assert!(matches!(
        c.unlock(pw("new")),
        Err(VaultError::WrongPassword)
    ));
    c.unlock(pw("reset")).unwrap();
}

#[test]
fn pending_wraps_go_with_the_sync_account() {
    let dir = tempfile::tempdir().unwrap();
    let (mut a, _) = created(&dir);
    let mut b = a.clone_for_device(&dir.path().join("b.db"), 2).unwrap();
    let mut c = a.clone_for_device(&dir.path().join("c.db"), 3).unwrap();
    a.change_password(pw("old"), pw("new")).unwrap();
    let (rotated, _) = a.rotate_recovery_key().unwrap();
    let m = a.account_material().unwrap();

    // Turning sync on again (b) or off (c) drops what the old account sent.
    b.install_account_material(&m, 2).unwrap();
    b.commit_enable(None, 4, &[]).unwrap();
    c.install_account_material(&m, 2).unwrap();
    c.sync_clear().unwrap();
    for v in [&mut b, &mut c] {
        assert!(matches!(
            v.unlock(pw("new")),
            Err(VaultError::WrongPassword)
        ));
        assert!(matches!(
            v.recovery_auth(&rotated),
            Err(VaultError::WrongRecoveryKey)
        ));
        v.unlock(pw("old")).unwrap();
    }
}

#[test]
fn rejoin_keeps_the_local_recovery_wrap_until_the_servers_opens() {
    let dir = tempfile::tempdir().unwrap();
    let (mut a, recovery) = created(&dir);
    let mut b = a.clone_for_device(&dir.path().join("b.db"), 2).unwrap();
    let mut m = a.account_material().unwrap();
    m.secret_by_recovery = vec![0x43; 73];
    a.rejoin(&m, pw("old"), 1, &[]).unwrap();
    a.recovery_auth(&recovery).unwrap();
    assert_ne!(
        a.account_material().unwrap().secret_by_recovery,
        m.secret_by_recovery
    );

    // A key rotated elsewhere is adopted once it opens.
    let (rotated, _) = a.rotate_recovery_key().unwrap();
    b.rejoin(&a.account_material().unwrap(), pw("old"), 2, &[])
        .unwrap();
    b.recovery_auth(&rotated).unwrap();
    assert!(matches!(
        b.recovery_auth(&recovery),
        Err(VaultError::WrongRecoveryKey)
    ));
}

#[test]
fn install_account_material_rejects_hostile_or_foreign_material() {
    let dir = tempfile::tempdir().unwrap();
    let (mut a, _) = created(&dir);
    let good = a.account_material().unwrap();
    type Breaker = Box<dyn Fn(&mut core_proto::AccountMaterial)>;
    let cases: Vec<(&str, Breaker)> = vec![
        ("m_kib", Box::new(|m| m.kdf.m_kib = 1024 * 1024 + 1)),
        ("t zero", Box::new(|m| m.kdf.t = 0)),
        ("t big", Box::new(|m| m.kdf.t = 17)),
        ("p zero", Box::new(|m| m.kdf.p = 0)),
        ("p big", Box::new(|m| m.kdf.p = 65)),
        ("salt", Box::new(|m| m.kdf.salt = vec![1; 15])),
        ("kek", Box::new(|m| m.secret_by_kek = vec![1; 72])),
        ("recovery", Box::new(|m| m.secret_by_recovery = vec![1; 74])),
        ("sealed", Box::new(|m| m.sealed_vault_key = vec![1; 80])),
        ("public", Box::new(|m| m.account_public = vec![1; 31])),
        (
            "vault id",
            Box::new(|m| m.vault_id = Id::now_v7().to_string()),
        ),
        ("public key", Box::new(|m| m.account_public = vec![9; 32])),
    ];
    for (what, break_it) in cases {
        let mut m = good.clone();
        break_it(&mut m);
        let err = a.install_account_material(&m, 5).unwrap_err();
        assert!(matches!(err, VaultError::BadMaterial(_)), "{what}: {err:?}");
        assert!(a.is_unlocked(), "{what}: nothing changed");
        assert!(a.account_material().unwrap() == good, "{what}");
    }
}

#[test]
fn join_creates_a_vault_from_material_and_checks_the_password() {
    let dir = tempfile::tempdir().unwrap();
    let (mut a, _) = created(&dir);
    let h = host("db");
    a.upsert(h).unwrap();
    let m = a.account_material().unwrap();

    let mut b = open(&dir, "b.db");
    assert!(matches!(
        b.join(&m, pw("wrong"), 9),
        Err(VaultError::WrongPassword)
    ));
    assert!(!b.is_initialized(), "a failed join writes nothing");
    b.join(&m, pw("old"), 9).unwrap();
    assert!(b.is_unlocked());
    assert_eq!(b.vault_id(), a.vault_id());
    assert_eq!(b.node().unwrap(), 9);
    assert!(b.account_material().unwrap() == m);
    assert!(matches!(
        b.join(&m, pw("old"), 9),
        Err(VaultError::AlreadyInitialized)
    ));
    b.lock();
    b.unlock(pw("old")).unwrap();
}

#[test]
fn replace_with_join_keeps_the_old_file_as_a_backup() {
    let dir = tempfile::tempdir().unwrap();
    let (a, _) = created(&dir);
    let m = a.account_material().unwrap();

    let mut other = open(&dir, "vault.db");
    other.create(pw("mine")).unwrap();
    let mine = host("local only");
    other.upsert(mine.clone()).unwrap();
    other.sync_put("url", b"https://old").unwrap();

    assert!(matches!(
        other.replace_with_join(&m, pw("wrong"), 3),
        Err(VaultError::WrongPassword)
    ));
    assert_eq!(other.list::<Host>().unwrap().len(), 1, "untouched");

    let backup = other.replace_with_join(&m, pw("old"), 3).unwrap();
    let name = backup.file_name().unwrap().to_str().unwrap().to_owned();
    assert!(name.starts_with("vault.db.before-join-20"), "{name}");
    assert_eq!(other.vault_id(), a.vault_id());
    assert!(other.list::<Host>().unwrap().is_empty());
    assert_eq!(other.sync_get("url").unwrap(), None);
    assert_eq!(other.node().unwrap(), 3);

    // A second replace the same day gets its own file.
    let again = other.replace_with_join(&m, pw("old"), 3).unwrap();
    assert_ne!(again, backup);

    let mut old =
        VaultService::open_for_tests(&backup, Box::new(MemoryKeyStore::default())).unwrap();
    old.unlock(pw("mine")).unwrap();
    assert_eq!(old.list::<Host>().unwrap()[0].label, "local only");
}

#[test]
fn recovery_auth_checks_the_key() {
    let dir = tempfile::tempdir().unwrap();
    let (v, recovery) = created(&dir);
    let a = v.recovery_auth(&recovery).unwrap();
    let b = v.recovery_auth(&recovery).unwrap();
    assert_eq!(a.expose_secret(), b.expose_secret());
    assert!(matches!(
        v.recovery_auth("not a key"),
        Err(VaultError::WrongRecoveryKey)
    ));
    let (_, other) = core_crypto::new_recovery_key();
    assert!(matches!(
        v.recovery_auth(&other),
        Err(VaultError::WrongRecoveryKey)
    ));
}

#[test]
fn rotate_recovery_key_replaces_the_recovery_wrap() {
    let dir = tempfile::tempdir().unwrap();
    let (mut v, old) = created(&dir);
    let before = v.account_material().unwrap();
    let (text, auth) = v.rotate_recovery_key().unwrap();
    assert_ne!(
        v.account_material().unwrap().secret_by_recovery,
        before.secret_by_recovery
    );
    assert!(matches!(
        v.recovery_auth(&old),
        Err(VaultError::WrongRecoveryKey)
    ));
    assert_eq!(
        v.recovery_auth(&text).unwrap().expose_secret(),
        auth.expose_secret()
    );
    v.lock();
    v.unlock_with_recovery(&text, pw("again")).unwrap();
}

#[test]
fn recovery_reset_from_material_sets_a_new_password() {
    let dir = tempfile::tempdir().unwrap();
    let (mut a, recovery) = created(&dir);
    let m = a.account_material().unwrap();
    let kdf = core_crypto::KdfParams {
        m_kib: m.kdf.m_kib,
        t: m.kdf.t,
        p: m.kdf.p,
        salt: [5; 16],
    };
    assert!(matches!(
        recovery_reset_for(&m, "garbage", &pw("x"), kdf.clone()),
        Err(VaultError::WrongRecoveryKey)
    ));
    let reset = recovery_reset_for(&m, &recovery, &pw("fresh"), kdf).unwrap();
    assert_eq!(reset.kdf.salt, vec![5; 16]);
    assert_eq!(
        reset.recovery_auth.expose_secret(),
        a.recovery_auth(&recovery).unwrap().expose_secret()
    );

    // Locally (sync on): commit installs the new wrap and unlocks.
    a.lock();
    let local = a
        .prepare_recovery_reset(&m, &recovery, &pw("fresh2"))
        .unwrap();
    a.commit_recovery_reset(&local, 4).unwrap();
    assert!(a.is_unlocked());
    assert_eq!(a.account_rev().unwrap(), Some(4));
    a.lock();
    assert!(matches!(
        a.unlock(pw("old")),
        Err(VaultError::WrongPassword)
    ));
    a.unlock(pw("fresh2")).unwrap();
}

#[test]
fn set_node_changes_the_clock_node() {
    let dir = tempfile::tempdir().unwrap();
    let (mut v, _) = created(&dir);
    v.set_node(42).unwrap();
    assert_eq!(v.node().unwrap(), 42);
    v.upsert(host("x")).unwrap();
    let row = v.outbox(10).unwrap().pop().unwrap();
    assert_eq!(row.hlc.node, 42);
    v.lock();
    v.unlock(pw("old")).unwrap();
    assert_eq!(v.node().unwrap(), 42);
}

/// During a full resync every rev is null; a clean local row that is newer
/// than the server's copy (the server was restored from an older backup) is
/// kept and pushed, not overwritten.
#[test]
fn resync_keeps_a_newer_clean_row_and_takes_an_older_one() {
    let dir = tempfile::tempdir().unwrap();
    let (mut v, _) = created(&dir);
    let (newer, older) = (host("newer local"), host("older local"));
    v.upsert(newer.clone()).unwrap();
    v.upsert(older.clone()).unwrap();
    let rows = v.outbox(10).unwrap();
    for r in &rows {
        v.ack_pushed(r.id, r.hlc, 3).unwrap();
    }
    let row_of = |id: Id| rows.iter().find(|r| r.id == id).unwrap().clone();
    let (n, o) = (row_of(newer.id), row_of(older.id));

    v.reset_revs().unwrap();
    let remote = |r: &core_store::RecordRow, seq: i64, hlc: Hlc| InboundRow {
        seq,
        id: r.id,
        kind: Kind::Host.as_str().to_owned(),
        key_version: r.key_version,
        hlc,
        rev: 1,
        deleted: false,
        envelope: r.envelope.clone(),
    };
    let before = Hlc {
        wall_ms: n.hlc.wall_ms - 1,
        ..n.hlc
    };
    let after = Hlc {
        wall_ms: o.hlc.wall_ms + 60_000,
        ..o.hlc
    };
    // Same envelope bytes are fine: the rule only looks at the HLC.
    v.queue_inbound(vec![remote(&n, 1, before), remote(&o, 2, after)])
        .unwrap();
    let report = v.apply_inbound().unwrap();
    assert_eq!(report.conflicts, 0);

    let out = v.outbox(10).unwrap();
    assert_eq!(out.len(), 1, "only the newer local row goes up");
    assert_eq!(out[0].id, newer.id);
    assert_eq!(out[0].rev, Some(1), "based on the server's revision");
    assert_eq!(out[0].hlc, n.hlc);
    v.mark_unsynced_dirty().unwrap();
    assert_eq!(v.outbox(10).unwrap().len(), 1);
}

/// Makes every later write to `sync_state` of key `k` fail, through a
/// second connection to the vault file.
fn fail_writes_of(dir: &TempDir, k: &str) {
    let conn = rusqlite::Connection::open(dir.path().join("a.db")).unwrap();
    conn.execute_batch(&format!(
        "CREATE TRIGGER fail_put BEFORE INSERT ON sync_state WHEN NEW.k = '{k}' \
             BEGIN SELECT RAISE(ABORT, 'injected'); END; \
         CREATE TRIGGER fail_delete BEFORE DELETE ON sync_state WHEN OLD.k = '{k}' \
             BEGIN SELECT RAISE(ABORT, 'injected'); END;"
    ))
    .unwrap();
}

fn allow_writes(dir: &TempDir) {
    let conn = rusqlite::Connection::open(dir.path().join("a.db")).unwrap();
    conn.execute_batch("DROP TRIGGER fail_put; DROP TRIGGER fail_delete;")
        .unwrap();
}

/// A vault with one synced (clean, rev 3) host and one queued inbound row.
fn synced_with_queue(dir: &TempDir) -> VaultService {
    let (mut v, _) = created(dir);
    v.upsert(host("synced")).unwrap();
    let row = v.outbox(1).unwrap().pop().unwrap();
    v.ack_pushed(row.id, row.hlc, 3).unwrap();
    v.queue_inbound(vec![InboundRow {
        seq: 9,
        id: Id::now_v7(),
        kind: "host".into(),
        key_version: 1,
        hlc: row.hlc,
        rev: 1,
        deleted: false,
        envelope: vec![1],
    }])
    .unwrap();
    v
}

#[test]
fn begin_resync_is_one_transaction() {
    let dir = tempfile::tempdir().unwrap();
    let mut v = synced_with_queue(&dir);
    v.sync_put("resync_epoch", b"old").unwrap();
    fail_writes_of(&dir, "resync");

    let set: &[(&str, &[u8])] = &[("cursor", b"0"), ("resync", b"1")];
    assert!(v.begin_resync(set, &["resync_epoch"]).is_err());
    // Nothing happened: revs kept (the clean row does not turn dirty), the
    // queue and the other keys untouched.
    v.mark_unsynced_dirty().unwrap();
    assert_eq!(v.outbox_len().unwrap(), 0, "revs kept");
    assert_eq!(v.inbound_len().unwrap(), 1, "queue kept");
    assert_eq!(v.sync_get("cursor").unwrap(), None);
    assert_eq!(
        v.sync_get("resync_epoch").unwrap().as_deref(),
        Some(&b"old"[..])
    );

    allow_writes(&dir);
    v.begin_resync(set, &["resync_epoch"]).unwrap();
    assert_eq!(v.inbound_len().unwrap(), 0);
    assert_eq!(v.sync_get("resync").unwrap().as_deref(), Some(&b"1"[..]));
    assert_eq!(v.sync_get("resync_epoch").unwrap(), None);
    v.mark_unsynced_dirty().unwrap();
    assert_eq!(v.outbox_len().unwrap(), 1, "revs forgotten");
}

#[test]
fn finish_resync_is_one_transaction() {
    let dir = tempfile::tempdir().unwrap();
    let mut v = synced_with_queue(&dir);
    v.begin_resync(&[("resync", b"1")], &[]).unwrap();
    fail_writes_of(&dir, "resync");

    assert!(v.finish_resync(&[("epoch", b"new")], &["resync"]).is_err());
    assert_eq!(v.outbox_len().unwrap(), 0, "nothing marked dirty");
    assert_eq!(v.sync_get("epoch").unwrap(), None);

    allow_writes(&dir);
    v.finish_resync(&[("epoch", b"new")], &["resync"]).unwrap();
    assert_eq!(v.outbox_len().unwrap(), 1);
    assert_eq!(v.sync_get("epoch").unwrap().as_deref(), Some(&b"new"[..]));
    assert_eq!(v.sync_get("resync").unwrap(), None);
}

#[test]
fn rejoin_keeps_records_and_installs_the_servers_material() {
    let dir = tempfile::tempdir().unwrap();
    let (mut a, _) = created(&dir);
    let kept = host("kept");
    a.upsert(kept.clone()).unwrap();
    a.sync_put("url", b"https://old").unwrap();
    // Elsewhere the password changed: the server's material is newer.
    let mut elsewhere = a.clone_for_device(&dir.path().join("b.db"), 9).unwrap();
    elsewhere.unlock(pw("old")).unwrap();
    let changed = elsewhere.change_password(pw("old"), pw("new")).unwrap();

    // Another vault's material is refused; so is a wrong password.
    let mut other = open(&dir, "other.db");
    other.create(pw("old")).unwrap();
    let foreign = other.account_material().unwrap();
    assert!(matches!(
        a.rejoin(&foreign, pw("old"), 4, &[]),
        Err(VaultError::BadMaterial(_))
    ));
    assert!(matches!(
        a.rejoin(&changed.material, pw("old"), 4, &[]),
        Err(VaultError::WrongPassword)
    ));
    assert_eq!(
        a.sync_get("url").unwrap().as_deref(),
        Some(&b"https://old"[..])
    );

    a.rejoin(&changed.material, pw("new"), 4, &[("url", b"https://new")])
        .unwrap();
    assert!(a.is_unlocked());
    assert_eq!(a.node().unwrap(), 4);
    assert_eq!(a.list::<Host>().unwrap(), vec![kept]);
    assert_eq!(
        a.sync_get("url").unwrap().as_deref(),
        Some(&b"https://new"[..])
    );
    a.lock();
    assert!(matches!(
        a.unlock(pw("old")),
        Err(VaultError::WrongPassword)
    ));
    a.unlock(pw("new")).unwrap();
}
