use core_crypto::random_key;
use core_model::{ConnSettings, Hlc, Host, Id, Kind};
use core_store::{RecordRow, Store, StoreError, open_row, seal_row};
use rusqlite::Connection;
use secrecy::ExposeSecret;
use tempfile::TempDir;

const CANARY: &str = "CANARY-7f3a";

fn new_key() -> [u8; 32] {
    *random_key().expose_secret()
}

fn hlc(n: u64) -> Hlc {
    Hlc {
        wall_ms: n,
        counter: 0,
        node: 1,
    }
}

fn host(label: &str) -> Host {
    Host {
        id: Id::now_v7(),
        group_id: None,
        label: label.to_owned(),
        address: "10.0.0.1".to_owned(),
        tag_ids: vec![],
        settings: ConnSettings::default(),
        forward_rule_ids: vec![],
        extra: Default::default(),
    }
}

fn sealed_host(key: &[u8; 32], vault: Id, label: &str, n: u64) -> (Host, RecordRow) {
    let h = host(label);
    let json = serde_json::to_vec(&h).unwrap();
    let row = seal_row(key, vault, Kind::Host, h.id, 1, hlc(n), &json);
    (h, row)
}

fn temp_db() -> (TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("vault.sqlite");
    (dir, path)
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn put_get_list_tombstone() {
    let (_dir, path) = temp_db();
    let store = Store::open(&path).unwrap();
    let key = new_key();
    let vault = Id::now_v7();
    let (h1, r1) = sealed_host(&key, vault, "one", 1);
    let (h2, r2) = sealed_host(&key, vault, "two", 2);
    // A row in another vault and one of another kind must not be listed.
    let (_, other_vault) = sealed_host(&key, Id::now_v7(), "elsewhere", 3);
    let tag = seal_row(&key, vault, Kind::Tag, Id::now_v7(), 1, hlc(4), b"{}");
    for r in [&r1, &r2, &other_vault, &tag] {
        store.put(r).unwrap();
    }

    assert_eq!(store.get(&h1.id).unwrap().as_ref(), Some(&r1));
    assert_eq!(store.get(&Id::now_v7()).unwrap(), None);
    let listed = store.list(&vault, Kind::Host).unwrap();
    assert_eq!(listed, vec![r1.clone(), r2.clone()]);
    let opened = open_row(&key, &listed[1]).unwrap();
    assert_eq!(serde_json::from_slice::<Host>(&opened).unwrap(), h2);

    // Upsert replaces in place.
    let mut r1b = r1.clone();
    r1b.hlc = hlc(10);
    store.put(&r1b).unwrap();
    assert_eq!(store.get(&h1.id).unwrap().unwrap().hlc, hlc(10));
    assert_eq!(store.list(&vault, Kind::Host).unwrap().len(), 2);

    // Tombstone: hidden from list, kept by get with the envelope cleared.
    store.tombstone(&h1.id, hlc(20)).unwrap();
    let ids: Vec<Id> = store
        .list(&vault, Kind::Host)
        .unwrap()
        .iter()
        .map(|r| r.id)
        .collect();
    assert_eq!(ids, vec![h2.id]);
    let dead = store.get(&h1.id).unwrap().unwrap();
    assert!(dead.deleted);
    assert!(dead.envelope.is_empty());
    assert_eq!(dead.hlc, hlc(20));
    assert!(matches!(
        open_row(&key, &dead),
        Err(StoreError::Deleted { id }) if id == h1.id
    ));

    let missing = Id::now_v7();
    assert!(matches!(
        store.tombstone(&missing, hlc(30)),
        Err(StoreError::NotFound { id }) if id == missing
    ));
}

#[test]
fn rows_are_marked_dirty_on_write() {
    let (_dir, path) = temp_db();
    let store = Store::open(&path).unwrap();
    let key = new_key();
    let (h, row) = sealed_host(&key, Id::now_v7(), "d", 1);
    store.put(&row).unwrap();
    store.tombstone(&h.id, hlc(2)).unwrap();
    let raw = Connection::open(&path).unwrap();
    raw.execute("UPDATE records SET dirty = 0", []).unwrap();
    store.put(&row).unwrap();
    let dirty: i64 = raw
        .query_row("SELECT dirty FROM records", [], |r| r.get(0))
        .unwrap();
    assert_eq!(dirty, 1);
    raw.execute("UPDATE records SET dirty = 0", []).unwrap();
    store.tombstone(&h.id, hlc(3)).unwrap();
    let dirty: i64 = raw
        .query_row("SELECT dirty FROM records", [], |r| r.get(0))
        .unwrap();
    assert_eq!(dirty, 1);
}

#[test]
fn canary_never_on_disk() {
    let (_dir, path) = temp_db();
    let wal = path.with_extension("sqlite-wal");
    let store = Store::open(&path).unwrap();
    let key = new_key();
    let vault = Id::now_v7();
    for i in 0..100 {
        let (_, row) = sealed_host(&key, vault, CANARY, i);
        store.put(&row).unwrap();
    }
    store.account_put("wrapped", b"opaque").unwrap();

    // Sanity: the canary is in the JSON we sealed, and the matcher sees it.
    let probe = serde_json::to_vec(&host(CANARY)).unwrap();
    assert!(contains(&probe, CANARY.as_bytes()));

    // The data is still in the WAL here; scan it before checkpointing it away.
    let wal_bytes = std::fs::read(&wal).unwrap();
    assert!(wal_bytes.len() > 4096, "expected pending WAL content");
    assert!(!contains(&wal_bytes, CANARY.as_bytes()));

    let raw = Connection::open(&path).unwrap();
    raw.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
        .unwrap();
    drop(raw);
    let db_bytes = std::fs::read(&path).unwrap();
    assert!(db_bytes.len() > 4096);
    assert!(!contains(&db_bytes, CANARY.as_bytes()));
    assert!(!contains(&std::fs::read(&wal).unwrap(), CANARY.as_bytes()));

    // And the rows really do decrypt to the canary-bearing record.
    let rows = store.list(&vault, Kind::Host).unwrap();
    assert_eq!(rows.len(), 100);
    let opened = open_row(&key, &rows[0]).unwrap();
    assert!(contains(&opened, CANARY.as_bytes()));
}

#[test]
fn unicode_labels_roundtrip() {
    let (_dir, path) = temp_db();
    let store = Store::open(&path).unwrap();
    let key = new_key();
    let vault = Id::now_v7();
    let label = "生产 🚀 'db' \"x\"";
    let (h, row) = sealed_host(&key, vault, label, 1);
    store.put(&row).unwrap();
    drop(store);

    // Reopen so the round trip goes through the file, not a cache.
    let store = Store::open(&path).unwrap();
    let got = store.get(&h.id).unwrap().unwrap();
    let back: Host = serde_json::from_slice(&open_row(&key, &got).unwrap()).unwrap();
    assert_eq!(back.label, label);
    assert_eq!(back, h);
}

#[test]
fn corrupted_envelope_reports_record_id() {
    let (_dir, path) = temp_db();
    let store = Store::open(&path).unwrap();
    let key = new_key();
    let vault = Id::now_v7();
    let rows: Vec<RecordRow> = (0..3)
        .map(|i| sealed_host(&key, vault, &format!("h{i}"), i).1)
        .collect();
    for r in &rows {
        store.put(r).unwrap();
    }

    let victim = rows[1].id;
    let raw = Connection::open(&path).unwrap();
    let mut env: Vec<u8> = raw
        .query_row(
            "SELECT envelope FROM records WHERE id = ?1",
            [victim.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    let mid = env.len() / 2;
    env[mid] ^= 0x01;
    raw.execute(
        "UPDATE records SET envelope = ?1 WHERE id = ?2",
        rusqlite::params![env, victim.to_string()],
    )
    .unwrap();

    let listed = store.list(&vault, Kind::Host).unwrap();
    assert_eq!(listed.len(), 3);
    for row in &listed {
        let res = open_row(&key, row);
        if row.id == victim {
            let err = res.unwrap_err();
            assert!(matches!(err, StoreError::Corrupt { id } if id == victim));
            assert!(err.to_string().contains(&victim.to_string()), "{err}");
        } else {
            assert!(res.is_ok(), "other rows must still open");
        }
    }
}

#[test]
fn envelope_moved_between_rows_is_corrupt() {
    // The AAD binds id/vault/kind/key_version, so a swapped envelope must not open.
    let key = new_key();
    let vault = Id::now_v7();
    let (_, a) = sealed_host(&key, vault, "a", 1);
    let (_, b) = sealed_host(&key, vault, "b", 2);
    let swapped = RecordRow {
        envelope: b.envelope.clone(),
        ..a.clone()
    };
    assert!(matches!(
        open_row(&key, &swapped),
        Err(StoreError::Corrupt { id }) if id == a.id
    ));
    let wrong_version = RecordRow {
        key_version: 2,
        ..a.clone()
    };
    assert!(open_row(&key, &wrong_version).is_err());
    assert!(open_row(&new_key(), &a).is_err());
}

#[test]
fn account_roundtrip_and_overwrite() {
    let (_dir, path) = temp_db();
    let store = Store::open(&path).unwrap();
    assert_eq!(store.account_get("kdf").unwrap(), None);
    store.account_put("kdf", b"one").unwrap();
    store.account_put("kdf", b"two").unwrap();
    drop(store);
    let store = Store::open(&path).unwrap();
    assert_eq!(
        store.account_get("kdf").unwrap().as_deref(),
        Some(&b"two"[..])
    );
}

#[test]
fn open_uses_wal_and_sets_schema_version() {
    let (_dir, path) = temp_db();
    drop(Store::open(&path).unwrap());
    let raw = Connection::open(&path).unwrap();
    let mode: String = raw
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .unwrap();
    let version: i64 = raw
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    assert_eq!(version, 2);
}

#[cfg(unix)]
#[test]
fn vault_files_and_copies_are_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    let set = |p: &std::path::Path, m: u32| {
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(m)).unwrap()
    };
    let (dir, path) = temp_db();
    let wal = path.with_extension("sqlite-wal");
    let shm = path.with_extension("sqlite-shm");
    let key = new_key();
    let store = Store::open(&path).unwrap();
    store.put(&sealed_host(&key, Id::now_v7(), "x", 1).1).unwrap();
    for p in [&path, &wal, &shm] {
        assert_eq!(mode(p), 0o600, "{}", p.display());
    }
    let copy = dir.path().join("copy.sqlite");
    store.copy_to(&copy).unwrap();
    assert_eq!(mode(&copy), 0o600);
    drop(store);

    // A vault written by an older version, readable by everyone, is tightened on open.
    set(&path, 0o644);
    let store = Store::open(&path).unwrap();
    store.put(&sealed_host(&key, Id::now_v7(), "y", 1).1).unwrap();
    for p in [&path, &wal, &shm] {
        assert_eq!(mode(p), 0o600, "{}", p.display());
    }
}

#[test]
fn reopening_up_to_date_db_does_not_write() {
    let (_dir, path) = temp_db();
    let key = new_key();
    let (_, row) = sealed_host(&key, Id::now_v7(), "x", 1);
    let store = Store::open(&path).unwrap();
    store.put(&row).unwrap();
    drop(store);

    // Fully checkpoint, then compare bytes (db + wal) across a reopen.
    let raw = Connection::open(&path).unwrap();
    raw.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
        .unwrap();
    drop(raw);
    let wal = path.with_extension("sqlite-wal");
    let snapshot = |p: &std::path::Path| std::fs::read(p).unwrap_or_default();
    let (db_before, wal_before) = (snapshot(&path), snapshot(&wal));

    drop(Store::open(&path).unwrap());
    assert_eq!(snapshot(&path), db_before);
    assert_eq!(snapshot(&wal), wal_before);
}

#[test]
fn newer_schema_version_is_rejected() {
    let (_dir, path) = temp_db();
    drop(Store::open(&path).unwrap());
    Connection::open(&path)
        .unwrap()
        .execute_batch("PRAGMA user_version = 99;")
        .unwrap();
    assert!(matches!(
        Store::open(&path),
        Err(StoreError::UnsupportedSchema { found: 99, .. })
    ));
}

#[test]
fn open_garbage_file_errs_without_modifying_it() {
    let dir = tempfile::tempdir().unwrap();
    let cases: [(&str, Vec<u8>); 3] = [
        (
            "garbage",
            b"this is definitely not a sqlite database".repeat(100),
        ),
        ("tiny", b"nope".to_vec()),
        ("zeros-with-header-junk", {
            let mut v = b"SQLite format 3\0".to_vec();
            v.extend(std::iter::repeat_n(0xAB, 4096));
            v
        }),
    ];
    for (name, bytes) in cases {
        let path = dir.path().join(format!("{name}.sqlite"));
        std::fs::write(&path, &bytes).unwrap();
        let res = Store::open(&path);
        assert!(res.is_err(), "{name} should not open");
        assert_eq!(std::fs::read(&path).unwrap(), bytes, "{name} was modified");
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .filter(|n| n.starts_with(name) && n != &format!("{name}.sqlite"))
            .collect();
        assert!(leftovers.is_empty(), "{name}: stray files {leftovers:?}");
    }
}

#[test]
fn open_truncated_db_errs_without_modifying_it() {
    let (dir, path) = temp_db();
    let key = new_key();
    let vault = Id::now_v7();
    let store = Store::open(&path).unwrap();
    for i in 0..50 {
        store.put(&sealed_host(&key, vault, "t", i).1).unwrap();
    }
    drop(store);
    // Merge the WAL so the main file is the whole database, then chop it.
    let raw = Connection::open(&path).unwrap();
    raw.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
        .unwrap();
    raw.query_row("PRAGMA journal_mode = DELETE", [], |_| Ok(()))
        .unwrap();
    drop(raw);
    let full = std::fs::read(&path).unwrap();
    let cut = dir.path().join("cut.sqlite");
    let truncated = &full[..full.len() / 2];
    std::fs::write(&cut, truncated).unwrap();

    // Half the pages are missing: open must fail cleanly (no panic) and leave the file alone.
    assert!(Store::open(&cut).is_err());
    assert_eq!(std::fs::read(&cut).unwrap(), truncated);
}

/// An existing 0-byte file is not a new database: SQLite would write a
/// schema into it and the app would offer "Create vault" over a truncated
/// vault. It is refused with a distinct error and left at 0 bytes.
#[test]
fn open_empty_file_errs_without_modifying_it() {
    let (dir, path) = temp_db();
    std::fs::write(&path, b"").unwrap();
    assert!(matches!(Store::open(&path), Err(StoreError::Empty)));
    assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
    let names: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names.len(), 1, "stray files {names:?}");

    // A missing file is still created.
    let fresh = dir.path().join("fresh.sqlite");
    Store::open(&fresh).unwrap();
    assert!(std::fs::metadata(&fresh).unwrap().len() > 0);
}

#[test]
fn open_db_with_damaged_pages_errs_without_modifying_it() {
    // A real WAL-mode vault whose header and schema survive but whose later
    // b-tree pages are garbage (same length, so no truncation signal). The
    // schema parses, so without an integrity check `open` succeeds and only
    // the first query fails.
    let (dir, path) = temp_db();
    let key = new_key();
    let vault = Id::now_v7();
    let store = Store::open(&path).unwrap();
    for i in 0..200 {
        store.put(&sealed_host(&key, vault, "t", i).1).unwrap();
    }
    drop(store); // the last close checkpoints and removes the WAL
    let full = std::fs::read(&path).unwrap();
    let page = 4096;
    assert!(full.len() > 16 * page);
    let mut damaged = full.clone();
    damaged[8 * page..].fill(0xAB);
    let bad = dir.path().join("damaged.sqlite");
    std::fs::write(&bad, &damaged).unwrap();

    assert!(
        Store::open(&bad).is_err(),
        "a file with damaged pages must not open"
    );
    assert_eq!(std::fs::read(&bad).unwrap(), damaged);
}

#[test]
fn atomically_commits_on_ok_and_rolls_back_on_err() {
    let (_dir, path) = temp_db();
    let store = Store::open(&path).unwrap();
    let key = new_key();
    let vault = Id::now_v7();
    let (_, a) = sealed_host(&key, vault, "a", 1);
    let (_, b) = sealed_host(&key, vault, "b", 2);

    let res: Result<(), StoreError> = store.atomically(|| {
        store.put(&a)?;
        store.account_put("k", b"v")?;
        Err(StoreError::NotFound { id: a.id })
    });
    assert!(matches!(res, Err(StoreError::NotFound { .. })));
    assert_eq!(store.get(&a.id).unwrap(), None);
    assert_eq!(store.account_get("k").unwrap(), None);

    store
        .atomically(|| -> Result<(), StoreError> {
            store.put(&a)?;
            store.put(&b)
        })
        .unwrap();
    assert_eq!(store.list(&vault, Kind::Host).unwrap(), vec![a, b]);
}

#[test]
fn account_delete_removes_key() {
    let (_dir, path) = temp_db();
    let store = Store::open(&path).unwrap();
    store.account_put("k", b"v").unwrap();
    store.account_delete("k").unwrap();
    assert_eq!(store.account_get("k").unwrap(), None);
    // Deleting a missing key is not an error.
    store.account_delete("k").unwrap();
}
