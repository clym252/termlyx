//! Sync-facing store API: `sync_state`, the outbox queries and the inbound queue.

use core_model::{Hlc, Id, Kind};
use core_store::{InboundRow, RecordRow, Store, seal_row};
use tempfile::TempDir;

const KEY: [u8; 32] = [9u8; 32];

fn hlc(n: u64) -> Hlc {
    Hlc {
        wall_ms: n,
        counter: 0,
        node: 1,
    }
}

fn open() -> (TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("vault.sqlite")).unwrap();
    (dir, store)
}

fn row(vault: Id, n: u64) -> RecordRow {
    seal_row(&KEY, vault, Kind::Host, Id::now_v7(), 1, hlc(n), b"{}")
}

fn inbound(seq: i64, n: u64) -> InboundRow {
    InboundRow {
        seq,
        id: Id::now_v7(),
        kind: "host".to_owned(),
        key_version: 1,
        hlc: hlc(n),
        rev: seq,
        deleted: false,
        envelope: vec![seq as u8; 4],
    }
}

#[test]
fn sync_state_put_get_delete_clear() {
    let (_d, store) = open();
    assert_eq!(store.sync_get("cursor").unwrap(), None);
    store.sync_put("cursor", b"1").unwrap();
    store.sync_put("cursor", b"2").unwrap();
    store.sync_put("other", &[0, 1, 2]).unwrap();
    assert_eq!(
        store.sync_get("cursor").unwrap().as_deref(),
        Some(&b"2"[..])
    );
    store.sync_delete("cursor").unwrap();
    store.sync_delete("cursor").unwrap(); // missing key is fine
    assert_eq!(store.sync_get("cursor").unwrap(), None);

    store.inbound_push(&[inbound(1, 1)]).unwrap();
    store.account_put("kdf", b"keep").unwrap();
    store.sync_clear().unwrap();
    assert_eq!(store.sync_get("other").unwrap(), None);
    assert_eq!(store.inbound_len().unwrap(), 0);
    // Account material is not sync state.
    assert_eq!(
        store.account_get("kdf").unwrap().as_deref(),
        Some(&b"keep"[..])
    );
}

#[test]
fn sync_state_prefix_query_and_delete() {
    let (_d, store) = open();
    store.sync_put("cascade/a/2", b"two").unwrap();
    store.sync_put("cascade/a/1", b"one").unwrap();
    store.sync_put("cascade/ab/1", b"other owner").unwrap();
    store.sync_put("cursor", b"7").unwrap();
    assert_eq!(
        store.sync_prefix("cascade/a/").unwrap(),
        vec![
            ("cascade/a/1".to_owned(), b"one".to_vec()),
            ("cascade/a/2".to_owned(), b"two".to_vec()),
        ]
    );
    // `%` and `_` are not wildcards.
    assert!(store.sync_prefix("cascade/%").unwrap().is_empty());
    store.sync_delete_prefix("cascade/a/").unwrap();
    assert!(store.sync_prefix("cascade/a/").unwrap().is_empty());
    assert_eq!(store.sync_prefix("cascade/").unwrap().len(), 1);
    assert_eq!(
        store.sync_get("cursor").unwrap().as_deref(),
        Some(&b"7"[..])
    );

    store.sync_put("cascade/x/dep", b"1").unwrap();
    store.sync_put("cascade/y/dep", b"2").unwrap();
    store.sync_put("cascade/y/other", b"3").unwrap();
    store.sync_put("elsewhere/dep", b"4").unwrap();
    store.sync_delete_matching("cascade/", "/dep").unwrap();
    assert_eq!(
        store.sync_prefix("cascade/").unwrap(),
        vec![
            ("cascade/ab/1".to_owned(), b"other owner".to_vec()),
            ("cascade/y/other".to_owned(), b"3".to_vec()),
        ]
    );
    assert!(store.sync_get("elsewhere/dep").unwrap().is_some());
}

#[test]
fn dirty_rows_ordered_limited_and_include_tombstones() {
    let (_d, store) = open();
    let vault = Id::now_v7();
    let (a, b, c) = (row(vault, 30), row(vault, 10), row(vault, 20));
    for r in [&a, &b, &c] {
        store.put(r).unwrap();
    }
    store.tombstone(&c.id, hlc(40)).unwrap();
    store.put_synced(&row(vault, 5)).unwrap(); // clean: never listed

    // The tombstone took hlc 40, so it sorts last.
    let all = store.dirty_rows(10).unwrap();
    let order: Vec<_> = all.iter().map(|r| r.id).collect();
    assert_eq!(order, vec![b.id, a.id, c.id]);
    assert!(all[2].deleted);
    let two = store.dirty_rows(2).unwrap();
    assert_eq!(
        two.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![b.id, a.id]
    );
}

#[test]
fn mark_synced_clears_dirty_and_sets_rev() {
    let (_d, store) = open();
    let r = row(Id::now_v7(), 1);
    store.put(&r).unwrap();
    store.mark_synced(&r.id, &r.hlc, 7).unwrap();
    assert!(store.dirty_rows(10).unwrap().is_empty());
    assert_eq!(store.get(&r.id).unwrap().unwrap().rev, Some(7));
}

#[test]
fn mark_synced_keeps_newer_local_edit_dirty() {
    let (_d, store) = open();
    let r = row(Id::now_v7(), 1);
    store.put(&r).unwrap();
    // The user edits the record while the push of hlc(1) is in flight.
    let mut edited = r.clone();
    edited.hlc = hlc(2);
    store.put(&edited).unwrap();

    store.mark_synced(&r.id, &hlc(1), 7).unwrap();
    let got = store.get(&r.id).unwrap().unwrap();
    assert_eq!(
        got.rev,
        Some(7),
        "rev advances so the next push has the right base"
    );
    assert_eq!(got.hlc, hlc(2));
    assert_eq!(store.dirty_rows(10).unwrap().len(), 1, "still dirty");
}

#[test]
fn mark_synced_on_missing_row_is_a_noop() {
    let (_d, store) = open();
    store.mark_synced(&Id::now_v7(), &hlc(1), 3).unwrap();
}

#[test]
fn put_synced_is_not_dirty() {
    let (_d, store) = open();
    let mut r = row(Id::now_v7(), 1);
    r.rev = Some(4);
    store.put_synced(&r).unwrap();
    assert!(store.dirty_rows(10).unwrap().is_empty());
    assert_eq!(store.get(&r.id).unwrap().unwrap(), r);
    // Overwriting a dirty row with a remote one cleans it.
    let mut local = r.clone();
    local.hlc = hlc(2);
    store.put(&local).unwrap();
    assert_eq!(store.dirty_rows(10).unwrap().len(), 1);
    let mut remote = r.clone();
    remote.hlc = hlc(3);
    remote.rev = Some(5);
    store.put_synced(&remote).unwrap();
    assert!(store.dirty_rows(10).unwrap().is_empty());
    assert_eq!(store.get(&r.id).unwrap().unwrap(), remote);
}

#[test]
fn reset_revs_keeps_dirty_flags() {
    let (_d, store) = open();
    let vault = Id::now_v7();
    let (mut clean, dirty) = (row(vault, 1), row(vault, 2));
    clean.rev = Some(3);
    store.put_synced(&clean).unwrap();
    let mut d = dirty.clone();
    d.rev = Some(4);
    store.put(&d).unwrap();
    store.reset_revs().unwrap();
    assert!(store.get(&clean.id).unwrap().unwrap().rev.is_none());
    assert_eq!(store.is_dirty(&clean.id).unwrap(), Some(false));
    assert!(store.get(&dirty.id).unwrap().unwrap().rev.is_none());
    assert_eq!(store.is_dirty(&dirty.id).unwrap(), Some(true));
}

#[test]
fn mark_unsynced_dirty_marks_only_live_rows_without_rev() {
    let (_d, store) = open();
    let vault = Id::now_v7();
    let (mut synced, unsynced, gone) = (row(vault, 1), row(vault, 2), row(vault, 3));
    synced.rev = Some(1);
    store.put_synced(&synced).unwrap();
    store.put_synced(&unsynced).unwrap();
    let mut gone = gone;
    gone.deleted = true;
    gone.envelope = Vec::new();
    store.put_synced(&gone).unwrap();
    assert_eq!(store.dirty_count().unwrap(), 0);
    store.mark_unsynced_dirty().unwrap();
    assert_eq!(store.is_dirty(&synced.id).unwrap(), Some(false));
    assert_eq!(store.is_dirty(&unsynced.id).unwrap(), Some(true));
    // A clean tombstone the server lacks has nothing to tell it.
    assert_eq!(store.is_dirty(&gone.id).unwrap(), Some(false));
    assert_eq!(store.dirty_count().unwrap(), 1);
}

#[test]
fn inbound_clear_empties_the_queue_only() {
    let (_d, store) = open();
    store.inbound_push(&[inbound(1, 1), inbound(2, 2)]).unwrap();
    store.sync_put("cursor", b"2").unwrap();
    store.inbound_clear().unwrap();
    assert_eq!(store.inbound_len().unwrap(), 0);
    assert_eq!(
        store.sync_get("cursor").unwrap().as_deref(),
        Some(&b"2"[..])
    );
}

#[test]
fn backup_copy_and_wipe() {
    let (dir, store) = open();
    let vault = Id::now_v7();
    let r = row(vault, 1);
    store.put(&r).unwrap();
    store.account_put("kdf", b"old").unwrap();
    store.sync_put("url", b"https://x").unwrap();
    store.inbound_push(&[inbound(1, 1)]).unwrap();
    assert_eq!(
        store.path().unwrap().canonicalize().unwrap(),
        dir.path().join("vault.sqlite").canonicalize().unwrap()
    );

    let copy = dir.path().join("copy.sqlite");
    store.copy_to(&copy).unwrap();
    // Refuses to overwrite.
    assert!(store.copy_to(&copy).is_err());

    store.atomically(|| store.wipe()).unwrap();
    assert!(store.get(&r.id).unwrap().is_none());
    assert_eq!(store.account_get("kdf").unwrap(), None);
    assert_eq!(store.sync_get("url").unwrap(), None);
    assert_eq!(store.inbound_len().unwrap(), 0);

    let old = Store::open(&copy).unwrap();
    assert_eq!(old.get(&r.id).unwrap().unwrap().id, r.id);
    assert_eq!(
        old.account_get("kdf").unwrap().as_deref(),
        Some(&b"old"[..])
    );
}

#[test]
fn inbound_queue_fifo_and_delete_through() {
    let (_d, store) = open();
    // Pushed out of order and across calls; taken in ascending seq.
    store.inbound_push(&[inbound(3, 3), inbound(1, 1)]).unwrap();
    store.inbound_push(&[inbound(2, 2)]).unwrap();
    assert_eq!(store.inbound_len().unwrap(), 3);

    let first = store.inbound_take(2).unwrap();
    assert_eq!(first.iter().map(|r| r.seq).collect::<Vec<_>>(), vec![1, 2]);
    assert_eq!(store.inbound_len().unwrap(), 3, "take does not delete");

    store.inbound_delete_through(2).unwrap();
    let rest = store.inbound_take(10).unwrap();
    assert_eq!(rest.len(), 1);
    assert_eq!(rest[0].seq, 3);
    assert_eq!(rest[0].envelope, vec![3u8; 4]);
}

#[test]
fn inbound_round_trips_every_field() {
    let (_d, store) = open();
    let row = InboundRow {
        seq: 42,
        id: Id::now_v7(),
        kind: "some_future_kind".to_owned(),
        key_version: 3,
        hlc: Hlc {
            wall_ms: 123,
            counter: 4,
            node: 5,
        },
        rev: 9,
        deleted: true,
        envelope: vec![],
    };
    store.inbound_push(std::slice::from_ref(&row)).unwrap();
    assert_eq!(store.inbound_take(1).unwrap(), vec![row]);
}

#[test]
fn inbound_duplicate_seq_is_ignored() {
    let (_d, store) = open();
    let mut first = inbound(1, 1);
    first.envelope = b"first".to_vec();
    let mut again = inbound(1, 99);
    again.envelope = b"second".to_vec();
    store.inbound_push(std::slice::from_ref(&first)).unwrap();
    store.inbound_push(&[again, inbound(2, 2)]).unwrap(); // re-pulled page
    let all = store.inbound_take(10).unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!(all[0], first);
}

#[test]
fn local_edit_keeps_base_rev() {
    let (_d, store) = open();
    let mut r = row(Id::now_v7(), 1);
    r.rev = Some(5);
    store.put_synced(&r).unwrap();
    // A local edit is a fresh seal_row, which carries rev None.
    let mut edited = seal_row(&KEY, r.vault_id, Kind::Host, r.id, 1, hlc(2), b"{\"a\":1}");
    assert_eq!(edited.rev, None);
    store.put(&edited).unwrap();
    let got = store.get(&r.id).unwrap().unwrap();
    assert_eq!(got.rev, Some(5), "base rev survives a local edit");
    assert_eq!(got.hlc, hlc(2));
    assert_eq!(store.dirty_rows(10).unwrap().len(), 1);
    // An explicit rev on a local put still wins.
    edited.rev = Some(9);
    store.put(&edited).unwrap();
    assert_eq!(store.get(&r.id).unwrap().unwrap().rev, Some(9));
}

#[test]
fn put_synced_writes_the_given_rev_exactly() {
    let (_d, store) = open();
    let mut r = row(Id::now_v7(), 1);
    r.rev = Some(5);
    store.put_synced(&r).unwrap();
    r.rev = None;
    store.put_synced(&r).unwrap();
    assert_eq!(store.get(&r.id).unwrap().unwrap().rev, None);
}

#[test]
fn tombstone_keeps_base_rev() {
    let (_d, store) = open();
    let mut r = row(Id::now_v7(), 1);
    r.rev = Some(5);
    store.put_synced(&r).unwrap();
    store.tombstone(&r.id, hlc(2)).unwrap();
    let got = store.get(&r.id).unwrap().unwrap();
    assert!(got.deleted);
    assert_eq!(got.rev, Some(5));
    assert_eq!(store.dirty_rows(10).unwrap().len(), 1);
}

#[test]
fn is_dirty_reports_the_flag() {
    let (_d, store) = open();
    let vault = Id::now_v7();
    let r = row(vault, 1);
    assert_eq!(store.is_dirty(&r.id).unwrap(), None);
    store.put(&r).unwrap();
    assert_eq!(store.is_dirty(&r.id).unwrap(), Some(true));
    store.mark_synced(&r.id, &r.hlc, 1).unwrap();
    assert_eq!(store.is_dirty(&r.id).unwrap(), Some(false));
    store.tombstone(&r.id, hlc(2)).unwrap();
    assert_eq!(store.is_dirty(&r.id).unwrap(), Some(true));
}

#[test]
fn max_hlc_covers_records_tombstones_and_the_inbound_queue() {
    let (_d, store) = open();
    let vault = Id::now_v7();
    assert_eq!(store.max_hlc(&vault).unwrap(), None);
    let a = row(vault, 5);
    store.put(&a).unwrap();
    store.put(&row(Id::now_v7(), 50)).unwrap(); // another vault's row
    assert_eq!(store.max_hlc(&vault).unwrap(), Some(hlc(5)));
    store.tombstone(&a.id, hlc(7)).unwrap();
    assert_eq!(store.max_hlc(&vault).unwrap(), Some(hlc(7)));
    store.inbound_push(&[inbound(1, 9)]).unwrap();
    assert_eq!(store.max_hlc(&vault).unwrap(), Some(hlc(9)));
}

#[test]
fn inbound_latest_returns_the_newest_queued_row_of_an_id() {
    let (_d, store) = open();
    let first = inbound(1, 1);
    let other = inbound(2, 2);
    let newer = InboundRow {
        seq: 3,
        rev: 9,
        deleted: true,
        envelope: vec![],
        ..first.clone()
    };
    assert_eq!(store.inbound_latest(&first.id).unwrap(), None);
    store
        .inbound_push(&[first.clone(), other, newer.clone()])
        .unwrap();
    assert_eq!(store.inbound_latest(&first.id).unwrap(), Some(newer));
}
