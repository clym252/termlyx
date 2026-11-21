//! Vault sync surface: merge rules, conflict records, repairs and conflict
//! resolution. Two `VaultService`s share one account (`clone_for_device`) and
//! exchange rows through a tiny in-memory stand-in for the server that applies
//! the protocol's push rule (accepted iff the base revision matches).

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use core_api::vault::{
    ConflictSide, MemoryKeyStore, MergeReport, Resolution, VaultError, VaultService,
};
use core_model::{
    ConnSettings, ForwardKind, ForwardRule, Group, Hlc, Host, Id, Identity, Kind, KnownHost,
    KnownHostSource, Snippet,
};
use core_store::InboundRow;
use secrecy::{ExposeSecret, SecretString};
use tempfile::TempDir;

const PASSWORD: &str = "master";

fn pw() -> SecretString {
    SecretString::from(PASSWORD)
}

/// One device: its vault and its pull cursor.
struct Device {
    vault: VaultService,
    cursor: i64,
}

/// Server stand-in: the latest row of every record, with `rev` and `seq`.
#[derive(Default)]
struct Server {
    rows: BTreeMap<Id, InboundRow>,
    seq: i64,
}

impl Server {
    /// Pushes the device's whole outbox once. Accepted rows are acked; a
    /// conflict is handed to `apply_conflict`. Returns the merged report of
    /// the conflicts.
    fn push(&mut self, dev: &mut Device) -> MergeReport {
        let report = self.push_results(dev);
        // As the engine does once a push's results are applied.
        dev.vault.revive_restored_owners().unwrap();
        report
    }

    /// [`Server::push`] without the closing revive (a crash right after).
    fn push_results(&mut self, dev: &mut Device) -> MergeReport {
        let mut report = MergeReport::default();
        for row in dev.vault.outbox(1000).unwrap() {
            let current = self.rows.get(&row.id).cloned();
            let accepted = match (&current, row.rev) {
                (None, None) => true,
                (Some(c), Some(base)) => c.rev == base,
                _ => false,
            };
            if accepted {
                self.seq += 1;
                let rev = row.rev.map_or(1, |b| b + 1);
                self.rows.insert(
                    row.id,
                    InboundRow {
                        seq: self.seq,
                        id: row.id,
                        kind: row.kind.as_str().to_owned(),
                        key_version: row.key_version,
                        hlc: row.hlc,
                        rev,
                        deleted: row.deleted,
                        envelope: row.envelope.clone(),
                    },
                );
                dev.vault.ack_pushed(row.id, row.hlc, rev).unwrap();
            } else {
                let current = current.expect("no 410 in these tests");
                if current.hlc == row.hlc && current.envelope == row.envelope {
                    dev.vault.ack_pushed(row.id, row.hlc, current.rev).unwrap();
                    continue;
                }
                let r = dev.vault.apply_conflict(row.id, current).unwrap();
                report.applied += r.applied;
                report.conflicts += r.conflicts;
                report.skipped_unknown += r.skipped_unknown;
            }
        }
        report
    }

    /// Queues every row newer than the device's cursor (ascending `seq`).
    fn pull(&self, dev: &mut Device) {
        let mut rows: Vec<InboundRow> = self
            .rows
            .values()
            .filter(|r| r.seq > dev.cursor)
            .cloned()
            .collect();
        rows.sort_by_key(|r| r.seq);
        if let Some(last) = rows.last() {
            dev.cursor = last.seq;
        }
        dev.vault.queue_inbound(rows).unwrap();
    }

    /// Pull + apply, then push until the outbox is empty.
    fn sync(&mut self, dev: &mut Device) -> MergeReport {
        self.pull(dev);
        let mut report = dev.vault.apply_inbound().unwrap();
        for _ in 0..5 {
            if dev.vault.outbox(1000).unwrap().is_empty() {
                break;
            }
            let r = self.push(dev);
            report.applied += r.applied;
            report.conflicts += r.conflicts;
        }
        assert!(dev.vault.outbox(1000).unwrap().is_empty(), "push loop");
        report
    }
}

struct World {
    _dir: TempDir,
    server: Server,
    a: Device,
    b: Device,
}

fn world() -> World {
    let dir = tempfile::tempdir().unwrap();
    let mut a = VaultService::open_for_tests(
        &dir.path().join("a.sqlite"),
        Box::new(MemoryKeyStore::default()),
    )
    .unwrap();
    a.create(pw()).unwrap();
    let mut b = a.clone_for_device(&dir.path().join("b.sqlite"), 2).unwrap();
    assert!(!b.is_unlocked(), "clone starts locked");
    b.unlock(pw()).unwrap();
    // As the sync engine marks a synced vault (the cascade stash needs it).
    a.sync_put("enabled", b"1").unwrap();
    b.sync_put("enabled", b"1").unwrap();
    World {
        _dir: dir,
        server: Server::default(),
        a: Device {
            vault: a,
            cursor: 0,
        },
        b: Device {
            vault: b,
            cursor: 0,
        },
    }
}

impl World {
    /// Both devices fully caught up (each synced twice, alternately).
    fn settle(&mut self) {
        for _ in 0..2 {
            self.server.sync(&mut self.a);
            self.server.sync(&mut self.b);
        }
    }
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

fn group(name: &str, parent_id: Option<Id>) -> Group {
    Group {
        id: Id::now_v7(),
        parent_id,
        name: name.into(),
        settings: ConnSettings::default(),
        extra: Default::default(),
    }
}

fn identity(password: &str) -> Identity {
    Identity {
        id: Id::now_v7(),
        label: "prod".into(),
        username: "root".into(),
        password: Some(SecretString::from(password)),
        key_id: None,
        owner: None,
        extra: Default::default(),
    }
}

fn known(id: Id) -> KnownHost {
    KnownHost {
        id,
        host_pattern: "example.org".into(),
        port: 22,
        algorithm: "ssh-ed25519".into(),
        public_key: "AAAAC3NzaC1lZDI1NTE5AAAAIexample".into(),
        added_at: 1,
        source: KnownHostSource::Prompt,
        extra: Default::default(),
    }
}

/// Makes sure the next write's wall time is later than everything before it.
fn tick() {
    std::thread::sleep(Duration::from_millis(3));
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

/// A conflict where A's edit (older) reached the server first and B's newer
/// edit wins: B keeps its version and records A's.
fn concurrent_host_edit(w: &mut World) -> Host {
    let h = host("original");
    w.a.vault.upsert(h.clone()).unwrap();
    w.settle();

    w.a.vault
        .upsert(Host {
            label: "from-a".into(),
            address: "a.example".into(),
            ..h.clone()
        })
        .unwrap();
    tick();
    w.b.vault
        .upsert(Host {
            label: "from-b".into(),
            ..h.clone()
        })
        .unwrap();
    w.server.sync(&mut w.a);
    let report = w.server.sync(&mut w.b);
    assert_eq!(report.conflicts, 1, "{report:?}");
    h
}

#[test]
fn remote_row_applies_when_local_clean() {
    let mut w = world();
    let h = host("web");
    w.a.vault.upsert(h.clone()).unwrap();
    w.server.sync(&mut w.a);
    let report = w.server.sync(&mut w.b);
    assert_eq!(report.applied, 1);
    assert_eq!(report.conflicts, 0);
    assert_eq!(w.b.vault.get::<Host>(h.id).unwrap().unwrap(), h);
    assert!(w.b.vault.outbox(10).unwrap().is_empty(), "taken as synced");
    assert!(w.b.vault.list_conflicts().unwrap().is_empty());
}

#[test]
fn concurrent_edit_higher_hlc_wins_and_loser_is_conflict() {
    let mut w = world();
    let h = concurrent_host_edit(&mut w);
    w.settle();

    for dev in [&w.a, &w.b] {
        let current = dev.vault.get::<Host>(h.id).unwrap().unwrap();
        assert_eq!(current.label, "from-b", "higher HLC wins everywhere");
        let conflicts = dev.vault.list_conflicts().unwrap();
        assert_eq!(conflicts.len(), 1, "one conflict, synced to both");
        let c = &conflicts[0];
        assert_eq!(c.record_id, h.id);
        assert_eq!(c.kind, Kind::Host);
        assert_eq!(c.other.as_ref().unwrap()["label"], "from-a");
        assert_eq!(c.current.as_ref().unwrap()["label"], "from-b");
        assert_eq!(c.changed_fields, vec!["address", "label"]);
    }

    // The other way round: the local version is the older one and loses.
    let mut w = world();
    let h = host("original");
    w.a.vault.upsert(h.clone()).unwrap();
    w.settle();
    w.b.vault
        .upsert(Host {
            label: "older-b".into(),
            ..h.clone()
        })
        .unwrap();
    tick();
    w.a.vault
        .upsert(Host {
            label: "newer-a".into(),
            ..h.clone()
        })
        .unwrap();
    w.server.sync(&mut w.a);
    assert_eq!(w.server.sync(&mut w.b).conflicts, 1);
    assert_eq!(
        w.b.vault.get::<Host>(h.id).unwrap().unwrap().label,
        "newer-a"
    );
    let c = &w.b.vault.list_conflicts().unwrap()[0];
    assert_eq!(c.other.as_ref().unwrap()["label"], "older-b");
}

#[test]
fn snippet_concurrent_edit_is_a_conflict_showing_its_fields() {
    let mut w = world();
    let s = Snippet {
        id: Id::now_v7(),
        label: "Upgrade".into(),
        text: "sudo apt update".into(),
        run: false,
        extra: Default::default(),
    };
    w.a.vault.upsert(s.clone()).unwrap();
    w.settle();
    w.a.vault
        .upsert(Snippet {
            text: "sudo apt upgrade".into(),
            ..s.clone()
        })
        .unwrap();
    tick();
    w.b.vault
        .upsert(Snippet {
            label: "Upgrade all".into(),
            run: true,
            ..s.clone()
        })
        .unwrap();
    w.settle();
    for dev in [&w.a, &w.b] {
        assert_eq!(dev.vault.get::<Snippet>(s.id).unwrap().unwrap().label, "Upgrade all");
        let conflicts = dev.vault.list_conflicts().unwrap();
        assert_eq!(conflicts.len(), 1);
        let c = &conflicts[0];
        assert_eq!(c.kind, Kind::Snippet);
        assert_eq!(c.other.as_ref().unwrap()["text"], "sudo apt upgrade");
        assert_eq!(c.changed_fields, vec!["label", "run", "text"]);
    }
}

#[test]
fn conflict_keeps_losing_password() {
    let mut w = world();
    let i = identity("pw-original");
    w.a.vault.upsert(i.clone()).unwrap();
    w.settle();

    w.a.vault
        .upsert(Identity {
            password: Some(SecretString::from("pw-from-a")),
            ..i.clone()
        })
        .unwrap();
    tick();
    w.b.vault
        .upsert(Identity {
            password: Some(SecretString::from("pw-from-b")),
            ..i.clone()
        })
        .unwrap();
    w.server.sync(&mut w.a);
    assert_eq!(w.server.sync(&mut w.b).conflicts, 1);
    w.settle();

    for dev in [&w.a, &w.b] {
        let current = dev.vault.get::<Identity>(i.id).unwrap().unwrap();
        assert_eq!(current.password.unwrap().expose_secret(), "pw-from-b");
        let c = &dev.vault.list_conflicts().unwrap()[0];
        assert_eq!(c.changed_fields, vec!["password"]);
        let lost = dev
            .vault
            .reveal_conflict_field(c.id, ConflictSide::Other, "password")
            .unwrap();
        assert_eq!(lost.expose_secret(), "pw-from-a");
        let kept = dev
            .vault
            .reveal_conflict_field(c.id, ConflictSide::Current, "password")
            .unwrap();
        assert_eq!(kept.expose_secret(), "pw-from-b");
        // Only secret fields can be revealed.
        assert!(
            dev.vault
                .reveal_conflict_field(c.id, ConflictSide::Other, "username")
                .is_err()
        );
    }
}

#[test]
fn edit_beats_delete_and_records_conflict() {
    // Remote deletion vs local edit: the local edit wins.
    let mut w = world();
    let h = host("doomed");
    w.a.vault.upsert(h.clone()).unwrap();
    w.settle();
    tick();
    w.b.vault
        .upsert(Host {
            label: "edited-b".into(),
            ..h.clone()
        })
        .unwrap();
    tick();
    w.a.vault.delete::<Host>(h.id).unwrap();
    w.server.sync(&mut w.a);
    assert_eq!(w.server.sync(&mut w.b).conflicts, 1);
    w.settle();
    for dev in [&w.a, &w.b] {
        let got = dev.vault.get::<Host>(h.id).unwrap();
        assert_eq!(got.unwrap().label, "edited-b", "edit restored the record");
        let c = &dev.vault.list_conflicts().unwrap()[0];
        assert_eq!(c.record_id, h.id);
        assert!(c.other.is_none(), "the other side is the deletion");
        assert!(c.current.is_some());
    }

    // Local deletion vs remote edit: the record comes back.
    let mut w = world();
    let h = host("doomed");
    w.a.vault.upsert(h.clone()).unwrap();
    w.settle();
    w.b.vault.delete::<Host>(h.id).unwrap();
    tick();
    w.a.vault
        .upsert(Host {
            label: "edited-a".into(),
            ..h.clone()
        })
        .unwrap();
    w.server.sync(&mut w.a);
    assert_eq!(w.server.sync(&mut w.b).conflicts, 1);
    w.settle();
    for dev in [&w.a, &w.b] {
        let got = dev.vault.get::<Host>(h.id).unwrap();
        assert_eq!(got.unwrap().label, "edited-a", "record restored");
        let c = &dev.vault.list_conflicts().unwrap()[0];
        assert!(c.other.is_none());
    }
}

#[test]
fn delete_vs_delete_no_conflict() {
    let mut w = world();
    let h = host("gone");
    w.a.vault.upsert(h.clone()).unwrap();
    w.settle();
    w.a.vault.delete::<Host>(h.id).unwrap();
    w.b.vault.delete::<Host>(h.id).unwrap();
    w.server.sync(&mut w.a);
    let report = w.server.sync(&mut w.b);
    assert_eq!(report.conflicts, 0);
    w.settle();
    for dev in [&w.a, &w.b] {
        assert!(dev.vault.get::<Host>(h.id).unwrap().is_none());
        assert!(dev.vault.list_conflicts().unwrap().is_empty());
    }
}

#[test]
fn unknown_kind_skipped_and_counted() {
    let mut w = world();
    let h = host("web");
    w.a.vault.upsert(h.clone()).unwrap();
    w.server.sync(&mut w.a);
    let known_row = w.server.rows[&h.id].clone();
    let future = InboundRow {
        seq: known_row.seq + 1,
        id: Id::now_v7(),
        kind: "teleporter".into(),
        key_version: 1,
        hlc: known_row.hlc,
        rev: 1,
        deleted: false,
        envelope: vec![1, 2, 3],
    };
    w.b.vault.queue_inbound(vec![known_row, future]).unwrap();
    let report = w.b.vault.apply_inbound().unwrap();
    assert_eq!(report.skipped_unknown, 1);
    assert_eq!(report.applied, 1);
    assert!(w.b.vault.get::<Host>(h.id).unwrap().is_some());
    // The queue advanced past both rows.
    assert_eq!(w.b.vault.apply_inbound().unwrap(), MergeReport::default());
}

#[test]
fn report_counts_only_rows_that_change_what_the_vault_shows() {
    let mut w = world();
    let h = host("web");
    w.a.vault.upsert(h.clone()).unwrap();
    // A's own push comes back as an echo on its next pull: nothing new.
    let first = w.server.sync(&mut w.a);
    assert!(!first.touched(), "{first:?}");
    let echo = w.server.sync(&mut w.a);
    assert!(!echo.touched(), "{echo:?}");

    // B takes the new host.
    let got = w.server.sync(&mut w.b);
    assert_eq!((got.changed, got.conflicts), (1, 0));

    // B edits it; A takes the edit; B's echo changes nothing.
    let mut edited = w.b.vault.get::<Host>(h.id).unwrap().unwrap();
    edited.label = "edited".into();
    w.b.vault.upsert(edited).unwrap();
    let pushed = w.server.sync(&mut w.b);
    assert!(!pushed.touched(), "{pushed:?}");
    let got = w.server.sync(&mut w.a);
    assert_eq!((got.changed, got.conflicts), (1, 0));
    let echo = w.server.sync(&mut w.b);
    assert!(!echo.touched(), "{echo:?}");

    // A tombstone for a record this device never had shows nothing.
    let gone = host("never seen");
    w.a.vault.upsert(gone.clone()).unwrap();
    w.server.sync(&mut w.a);
    w.a.vault.delete::<Host>(gone.id).unwrap();
    w.server.sync(&mut w.a);
    let mut seen = w.server.sync(&mut w.b);
    seen += w.server.sync(&mut w.b);
    assert_eq!(seen.changed, 0, "{seen:?}");
}

#[test]
fn report_counts_a_created_conflict() {
    let mut w = world();
    let h = host("web");
    w.a.vault.upsert(h.clone()).unwrap();
    w.settle();
    for (dev, label) in [(&mut w.a, "from a"), (&mut w.b, "from b")] {
        let mut x = dev.vault.get::<Host>(h.id).unwrap().unwrap();
        x.label = label.into();
        dev.vault.upsert(x).unwrap();
        std::thread::sleep(Duration::from_millis(5));
    }
    w.server.sync(&mut w.b);
    let report = w.server.sync(&mut w.a);
    assert!(report.touched(), "{report:?}");
    assert_eq!(report.conflicts, 1);
}

#[test]
fn observe_advances_local_clock() {
    let mut w = world();
    let h = host("web");
    w.a.vault.upsert(h.clone()).unwrap();
    w.server.sync(&mut w.a);
    // A peer whose clock runs 10 minutes ahead (the hlc is not in the AAD).
    let mut row = w.server.rows[&h.id].clone();
    let ahead = Hlc {
        wall_ms: now_ms() + 10 * 60 * 1000,
        counter: 0,
        node: 7,
    };
    row.hlc = ahead;
    w.b.vault.queue_inbound(vec![row]).unwrap();
    w.b.vault.apply_inbound().unwrap();

    w.b.vault.upsert(host("later")).unwrap();
    let out = w.b.vault.outbox(10).unwrap();
    assert_eq!(out.len(), 1);
    assert!(out[0].hlc > ahead, "{} not after {}", out[0].hlc, ahead);
}

#[test]
fn repair_breaks_group_cycle_deterministically() {
    let mut w = world();
    let g1 = group("one", None);
    let g2 = group("two", None);
    w.a.vault.upsert_all(vec![g1.clone(), g2.clone()]).unwrap();
    w.settle();

    // Concurrent moves, each valid locally: together they form a loop.
    w.a.vault
        .upsert(Group {
            parent_id: Some(g2.id),
            ..g1.clone()
        })
        .unwrap();
    w.b.vault
        .upsert(Group {
            parent_id: Some(g1.id),
            ..g2.clone()
        })
        .unwrap();
    w.settle();
    for dev in [&w.a, &w.b] {
        assert!(dev.vault.list_conflicts().unwrap().is_empty());
    }

    // Both devices, caught up, repair independently.
    assert_eq!(w.a.vault.repair().unwrap(), 1);
    assert_eq!(w.b.vault.repair().unwrap(), 1);
    let parents = |v: &VaultService| {
        let mut gs: Vec<(Id, Option<Id>)> = v
            .list::<Group>()
            .unwrap()
            .into_iter()
            .map(|g| (g.id, g.parent_id))
            .collect();
        gs.sort();
        gs
    };
    let after_a = parents(&w.a.vault);
    assert_eq!(after_a, parents(&w.b.vault), "same choice on both devices");
    assert_eq!(
        after_a.iter().filter(|(_, p)| p.is_none()).count(),
        1,
        "exactly one group moved to the root"
    );
    // A second repair finds nothing.
    assert_eq!(w.a.vault.repair().unwrap(), 0);

    // The identical repair writes converge without a conflict.
    w.settle();
    assert_eq!(parents(&w.a.vault), after_a);
    assert_eq!(parents(&w.b.vault), after_a);
    for dev in [&w.a, &w.b] {
        assert!(dev.vault.list_conflicts().unwrap().is_empty());
    }
}

#[test]
fn repair_never_deletes() {
    let mut w = world();
    let h = host("web");
    w.a.vault.upsert(h.clone()).unwrap();
    let login =
        w.a.vault
            .set_own_login(
                h.id,
                "deploy".into(),
                core_api::vault::PasswordChange::Set(SecretString::from("pw")),
            )
            .unwrap();
    // Unlink the own login without tidying: it is now an orphan.
    let mut unlinked = w.a.vault.get::<Host>(h.id).unwrap().unwrap();
    unlinked.settings.identity_id = None;
    w.a.vault.upsert(unlinked).unwrap();
    w.server.sync(&mut w.a);
    w.server.sync(&mut w.b);

    for dev in [&mut w.a, &mut w.b] {
        assert_eq!(dev.vault.repair().unwrap(), 0);
        let kept = dev.vault.get::<Identity>(login.id).unwrap().unwrap();
        assert_eq!(kept.password.unwrap().expose_secret(), "pw");
        assert!(dev.vault.get::<Host>(h.id).unwrap().is_some());
    }
}

#[test]
fn known_hosts_duplicates_collapse_on_list() {
    let mut w = world();
    // Both devices trust the same key on their own, before syncing.
    let first = known(Id::now_v7());
    let second = known(Id::now_v7());
    w.a.vault.upsert(second.clone()).unwrap();
    w.b.vault.upsert(first.clone()).unwrap();
    w.settle();
    let lowest = first.id.min(second.id);
    for dev in [&w.a, &w.b] {
        let listed = dev.vault.list::<KnownHost>().unwrap();
        assert_eq!(listed.len(), 1, "duplicates collapse");
        assert_eq!(listed[0].id, lowest, "the lowest id is kept");
        // Nothing was deleted: both rows are still readable by id.
        assert!(dev.vault.get::<KnownHost>(first.id).unwrap().is_some());
        assert!(dev.vault.get::<KnownHost>(second.id).unwrap().is_some());
    }
    // Removing the trusted key removes its hidden duplicates too.
    w.a.vault.delete::<KnownHost>(lowest).unwrap();
    assert!(w.a.vault.list::<KnownHost>().unwrap().is_empty());
}

#[test]
fn resolve_use_other_restores_loser() {
    let mut w = world();
    let h = concurrent_host_edit(&mut w);
    let c = w.b.vault.list_conflicts().unwrap().remove(0);
    w.b.vault
        .resolve_conflict(c.id, Resolution::UseOther)
        .unwrap();
    let got = w.b.vault.get::<Host>(h.id).unwrap().unwrap();
    assert_eq!(got.label, "from-a");
    assert_eq!(got.address, "a.example");
    assert!(w.b.vault.list_conflicts().unwrap().is_empty());
    w.settle();
    let on_a = w.a.vault.get::<Host>(h.id).unwrap().unwrap();
    assert_eq!(on_a.label, "from-a");
    assert!(
        w.a.vault.list_conflicts().unwrap().is_empty(),
        "dismissal synced"
    );

    // Using the other version of an edit-vs-delete conflict deletes again.
    let mut w = world();
    let h = host("doomed");
    w.a.vault.upsert(h.clone()).unwrap();
    w.settle();
    tick();
    w.b.vault
        .upsert(Host {
            label: "edited-b".into(),
            ..h.clone()
        })
        .unwrap();
    w.a.vault.delete::<Host>(h.id).unwrap();
    w.server.sync(&mut w.a);
    w.server.sync(&mut w.b);
    let c = w.b.vault.list_conflicts().unwrap().remove(0);
    w.b.vault
        .resolve_conflict(c.id, Resolution::UseOther)
        .unwrap();
    assert!(w.b.vault.get::<Host>(h.id).unwrap().is_none());
    assert!(w.b.vault.list_conflicts().unwrap().is_empty());
}

#[test]
fn resolve_keep_current_dismisses() {
    let mut w = world();
    let h = concurrent_host_edit(&mut w);
    let c = w.b.vault.list_conflicts().unwrap().remove(0);
    w.b.vault
        .resolve_conflict(c.id, Resolution::KeepCurrent)
        .unwrap();
    assert_eq!(
        w.b.vault.get::<Host>(h.id).unwrap().unwrap().label,
        "from-b"
    );
    assert!(w.b.vault.list_conflicts().unwrap().is_empty());
    assert!(matches!(
        w.b.vault.resolve_conflict(c.id, Resolution::KeepCurrent),
        Err(VaultError::Store(_))
    ));
}

#[test]
fn resolve_fields_merges_selected_fields() {
    let mut w = world();
    let h = concurrent_host_edit(&mut w);
    let c = w.b.vault.list_conflicts().unwrap().remove(0);
    w.b.vault
        .resolve_conflict(c.id, Resolution::Fields(vec!["address".into()]))
        .unwrap();
    let got = w.b.vault.get::<Host>(h.id).unwrap().unwrap();
    assert_eq!(got.label, "from-b", "unselected field kept");
    assert_eq!(got.address, "a.example", "selected field taken");
    assert!(w.b.vault.list_conflicts().unwrap().is_empty());

    // Secrets can be picked too.
    let mut w = world();
    let i = identity("pw-original");
    w.a.vault.upsert(i.clone()).unwrap();
    w.settle();
    w.a.vault
        .upsert(Identity {
            password: Some(SecretString::from("pw-from-a")),
            ..i.clone()
        })
        .unwrap();
    tick();
    w.b.vault
        .upsert(Identity {
            username: "admin".into(),
            ..i.clone()
        })
        .unwrap();
    w.server.sync(&mut w.a);
    w.server.sync(&mut w.b);
    let c = w.b.vault.list_conflicts().unwrap().remove(0);
    w.b.vault
        .resolve_conflict(c.id, Resolution::Fields(vec!["password".into()]))
        .unwrap();
    let got = w.b.vault.get::<Identity>(i.id).unwrap().unwrap();
    assert_eq!(got.username, "admin");
    assert_eq!(got.password.unwrap().expose_secret(), "pw-from-a");
}

#[test]
fn conflict_view_masks_secrets() {
    let mut w = world();
    let i = identity("CANARY-original");
    w.a.vault.upsert(i.clone()).unwrap();
    w.settle();
    w.a.vault
        .upsert(Identity {
            password: Some(SecretString::from("CANARY-a")),
            ..i.clone()
        })
        .unwrap();
    tick();
    w.b.vault
        .upsert(Identity {
            password: Some(SecretString::from("CANARY-b")),
            ..i.clone()
        })
        .unwrap();
    w.server.sync(&mut w.a);
    w.server.sync(&mut w.b);
    let views = w.b.vault.list_conflicts().unwrap();
    assert_eq!(views.len(), 1);
    let v = &views[0];
    let text = format!(
        "{} {} {:?}",
        v.current.as_ref().unwrap(),
        v.other.as_ref().unwrap(),
        v
    );
    assert!(!text.contains("CANARY"), "secret leaked into the view");
    let marker = serde_json::json!({"hasSecret": true});
    assert_eq!(v.current.as_ref().unwrap()["password"], marker);
    assert_eq!(v.other.as_ref().unwrap()["password"], marker);
    // Compared by value, even though both are masked.
    assert_eq!(v.changed_fields, vec!["password"]);
}

#[test]
fn inbound_queued_while_locked_applies_on_unlock() {
    let mut w = world();
    let h = host("web");
    w.a.vault.upsert(h.clone()).unwrap();
    w.server.sync(&mut w.a);

    w.b.vault.lock();
    w.server.pull(&mut w.b);
    assert!(matches!(w.b.vault.apply_inbound(), Err(VaultError::Locked)));
    w.b.vault.unlock(pw()).unwrap();
    let report = w.b.vault.apply_inbound().unwrap();
    assert_eq!(report.applied, 1);
    assert!(w.b.vault.get::<Host>(h.id).unwrap().is_some());
}

#[test]
fn redelivered_row_is_ignored() {
    let mut w = world();
    let h = host("web");
    w.a.vault.upsert(h.clone()).unwrap();
    w.settle();
    let old = w.server.rows[&h.id].clone();
    // B edits on top of the row it already has; the same row arrives again.
    w.b.vault
        .upsert(Host {
            label: "edited".into(),
            ..h.clone()
        })
        .unwrap();
    w.b.vault
        .queue_inbound(vec![InboundRow { seq: 999, ..old }])
        .unwrap();
    let report = w.b.vault.apply_inbound().unwrap();
    assert_eq!(report.conflicts, 0, "the base version is not a conflict");
    assert_eq!(
        w.b.vault.get::<Host>(h.id).unwrap().unwrap().label,
        "edited"
    );
    assert_eq!(
        w.b.vault.outbox(10).unwrap().len(),
        1,
        "edit still unpushed"
    );
}

#[test]
fn clone_shares_the_account() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = VaultService::open_for_tests(
        &dir.path().join("a.sqlite"),
        Box::new(MemoryKeyStore::default()),
    )
    .unwrap();
    a.create(pw()).unwrap();
    let path: &Path = &dir.path().join("b.sqlite");
    let mut b = a.clone_for_device(path, 9).unwrap();
    assert!(matches!(
        b.unlock(SecretString::from("wrong")),
        Err(VaultError::WrongPassword)
    ));
    b.unlock(pw()).unwrap();
    assert!(b.list::<Host>().unwrap().is_empty(), "no records copied");
    b.upsert(host("x")).unwrap();
    assert_eq!(b.outbox(10).unwrap()[0].hlc.node, 9);
}

/// A third device, and a host that A, C and B (in that order, so B's HLC is
/// the highest) edited at once. A's version is on the server; B and C have
/// each pulled it and recorded it as a conflict, neither has pushed.
fn detected_twice(w: &mut World) -> (Device, Host) {
    let mut c = Device {
        vault: w
            .a
            .vault
            .clone_for_device(&w._dir.path().join("c.sqlite"), 3)
            .unwrap(),
        cursor: 0,
    };
    c.vault.unlock(pw()).unwrap();
    let h = host("original");
    w.a.vault.upsert(h.clone()).unwrap();
    w.settle();
    w.server.sync(&mut c);

    let edit = |v: &mut VaultService, label: &str| {
        v.upsert(Host {
            label: label.into(),
            ..h.clone()
        })
        .unwrap();
        tick();
    };
    edit(&mut w.a.vault, "from-a");
    edit(&mut c.vault, "from-c");
    edit(&mut w.b.vault, "from-b");
    w.server.sync(&mut w.a);
    for dev in [&mut w.b, &mut c] {
        w.server.pull(dev);
        assert_eq!(dev.vault.apply_inbound().unwrap().conflicts, 1);
    }
    (c, h)
}

/// Labels of the losing versions a device lists.
fn lost_labels(v: &VaultService) -> Vec<String> {
    let mut lost: Vec<String> = v
        .list_conflicts()
        .unwrap()
        .into_iter()
        .map(|c| {
            assert_eq!(c.kind, Kind::Host, "no conflict about a conflict");
            c.other.unwrap()["label"].as_str().unwrap().to_owned()
        })
        .collect();
    lost.sort();
    lost
}

#[test]
fn same_conflict_detected_twice_is_one_record() {
    let mut w = world();
    let (mut c, h) = detected_twice(&mut w);
    w.server.sync(&mut c);
    w.server.sync(&mut w.b);
    w.settle();
    w.server.sync(&mut c);
    for dev in [&w.a, &w.b, &c] {
        let current = dev.vault.get::<Host>(h.id).unwrap().unwrap();
        assert_eq!(current.label, "from-b");
        assert_eq!(lost_labels(&dev.vault), vec!["from-a", "from-c"]);
    }
}

#[test]
fn dismissal_beats_a_second_detection() {
    let mut w = world();
    let (mut c, _h) = detected_twice(&mut w);
    // C dismisses A's version before B's copy of the same conflict arrives.
    let on_c = c.vault.list_conflicts().unwrap().remove(0);
    c.vault
        .resolve_conflict(on_c.id, Resolution::KeepCurrent)
        .unwrap();
    w.server.sync(&mut c);
    w.server.sync(&mut w.b);
    w.settle();
    w.server.sync(&mut c);
    for dev in [&w.a, &w.b, &c] {
        assert!(
            !lost_labels(&dev.vault).contains(&"from-a".to_owned()),
            "the dismissed conflict came back"
        );
    }
}

#[test]
fn local_edit_after_relock_sorts_after_observed_remote_hlc() {
    let mut w = world();
    let h = host("web");
    w.a.vault.upsert(h.clone()).unwrap();
    w.server.sync(&mut w.a);
    let ahead = |minutes: u64| Hlc {
        wall_ms: now_ms() + minutes * 60 * 1000,
        counter: 0,
        node: 7,
    };

    // Applied before the lock.
    let mut row = w.server.rows[&h.id].clone();
    let applied = ahead(10);
    row.hlc = applied;
    w.b.vault.queue_inbound(vec![row.clone()]).unwrap();
    w.b.vault.apply_inbound().unwrap();
    w.b.vault.lock();
    w.b.vault.unlock(pw()).unwrap();
    w.b.vault.upsert(host("after-relock")).unwrap();
    let out = w.b.vault.outbox(10).unwrap();
    assert!(
        out.iter().all(|r| r.hlc > applied),
        "edit sorted before seen row"
    );

    // Only queued (pulled while locked), not applied yet.
    w.b.vault.lock();
    let queued = ahead(20);
    w.b.vault
        .queue_inbound(vec![InboundRow {
            seq: row.seq + 1,
            hlc: queued,
            rev: row.rev + 1,
            ..row
        }])
        .unwrap();
    w.b.vault.unlock(pw()).unwrap();
    let late = host("before-apply");
    w.b.vault.upsert(late.clone()).unwrap();
    let out = w.b.vault.outbox(10).unwrap();
    let late_row = out.iter().find(|r| r.id == late.id).unwrap();
    assert!(late_row.hlc > queued, "edit sorted before a queued row");
}

#[test]
fn use_other_with_dangling_reference_clears_it() {
    let mut w = world();
    let g = group("ops", None);
    let tag = core_model::Tag {
        id: Id::now_v7(),
        name: "prod".into(),
        color: "red".into(),
        extra: Default::default(),
    };
    let ident = identity("pw");
    let h = host("original");
    w.a.vault.upsert(g.clone()).unwrap();
    w.a.vault.upsert(tag.clone()).unwrap();
    w.a.vault.upsert(ident.clone()).unwrap();
    w.a.vault.upsert(h.clone()).unwrap();
    w.settle();

    // A's (losing) version points at the group, tag and identity.
    let mut from_a = Host {
        label: "from-a".into(),
        group_id: Some(g.id),
        tag_ids: vec![tag.id],
        ..h.clone()
    };
    from_a.settings.identity_id = Some(ident.id);
    w.a.vault.upsert(from_a).unwrap();
    tick();
    w.b.vault
        .upsert(Host {
            label: "from-b".into(),
            ..h.clone()
        })
        .unwrap();
    w.server.sync(&mut w.a);
    assert_eq!(w.server.sync(&mut w.b).conflicts, 1);

    // All three are deleted before the user picks A's version.
    w.b.vault.delete::<Group>(g.id).unwrap();
    w.b.vault.delete::<core_model::Tag>(tag.id).unwrap();
    w.b.vault.delete::<Identity>(ident.id).unwrap();
    let c = w.b.vault.list_conflicts().unwrap().remove(0);
    w.b.vault
        .resolve_conflict(c.id, Resolution::UseOther)
        .unwrap();
    let got = w.b.vault.get::<Host>(h.id).unwrap().unwrap();
    assert_eq!(got.label, "from-a");
    assert_eq!(got.group_id, None, "missing group reads as unset");
    assert!(got.tag_ids.is_empty(), "missing tag dropped");
    assert_eq!(got.settings.identity_id, None, "missing identity unset");
    assert!(w.b.vault.list_conflicts().unwrap().is_empty());
}

/// How the two devices' concurrent changes reach the server.
#[derive(Debug, Clone, Copy)]
enum Order {
    /// The deleting device syncs first.
    DeleterFirst,
    /// The editing device syncs first; the deleter then pulls before pushing.
    EditorFirst,
    /// The editing device syncs first; the deleter pushes without pulling,
    /// so it learns of the edit through push conflicts.
    EditorFirstPushConflict,
}

const ORDERS: [Order; 3] = [
    Order::DeleterFirst,
    Order::EditorFirst,
    Order::EditorFirstPushConflict,
];

/// A (the deleter) and B (the editor) each made their change; deliver them.
fn deliver(w: &mut World, order: Order) {
    match order {
        Order::DeleterFirst => {
            w.server.sync(&mut w.a);
            w.server.sync(&mut w.b);
        }
        Order::EditorFirst => {
            w.server.sync(&mut w.b);
            w.server.sync(&mut w.a);
        }
        Order::EditorFirstPushConflict => {
            w.server.sync(&mut w.b);
            for _ in 0..3 {
                w.server.push(&mut w.a);
            }
        }
    }
    w.settle();
    w.settle();
}

/// A host with an own login (password) and a forward rule, synced to both.
fn host_with_login_and_rule(w: &mut World) -> (Host, Identity, ForwardRule) {
    let h = host("web");
    w.a.vault.upsert(h.clone()).unwrap();
    let login =
        w.a.vault
            .set_own_login(
                h.id,
                "deploy".into(),
                core_api::vault::PasswordChange::Set(SecretString::from("pw-own")),
            )
            .unwrap();
    let rule = ForwardRule {
        id: Id::now_v7(),
        host_id: h.id,
        label: "db".into(),
        kind: ForwardKind::Local,
        bind_addr: "127.0.0.1".into(),
        bind_port: 5432,
        dest_host: Some("db".into()),
        dest_port: Some(5432),
        autostart: false,
        extra: Default::default(),
    };
    w.a.vault.upsert(rule.clone()).unwrap();
    w.settle();
    (h, login, rule)
}

/// Deletes a host the way the app does: delete, then tidy own logins.
fn app_delete_host(v: &mut VaultService, id: Id) {
    v.delete::<Host>(id).unwrap();
    assert_eq!(v.tidy_own_logins().unwrap(), 1, "own login went with it");
}

#[test]
fn host_delete_vs_edit_restores_own_login_and_rules() {
    for order in ORDERS {
        let mut w = world();
        let (h, login, rule) = host_with_login_and_rule(&mut w);
        app_delete_host(&mut w.a.vault, h.id);
        tick();
        let mut edited = w.b.vault.get::<Host>(h.id).unwrap().unwrap();
        edited.label = "renamed".into();
        w.b.vault.upsert(edited).unwrap();
        deliver(&mut w, order);

        let state = |v: &VaultService| {
            let host = v.get::<Host>(h.id).unwrap().expect("host restored");
            let ident = v
                .get::<Identity>(login.id)
                .unwrap()
                .expect("login restored");
            let r = v
                .get::<ForwardRule>(rule.id)
                .unwrap()
                .expect("rule restored");
            (host, ident, r)
        };
        for dev in [&mut w.a, &mut w.b] {
            let (host, ident, r) = state(&dev.vault);
            assert_eq!(host.label, "renamed", "{order:?}");
            assert_eq!(host.settings.identity_id, Some(login.id), "{order:?}");
            assert_eq!(host.forward_rule_ids, vec![rule.id], "{order:?}");
            assert_eq!(ident.owner, Some(h.id), "{order:?}");
            assert_eq!(ident.password.unwrap().expose_secret(), "pw-own");
            assert_eq!(r, rule, "{order:?}");
            assert!(dev.vault.outbox(10).unwrap().is_empty(), "{order:?}");
            // Tidy keeps it (it only renames it after the renamed host).
            assert_eq!(dev.vault.tidy_own_logins().unwrap(), 0, "{order:?}");
        }
        w.settle();
        let conflicts_of = |v: &VaultService| {
            let mut c: Vec<(Id, bool)> = v
                .list_conflicts()
                .unwrap()
                .into_iter()
                .map(|c| (c.record_id, c.other.is_none()))
                .collect();
            c.sort();
            c
        };
        for dev in [&w.a, &w.b] {
            // The host's deletion, plus (R27) the cascaded deletions a
            // device refused because the host still links them.
            let conflicts = conflicts_of(&dev.vault);
            let ident = dev.vault.get::<Identity>(login.id).unwrap().unwrap();
            // Tidy renames it after its host, but leaves alone a login with
            // an open conflict.
            let label = if conflicts.iter().any(|(id, _)| *id == login.id) {
                "web"
            } else {
                "renamed"
            };
            assert_eq!(ident.label, label, "{order:?}");
            assert!(
                conflicts.contains(&(h.id, true)),
                "{order:?}: {conflicts:?}"
            );
            assert!(
                conflicts
                    .iter()
                    .all(|(id, deleted)| *deleted && [h.id, login.id, rule.id].contains(id)),
                "{order:?}: {conflicts:?}"
            );
        }
        assert_eq!(conflicts_of(&w.a.vault), conflicts_of(&w.b.vault));
        let (ha, ia, ra) = state(&w.a.vault);
        let (hb, ib, rb) = state(&w.b.vault);
        assert_eq!((ha, ra), (hb, rb), "{order:?}");
        assert_eq!((ia.username, ia.label), (ib.username, ib.label));
    }
}

#[test]
fn own_login_edit_vs_host_delete_keeps_password_as_keychain_identity() {
    for order in ORDERS {
        let mut w = world();
        let (h, login, rule) = host_with_login_and_rule(&mut w);
        app_delete_host(&mut w.a.vault, h.id);
        tick();
        // B changes only the login's password (the host is untouched).
        let mut changed = w.b.vault.get::<Identity>(login.id).unwrap().unwrap();
        changed.password = Some(SecretString::from("pw-new"));
        w.b.vault.upsert(changed).unwrap();
        deliver(&mut w, order);

        for dev in [&mut w.a, &mut w.b] {
            assert!(dev.vault.get::<Host>(h.id).unwrap().is_none(), "{order:?}");
            assert!(dev.vault.get::<ForwardRule>(rule.id).unwrap().is_none());
            let kept = dev
                .vault
                .get::<Identity>(login.id)
                .unwrap()
                .unwrap_or_else(|| panic!("{order:?}: password lost"));
            assert_eq!(kept.owner, None, "{order:?}: now a Keychain identity");
            assert_eq!(kept.label, login.label);
            assert_eq!(kept.password.unwrap().expose_secret(), "pw-new");
            assert_eq!(dev.vault.tidy_own_logins().unwrap(), 0, "{order:?}");
            assert!(dev.vault.get::<Identity>(login.id).unwrap().is_some());
            assert!(dev.vault.outbox(10).unwrap().is_empty(), "{order:?}");
        }
    }
}

#[test]
fn tidy_skips_identity_with_open_conflict() {
    let mut w = world();
    let (h, login, _rule) = host_with_login_and_rule(&mut w);
    for (dev, pw) in [(&mut w.a, "pw-a"), (&mut w.b, "pw-b")] {
        let mut i = dev.vault.get::<Identity>(login.id).unwrap().unwrap();
        i.password = Some(SecretString::from(pw));
        dev.vault.upsert(i).unwrap();
        tick();
    }
    w.server.sync(&mut w.a);
    assert_eq!(w.server.sync(&mut w.b).conflicts, 1);

    // The host stops using its own login: normally tidy removes it.
    let mut unlinked = w.b.vault.get::<Host>(h.id).unwrap().unwrap();
    unlinked.settings.identity_id = None;
    w.b.vault.upsert(unlinked).unwrap();
    assert_eq!(w.b.vault.tidy_own_logins().unwrap(), 0, "open conflict");
    assert!(w.b.vault.get::<Identity>(login.id).unwrap().is_some());

    let c = w.b.vault.list_conflicts().unwrap().remove(0);
    w.b.vault
        .resolve_conflict(c.id, Resolution::KeepCurrent)
        .unwrap();
    assert_eq!(w.b.vault.tidy_own_logins().unwrap(), 1);
}

#[test]
fn deleting_a_rule_or_switching_login_still_syncs() {
    // Dependent tombstones whose owner no longer links them go through.
    let mut w = world();
    let (h, login, rule) = host_with_login_and_rule(&mut w);
    w.a.vault.delete::<ForwardRule>(rule.id).unwrap();
    let mut switched = w.a.vault.get::<Host>(h.id).unwrap().unwrap();
    switched.settings.identity_id = None;
    w.a.vault.upsert(switched).unwrap();
    assert_eq!(w.a.vault.tidy_own_logins().unwrap(), 1);
    w.settle();
    w.settle();
    for dev in [&w.a, &w.b] {
        assert!(dev.vault.get::<ForwardRule>(rule.id).unwrap().is_none());
        assert!(dev.vault.get::<Identity>(login.id).unwrap().is_none());
        let host = dev.vault.get::<Host>(h.id).unwrap().unwrap();
        assert!(host.forward_rule_ids.is_empty());
        assert!(dev.vault.list_conflicts().unwrap().is_empty());
        assert!(dev.vault.outbox(10).unwrap().is_empty());
    }
}

#[test]
fn plain_host_delete_cascades_even_across_apply_batches() {
    let mut w = world();
    let (h, login, rule) = host_with_login_and_rule(&mut w);
    // 199 earlier rows, so the rule's tombstone ends batch 1 (200 rows) and
    // the host's tombstone starts batch 2.
    let tags: Vec<core_model::Tag> = (0..199)
        .map(|n| core_model::Tag {
            id: Id::now_v7(),
            name: format!("t{n}"),
            color: "red".into(),
            extra: Default::default(),
        })
        .collect();
    w.a.vault.upsert_all(tags).unwrap();
    tick();
    app_delete_host(&mut w.a.vault, h.id);
    w.server.sync(&mut w.a);
    w.server.sync(&mut w.b);
    w.settle();
    for dev in [&w.a, &w.b] {
        assert!(dev.vault.get::<Host>(h.id).unwrap().is_none());
        assert!(dev.vault.get::<ForwardRule>(rule.id).unwrap().is_none());
        assert!(dev.vault.get::<Identity>(login.id).unwrap().is_none());
        assert!(dev.vault.list_conflicts().unwrap().is_empty());
        assert!(dev.vault.outbox(10).unwrap().is_empty());
    }
}

/// The deletions recorded as conflicts about `id` (R27: losing side deleted).
fn deletion_conflicts(v: &VaultService, id: Id) -> usize {
    v.list_conflicts()
        .unwrap()
        .iter()
        .filter(|c| c.record_id == id && c.other.is_none())
        .count()
}

#[test]
fn rule_delete_vs_host_edit_records_deletion_conflict() {
    for order in ORDERS {
        let mut w = world();
        let (h, _login, rule) = host_with_login_and_rule(&mut w);
        // A deletes the rule (its host stops listing it); B, later and from
        // a copy that still lists it, renames the host. B's host wins.
        w.a.vault.delete::<ForwardRule>(rule.id).unwrap();
        w.a.vault.tidy_own_logins().unwrap();
        tick();
        let mut renamed = w.b.vault.get::<Host>(h.id).unwrap().unwrap();
        renamed.label = "renamed".into();
        w.b.vault.upsert(renamed).unwrap();
        deliver(&mut w, order);

        for dev in [&w.a, &w.b] {
            let host = dev.vault.get::<Host>(h.id).unwrap().unwrap();
            assert_eq!(host.label, "renamed", "{order:?}");
            assert_eq!(host.forward_rule_ids, vec![rule.id], "{order:?}");
            assert_eq!(
                dev.vault.get::<ForwardRule>(rule.id).unwrap(),
                Some(rule.clone()),
                "{order:?}: kept for the winning host"
            );
            assert_eq!(
                deletion_conflicts(&dev.vault, rule.id),
                1,
                "{order:?}: the deletion is not lost"
            );
            assert!(dev.vault.outbox(10).unwrap().is_empty(), "{order:?}");
        }

        // The user deletes it again: now it goes everywhere.
        w.b.vault.delete::<ForwardRule>(rule.id).unwrap();
        w.settle();
        w.settle();
        for dev in [&w.a, &w.b] {
            assert!(dev.vault.get::<ForwardRule>(rule.id).unwrap().is_none());
            let host = dev.vault.get::<Host>(h.id).unwrap().unwrap();
            assert!(host.forward_rule_ids.is_empty(), "{order:?}");
        }
    }
}

/// B deletes the host; A renames it and syncs; B syncs and loses the delete
/// (`pull_first`: through a pulled row, else through its push's conflict),
/// while its cascade tombstones may already be on the server. Then A, not
/// synced since, deletes the host as well, and B renames the restored host,
/// so no device still holds the dependents from before.
fn delete_lost_twice(w: &mut World, h: Id, pull_first: bool) {
    app_delete_host(&mut w.b.vault, h);
    tick();
    let mut renamed = w.a.vault.get::<Host>(h).unwrap().unwrap();
    renamed.label = "from-a".into();
    w.a.vault.upsert(renamed).unwrap();
    w.server.sync(&mut w.a);
    if pull_first {
        w.server.sync(&mut w.b);
    } else {
        for _ in 0..3 {
            w.server.push(&mut w.b);
        }
    }
    tick();
    app_delete_host(&mut w.a.vault, h);
    tick();
    let mut renamed = w.b.vault.get::<Host>(h).unwrap().expect("restored on B");
    renamed.label = "from-b".into();
    w.b.vault.upsert(renamed).unwrap();
    w.b.vault.tidy_own_logins().unwrap();
    w.settle();
    w.settle();
}

#[test]
fn host_delete_loses_then_cascade_is_revived() {
    for pull_first in [true, false] {
        let mut w = world();
        let (h, login, rule) = host_with_login_and_rule(&mut w);
        delete_lost_twice(&mut w, h.id, pull_first);
        for dev in [&w.a, &w.b] {
            let host = dev.vault.get::<Host>(h.id).unwrap().expect("host restored");
            assert_eq!(host.label, "from-b", "{pull_first}");
            assert_eq!(host.forward_rule_ids, vec![rule.id], "{pull_first}");
            assert_eq!(
                dev.vault.get::<ForwardRule>(rule.id).unwrap(),
                Some(rule.clone()),
                "{pull_first}: the rule came back with its host"
            );
            assert!(dev.vault.get::<Identity>(login.id).unwrap().is_some());
            assert!(dev.vault.outbox(10).unwrap().is_empty(), "{pull_first}");
            // Both of the host's deletions are kept as conflicts.
            assert!(deletion_conflicts(&dev.vault, h.id) >= 1, "{pull_first}");
        }
        // Revived: the stash is gone.
        let key = format!("cascade/{}/{}", h.id, rule.id);
        assert_eq!(w.b.vault.sync_get(&key).unwrap(), None);
    }
}

#[test]
fn own_login_survives_lost_host_delete() {
    for pull_first in [true, false] {
        let mut w = world();
        let (h, login, _rule) = host_with_login_and_rule(&mut w);
        delete_lost_twice(&mut w, h.id, pull_first);
        for dev in [&mut w.a, &mut w.b] {
            let host = dev.vault.get::<Host>(h.id).unwrap().unwrap();
            assert_eq!(host.settings.identity_id, Some(login.id), "{pull_first}");
            let kept = dev
                .vault
                .get::<Identity>(login.id)
                .unwrap()
                .unwrap_or_else(|| panic!("{pull_first}: the saved password is lost"));
            // Still the host's own login, or adopted into the Keychain by a
            // device that saw it come back while its own delete of the host
            // still stood (rule 6); either way the password is kept.
            assert!(
                kept.owner.is_none_or(|o| o == h.id),
                "{pull_first}: {:?}",
                kept.owner
            );
            assert_eq!(kept.password.unwrap().expose_secret(), "pw-own");
            assert_eq!(dev.vault.tidy_own_logins().unwrap(), 0, "{pull_first}");
            assert!(dev.vault.get::<Identity>(login.id).unwrap().is_some());
        }
    }
}

#[test]
fn cascade_stash_is_kept_until_the_delete_is_accepted() {
    let mut w = world();
    let (h, login, rule) = host_with_login_and_rule(&mut w);
    let key = |dep: Id| format!("cascade/{}/{dep}", h.id);
    app_delete_host(&mut w.a.vault, h.id);
    assert!(w.a.vault.sync_get(&key(rule.id)).unwrap().is_some());
    assert!(w.a.vault.sync_get(&key(login.id)).unwrap().is_some());
    w.server.sync(&mut w.a);
    assert_eq!(w.a.vault.sync_get(&key(rule.id)).unwrap(), None, "accepted");
    assert_eq!(w.a.vault.sync_get(&key(login.id)).unwrap(), None);

    // Without sync nothing is stashed.
    let mut w = world();
    let (h, _login, rule) = host_with_login_and_rule(&mut w);
    w.a.vault.sync_delete("enabled").unwrap();
    app_delete_host(&mut w.a.vault, h.id);
    let key = format!("cascade/{}/{}", h.id, rule.id);
    assert_eq!(w.a.vault.sync_get(&key).unwrap(), None);
}

#[test]
fn revive_skips_dependent_deleted_after_owner_edit() {
    // Seed 253's shape: A deletes the host (its rule goes into A's stash);
    // B edits the host, then deletes the rule (the host stops listing it).
    // A's delete loses to B's edit, but the rule must stay deleted.
    for order in ORDERS {
        let mut w = world();
        let (h, login, rule) = host_with_login_and_rule(&mut w);
        app_delete_host(&mut w.a.vault, h.id);
        tick();
        let mut renamed = w.b.vault.get::<Host>(h.id).unwrap().unwrap();
        renamed.label = "renamed".into();
        w.b.vault.upsert(renamed).unwrap();
        tick();
        w.b.vault.delete::<ForwardRule>(rule.id).unwrap();
        w.b.vault.tidy_own_logins().unwrap();
        deliver(&mut w, order);

        for dev in [&w.a, &w.b] {
            let host = dev.vault.get::<Host>(h.id).unwrap().expect("host restored");
            assert_eq!(host.label, "renamed", "{order:?}");
            assert!(host.forward_rule_ids.is_empty(), "{order:?}");
            assert!(
                dev.vault.get::<ForwardRule>(rule.id).unwrap().is_none(),
                "{order:?}: B's delete of the rule stands"
            );
            assert!(
                dev.vault.get::<Identity>(login.id).unwrap().is_some(),
                "{order:?}: the login the host still links comes back"
            );
            assert!(dev.vault.outbox(10).unwrap().is_empty(), "{order:?}");
        }
        let key = format!("cascade/{}/{}", h.id, rule.id);
        assert_eq!(w.a.vault.sync_get(&key).unwrap(), None, "{order:?}");
    }
}

#[test]
fn revive_skips_dependent_deleted_after_pull() {
    // R31: B deletes the host (its rule is stashed); A edits the rule and
    // syncs; B pulls and applies (the edit beats B's cascade delete, so the
    // rule is live again on B at A's port) but does not push; B's user then
    // deletes the rule. A renames the host and syncs; B syncs and its host
    // delete loses. The rule must stay deleted: reviving the stash would
    // undo B's delete and bring back the content from before A's edit.
    // (A's host still lists the rule, so rule 6 keeps A's version and
    // records B's delete as a conflict.)
    let mut w = world();
    let (h, _login, rule) = host_with_login_and_rule(&mut w);
    app_delete_host(&mut w.b.vault, h.id);
    tick();
    let mut edited = w.a.vault.get::<ForwardRule>(rule.id).unwrap().unwrap();
    edited.bind_port = 6000;
    w.a.vault.upsert(edited).unwrap();
    w.server.sync(&mut w.a);
    w.server.pull(&mut w.b);
    w.b.vault.apply_inbound().unwrap();
    let on_b = w.b.vault.get::<ForwardRule>(rule.id).unwrap().unwrap();
    assert_eq!(on_b.bind_port, 6000, "A's edit beat B's cascade delete");
    tick();
    w.b.vault.delete::<ForwardRule>(rule.id).unwrap();
    w.b.vault.tidy_own_logins().unwrap();
    tick();
    let mut renamed = w.a.vault.get::<Host>(h.id).unwrap().unwrap();
    renamed.label = "renamed".into();
    w.a.vault.upsert(renamed).unwrap();
    w.server.sync(&mut w.a);
    w.server.sync(&mut w.b);
    w.settle();
    w.settle();

    for dev in [&w.a, &w.b] {
        let host = dev.vault.get::<Host>(h.id).unwrap().expect("host restored");
        assert_eq!(host.label, "renamed");
        let kept = dev.vault.get::<ForwardRule>(rule.id).unwrap();
        assert_eq!(
            kept.map(|r| r.bind_port),
            Some(6000),
            "A's edit survives, never the stashed copy from before it"
        );
        let deletions = dev
            .vault
            .list_conflicts()
            .unwrap()
            .into_iter()
            .filter(|c| c.record_id == rule.id && c.other.is_none())
            .count();
        assert_eq!(deletions, 2, "B's cascade and B's own delete are both kept");
        assert!(dev.vault.outbox(10).unwrap().is_empty());
    }
    let key = format!("cascade/{}/{}", h.id, rule.id);
    assert_eq!(w.b.vault.sync_get(&key).unwrap(), None);
}

#[test]
fn revive_skips_when_newer_dependent_is_queued() {
    // R32: B deletes the host offline (rule and login stashed). A edits the
    // rule and the login's password and renames the host, then syncs. B's
    // pull holds all three; the host applies first (owners first) and loses
    // B's delete, while A's newer rule and login are still queued: they must
    // win, not the stashed copies from before A's edits.
    for edit_login in [false, true] {
        let mut w = world();
        let (h, login, rule) = host_with_login_and_rule(&mut w);
        app_delete_host(&mut w.b.vault, h.id);
        tick();
        if edit_login {
            w.a.vault
                .set_own_login(
                    h.id,
                    "deploy".into(),
                    core_api::vault::PasswordChange::Set(SecretString::from("pw-newer")),
                )
                .unwrap();
        } else {
            let mut edited = w.a.vault.get::<ForwardRule>(rule.id).unwrap().unwrap();
            edited.bind_port = 6000;
            w.a.vault.upsert(edited).unwrap();
        }
        let mut renamed = w.a.vault.get::<Host>(h.id).unwrap().unwrap();
        renamed.label = "renamed".into();
        w.a.vault.upsert(renamed).unwrap();
        w.server.sync(&mut w.a);
        w.server.sync(&mut w.b);
        w.settle();
        w.settle();

        for dev in [&w.a, &w.b] {
            let host = dev.vault.get::<Host>(h.id).unwrap().expect("host restored");
            assert_eq!(host.label, "renamed", "{edit_login}");
            let r = dev
                .vault
                .get::<ForwardRule>(rule.id)
                .unwrap()
                .expect("rule");
            assert_eq!(r.bind_port, if edit_login { 5432 } else { 6000 });
            let l = dev.vault.get::<Identity>(login.id).unwrap().expect("login");
            let pw = if edit_login { "pw-newer" } else { "pw-own" };
            assert_eq!(l.password.unwrap().expose_secret(), pw, "{edit_login}");
            // A's newer versions never became a conflict's losing side.
            for c in dev.vault.list_conflicts().unwrap() {
                if c.record_id == rule.id || c.record_id == login.id {
                    assert!(c.other.is_none(), "{edit_login}: {c:?}");
                }
            }
            assert!(dev.vault.outbox(10).unwrap().is_empty(), "{edit_login}");
        }
    }
}

#[test]
fn stale_own_login_not_revived_over_push_conflict() {
    // R33: B deletes the host offline (tidy tombstones the own login after
    // the host). A changes the login's password and renames the host, then
    // syncs. B pushes before pulling: the host's result restores the host,
    // and the login's own result in the same push brings A's newer login.
    // The revive runs after both, so A's password stands.
    let mut w = world();
    let (h, login, rule) = host_with_login_and_rule(&mut w);
    app_delete_host(&mut w.b.vault, h.id);
    tick();
    w.a.vault
        .set_own_login(
            h.id,
            "deploy".into(),
            core_api::vault::PasswordChange::Set(SecretString::from("pw-newer")),
        )
        .unwrap();
    let mut renamed = w.a.vault.get::<Host>(h.id).unwrap().unwrap();
    renamed.label = "renamed".into();
    w.a.vault.upsert(renamed).unwrap();
    w.server.sync(&mut w.a);
    for _ in 0..3 {
        w.server.push(&mut w.b);
    }
    w.settle();
    w.settle();
    for dev in [&w.a, &w.b] {
        let host = dev.vault.get::<Host>(h.id).unwrap().expect("host restored");
        assert_eq!(host.label, "renamed");
        let l = dev.vault.get::<Identity>(login.id).unwrap().expect("login");
        assert_eq!(l.password.unwrap().expose_secret(), "pw-newer");
        assert!(dev.vault.get::<ForwardRule>(rule.id).unwrap().is_some());
        for c in dev.vault.list_conflicts().unwrap() {
            if c.record_id == login.id {
                assert!(c.other.is_none(), "A's password lost to a conflict: {c:?}");
            }
        }
        assert!(dev.vault.outbox(10).unwrap().is_empty());
    }
}

#[test]
fn revive_survives_a_crash_between_batch_and_revive() {
    // R33: the restored owner is recorded in the batch's transactions, so
    // a crash before the revive call loses nothing.
    let mut w = world();
    let (h, login, rule) = host_with_login_and_rule(&mut w);
    app_delete_host(&mut w.b.vault, h.id);
    tick();
    let mut renamed = w.a.vault.get::<Host>(h.id).unwrap().unwrap();
    renamed.label = "renamed".into();
    w.a.vault.upsert(renamed).unwrap();
    w.server.sync(&mut w.a);
    for _ in 0..3 {
        w.server.push_results(&mut w.b);
    }
    let pending = format!("restored/{}", h.id);
    assert!(w.b.vault.sync_get(&pending).unwrap().is_some());
    assert!(w.b.vault.get::<ForwardRule>(rule.id).unwrap().is_none());

    // Crash: the vault is reopened from its file.
    w.b.vault = VaultService::open_for_tests(
        &w._dir.path().join("b.sqlite"),
        Box::new(MemoryKeyStore::default()),
    )
    .unwrap();
    w.b.vault.unlock(pw()).unwrap();
    assert_eq!(w.b.vault.revive_restored_owners().unwrap(), 2);
    assert_eq!(w.b.vault.sync_get(&pending).unwrap(), None);
    w.settle();
    w.settle();
    for dev in [&w.a, &w.b] {
        assert_eq!(
            dev.vault.get::<ForwardRule>(rule.id).unwrap(),
            Some(rule.clone())
        );
        let l = dev.vault.get::<Identity>(login.id).unwrap().expect("login");
        assert_eq!(l.password.unwrap().expose_secret(), "pw-own");
        assert!(dev.vault.outbox(10).unwrap().is_empty());
    }
}

#[test]
fn rule6_does_not_keep_stale_version_over_newer_queued_row() {
    // R34: B's queue holds a dependent's tombstone (A deleted the host) and,
    // later, a newer version of it (C's edit beat that delete and restored
    // the host). Rule 6 must not keep B's older copy against the tombstone:
    // re-stamped, it would beat C's newer version.
    for edit_login in [false, true] {
        let mut w = world();
        let (h, login, rule) = host_with_login_and_rule(&mut w);
        let c_vault =
            w.a.vault
                .clone_for_device(&w._dir.path().join("c.sqlite"), 3)
                .unwrap();
        let mut c = Device {
            vault: c_vault,
            cursor: 0,
        };
        c.vault.unlock(pw()).unwrap();
        c.vault.sync_put("enabled", b"1").unwrap();
        w.server.sync(&mut c);

        // C edits offline: the dependent, and the host.
        if edit_login {
            c.vault
                .set_own_login(
                    h.id,
                    "deploy".into(),
                    core_api::vault::PasswordChange::Set(SecretString::from("pw-from-c")),
                )
                .unwrap();
        } else {
            let mut r = c.vault.get::<ForwardRule>(rule.id).unwrap().unwrap();
            r.bind_port = 7000;
            c.vault.upsert(r).unwrap();
        }
        let mut renamed = c.vault.get::<Host>(h.id).unwrap().unwrap();
        renamed.label = "from-c".into();
        c.vault.upsert(renamed).unwrap();
        tick();
        // A deletes the host and syncs; B queues the tombstones unapplied.
        app_delete_host(&mut w.a.vault, h.id);
        w.server.sync(&mut w.a);
        w.server.pull(&mut w.b);
        // C syncs: its edits beat A's delete. B queues those too, then
        // applies everything at once.
        w.server.sync(&mut c);
        w.server.sync(&mut w.b);
        w.settle();
        w.server.sync(&mut c);
        w.settle();

        for dev in [&w.a, &w.b, &c] {
            let host = dev.vault.get::<Host>(h.id).unwrap().expect("host restored");
            assert_eq!(host.label, "from-c", "{edit_login}");
            let r = dev
                .vault
                .get::<ForwardRule>(rule.id)
                .unwrap()
                .expect("rule");
            assert_eq!(
                r.bind_port,
                if edit_login { 5432 } else { 7000 },
                "{edit_login}"
            );
            let l = dev.vault.get::<Identity>(login.id).unwrap().expect("login");
            let pw = if edit_login { "pw-from-c" } else { "pw-own" };
            assert_eq!(l.password.unwrap().expose_secret(), pw, "{edit_login}");
            for conflict in dev.vault.list_conflicts().unwrap() {
                if conflict.record_id == rule.id || conflict.record_id == login.id {
                    assert!(conflict.other.is_none(), "{edit_login}: {conflict:?}");
                }
            }
        }
    }
}
