//! Schema migration tests, driven by a committed schema-v1 vault file.

use std::path::{Path, PathBuf};

use core_model::{Hlc, Id, Kind};
use core_store::{Store, open_row, seal_row};
use rusqlite::Connection;

/// Fixed key the golden v1 vault was sealed with.
const GOLDEN_KEY: [u8; 32] = [7u8; 32];
const GOLDEN_VAULT_ID: &str = "0198a1b2-0000-7000-8000-0000000000a1";
const GOLDEN_FILE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden/vault-v1.db");

/// (id, kind, hlc wall_ms, plaintext or None for a tombstone, rev)
type Fixture = (&'static str, Kind, u64, Option<&'static [u8]>, Option<i64>);
const GOLDEN_RECORDS: &[Fixture] = &[
    (
        "0198a1b2-0000-7000-8000-0000000000b1",
        Kind::Host,
        100,
        Some(br#"{"label":"golden-one"}"#),
        None,
    ),
    (
        "0198a1b2-0000-7000-8000-0000000000b2",
        Kind::Host,
        200,
        Some(br#"{"label":"golden-two"}"#),
        Some(5),
    ),
    (
        "0198a1b2-0000-7000-8000-0000000000b3",
        Kind::Tag,
        300,
        Some(br#"{"name":"golden-tag"}"#),
        None,
    ),
    (
        "0198a1b2-0000-7000-8000-0000000000b4",
        Kind::Host,
        400,
        None,
        None,
    ),
];
/// `account` rows in the golden file.
const GOLDEN_ACCOUNT: &[(&str, &[u8])] = &[("kdf", b"golden-kdf"), ("wrapped", b"golden-wrapped")];

fn hlc(n: u64) -> Hlc {
    Hlc {
        wall_ms: n,
        counter: 0,
        node: 1,
    }
}

fn id(s: &str) -> Id {
    Id::parse_str(s).unwrap()
}

/// Copies the golden file into a fresh temp dir and returns the copy's path.
fn golden_copy() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("vault.sqlite");
    std::fs::copy(GOLDEN_FILE, &path).unwrap();
    (dir, path)
}

fn user_version(path: &Path) -> i64 {
    Connection::open(path)
        .unwrap()
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap()
}

fn table_exists(path: &Path, name: &str) -> bool {
    Connection::open(path)
        .unwrap()
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [name],
            |r| r.get::<_, i64>(0),
        )
        .unwrap()
        == 1
}

/// Generators are no-ops unless `TETHER_REGEN_GOLDEN=1`, so `--include-ignored`
/// cannot silently rewrite the committed fixture.
fn regen_enabled() -> bool {
    std::env::var("TETHER_REGEN_GOLDEN").as_deref() == Ok("1")
}

/// One-off fixture generator. It must be run against schema-v1 code (the
/// fixture exists to prove a Phase 1 file upgrades), so it refuses to write
/// anything else:
/// `TETHER_REGEN_GOLDEN=1 cargo test -p core-store --test migrate -- --ignored --exact generate_golden_v1`
#[test]
#[ignore = "regenerates tests/golden/vault-v1.db; only valid on schema-v1 code"]
fn generate_golden_v1() {
    if !regen_enabled() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("vault.sqlite");
    let vault = id(GOLDEN_VAULT_ID);
    {
        let store = Store::open(&path).unwrap();
        for &(rid, kind, wall, plain, rev) in GOLDEN_RECORDS {
            let rid = id(rid);
            let mut row = seal_row(
                &GOLDEN_KEY,
                vault,
                kind,
                rid,
                1,
                hlc(wall),
                plain.unwrap_or(b""),
            );
            row.rev = rev;
            store.put(&row).unwrap();
            if plain.is_none() {
                store.tombstone(&rid, hlc(wall)).unwrap();
            }
        }
        for (k, v) in GOLDEN_ACCOUNT {
            store.account_put(k, v).unwrap();
        }
    }
    assert_eq!(
        user_version(&path),
        1,
        "run this generator on schema-v1 code"
    );
    // Fold the WAL into the main file so no sidecars are needed.
    let raw = Connection::open(&path).unwrap();
    raw.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
        .unwrap();
    drop(raw);
    std::fs::copy(&path, GOLDEN_FILE).unwrap();
}

#[test]
fn golden_fixture_is_schema_v1_without_sidecars() {
    // Checked first, and the version is read from a copy: opening a WAL-mode
    // file in place would itself create the sidecars.
    for ext in ["-wal", "-shm"] {
        assert!(!Path::new(&format!("{GOLDEN_FILE}{ext}")).exists());
    }
    let (_dir, path) = golden_copy();
    assert_eq!(user_version(&path), 1);
}

#[test]
fn v1_file_migrates_to_v2_keeping_records() {
    let (_dir, path) = golden_copy();
    assert!(!table_exists(&path, "sync_state"));
    let store = Store::open(&path).unwrap();
    assert_eq!(user_version(&path), 2);
    assert!(table_exists(&path, "sync_state"));
    assert!(table_exists(&path, "inbound"));

    let vault = id(GOLDEN_VAULT_ID);
    for &(rid, kind, wall, plain, rev) in GOLDEN_RECORDS {
        let row = store.get(&id(rid)).unwrap().expect("record survived");
        assert_eq!(
            (row.vault_id, row.kind, row.hlc, row.rev),
            (vault, kind, hlc(wall), rev)
        );
        match plain {
            Some(p) => {
                assert!(!row.deleted);
                assert_eq!(&open_row(&GOLDEN_KEY, &row).unwrap()[..], p);
            }
            None => assert!(row.deleted && row.envelope.is_empty()),
        }
    }
    for (k, v) in GOLDEN_ACCOUNT {
        assert_eq!(store.account_get(k).unwrap().as_deref(), Some(*v));
    }
    // The new tables start empty and the old rows are still pushable.
    assert_eq!(store.inbound_len().unwrap(), 0);
    assert_eq!(store.sync_get("anything").unwrap(), None);
    assert_eq!(store.dirty_rows(100).unwrap().len(), GOLDEN_RECORDS.len());
}

#[test]
fn migration_is_idempotent() {
    let (_dir, path) = golden_copy();
    drop(Store::open(&path).unwrap());
    let store = Store::open(&path).unwrap();
    store.sync_put("k", b"v").unwrap();
    drop(store);
    let store = Store::open(&path).unwrap();
    assert_eq!(user_version(&path), 2);
    assert_eq!(store.sync_get("k").unwrap().as_deref(), Some(&b"v"[..]));
    assert!(store.get(&id(GOLDEN_RECORDS[0].0)).unwrap().is_some());
}

#[test]
fn fresh_file_gets_v2_in_one_go() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("vault.sqlite");
    drop(Store::open(&path).unwrap());
    assert_eq!(user_version(&path), 2);
    assert!(table_exists(&path, "records"));
    assert!(table_exists(&path, "sync_state"));
    assert!(table_exists(&path, "inbound"));
}

#[test]
fn newer_schema_rejected() {
    let (_dir, path) = golden_copy();
    Connection::open(&path)
        .unwrap()
        .execute_batch("PRAGMA user_version = 3;")
        .unwrap();
    assert!(matches!(
        Store::open(&path),
        Err(core_store::StoreError::UnsupportedSchema {
            found: 3,
            supported: 2
        })
    ));
}
