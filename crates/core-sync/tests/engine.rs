//! The sync engine against an in-process TLS server: live updates over the
//! WebSocket, offline merges, locked devices, crash safety, full resync after
//! a server restore, large uploads, and the error statuses.

mod common;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::*;
use core_model::{Host, Id, SshKey};
use core_sync::{Engine, SyncConfig, SyncErrorKind, SyncNotice, SyncStatus, TickOutcome};
use secrecy::SecretString;
use tether_server::testing::spawn_tls;

#[tokio::test(flavor = "multi_thread")]
async fn edit_on_a_appears_on_b_within_two_seconds() {
    let srv = spawn_tls().await;
    let dir = tempfile::tempdir().unwrap();
    let mut a = local_device(dir.path(), "a");
    enable(&srv, &mut a).await;
    let mut b = join(&srv, dir.path(), "b", &a).await;
    // Let B's event socket connect.
    tokio::time::sleep(Duration::from_millis(300)).await;

    let h = host("live");
    let id = h.id;
    a.with(|v| v.upsert(h).unwrap());
    let start = Instant::now();
    a.handle().notify_local_change();
    eventually("B sees the edit", || {
        b.with(|v| v.get::<Host>(id).unwrap().is_some())
    })
    .await;
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "{:?}",
        start.elapsed()
    );

    // And back, an edit to the same record.
    b.with(|v| {
        let mut h = v.get::<Host>(id).unwrap().unwrap();
        h.label = "edited on b".into();
        v.upsert(h).unwrap();
    });
    b.handle().notify_local_change();
    eventually("A sees B's edit", || {
        a.with(|v| v.get::<Host>(id).unwrap().unwrap().label == "edited on b")
    })
    .await;
    a.stop().await;
    b.stop().await;
}

/// Lists reload on `RecordsChanged`: it comes when another device's change
/// is pulled in, and not for the echo of this device's own push.
#[tokio::test(flavor = "multi_thread")]
async fn pulled_changes_notify_but_own_echoes_do_not() {
    let srv = spawn_tls().await;
    let dir = tempfile::tempdir().unwrap();
    let mut a = local_device(dir.path(), "a");
    enable(&srv, &mut a).await;
    let mut b = join(&srv, dir.path(), "b", &a).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let mut notices = a.handle().notices();

    let h = host("from b");
    let id = h.id;
    b.with(|v| v.upsert(h).unwrap());
    b.handle().notify_local_change();
    let notice = tokio::time::timeout(WAIT, notices.recv())
        .await
        .expect("no notice for B's new host")
        .unwrap();
    assert_eq!(notice, SyncNotice::RecordsChanged);
    assert!(a.with(|v| v.get::<Host>(id).unwrap().is_some()));

    // The same for an edit of an existing record.
    a.settle().await;
    while notices.try_recv().is_ok() {}
    b.with(|v| {
        let mut h = v.get::<Host>(id).unwrap().unwrap();
        h.label = "edited on b".into();
        v.upsert(h).unwrap();
    });
    b.handle().notify_local_change();
    let notice = tokio::time::timeout(WAIT, notices.recv())
        .await
        .expect("no notice for B's edit")
        .unwrap();
    assert_eq!(notice, SyncNotice::RecordsChanged);

    // A's own edit is pushed, then comes back as an echo: no notice, however
    // many cycles run.
    a.settle().await;
    while notices.try_recv().is_ok() {}
    a.with(|v| {
        let mut h = v.get::<Host>(id).unwrap().unwrap();
        h.label = "edited on a".into();
        v.upsert(h).unwrap();
    });
    a.settle().await;
    a.settle().await;
    a.settle().await;
    assert_eq!(
        notices.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty),
        "an echo of A's own push changes nothing"
    );
    a.stop().await;
    b.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn offline_edits_merge_with_conflict() {
    let srv = spawn_tls().await;
    let dir = tempfile::tempdir().unwrap();
    let mut a = local_device(dir.path(), "a");
    let h = host("shared");
    let id = h.id;
    a.with(|v| v.upsert(h).unwrap());
    enable(&srv, &mut a).await;
    let mut b = join(&srv, dir.path(), "b", &a).await;
    a.stop().await;
    b.stop().await;

    for (dev, label) in [(&a, "from a"), (&b, "from b")] {
        dev.with(|v| {
            let mut h = v.get::<Host>(id).unwrap().unwrap();
            h.label = label.into();
            v.upsert(h).unwrap();
        });
        // B's edit must be later by the clock, not by a node-id tie-break.
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    a.start();
    a.settle().await;
    b.start();
    b.settle().await;
    a.settle().await;

    let label_a = a.with(|v| v.get::<Host>(id).unwrap().unwrap().label);
    let label_b = b.with(|v| v.get::<Host>(id).unwrap().unwrap().label);
    assert_eq!(label_a, label_b, "both converge");
    assert_eq!(label_a, "from b", "the later edit wins");
    for dev in [&a, &b] {
        let conflicts = dev.with(|v| v.list_conflicts().unwrap());
        assert_eq!(conflicts.len(), 1, "the losing version is kept");
        assert_eq!(conflicts[0].record_id, id);
    }
    a.stop().await;
    b.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn locked_device_queues_then_applies_on_unlock() {
    let srv = spawn_tls().await;
    let dir = tempfile::tempdir().unwrap();
    let mut a = local_device(dir.path(), "a");
    enable(&srv, &mut a).await;
    let mut b = join(&srv, dir.path(), "b", &a).await;
    b.with(|v| v.lock());
    tokio::time::sleep(Duration::from_millis(300)).await;

    let h = host("while locked");
    let id = h.id;
    a.with(|v| v.upsert(h).unwrap());
    a.handle().notify_local_change();
    eventually("B queues the row while locked", || {
        b.with(|v| v.inbound_len().unwrap()) == 1
    })
    .await;

    b.with(|v| v.unlock(pw(PW)).unwrap());
    b.handle().notify_unlocked();
    eventually("B applies on unlock", || {
        b.with(|v| v.get::<Host>(id).unwrap().is_some())
    })
    .await;
    assert_eq!(b.with(|v| v.inbound_len().unwrap()), 0);
    a.stop().await;
    b.stop().await;
}

async fn run_until_idle(engine: &mut Engine) -> TickOutcome {
    for _ in 0..10_000 {
        match engine.tick().await {
            TickOutcome::Progress => continue,
            other => return other,
        }
    }
    panic!("engine did not settle");
}

#[tokio::test(flavor = "multi_thread")]
async fn engine_recovers_after_crash_mid_push() {
    let srv = spawn_tls().await;
    let dir = tempfile::tempdir().unwrap();
    let mut a = local_device(dir.path(), "a");
    enable(&srv, &mut a).await;
    a.stop().await;

    let mut engine = Engine::new(a.vault.clone(), a.tokens())
        .unwrap()
        .expect("sync on");
    engine.request_sync();
    assert_eq!(run_until_idle(&mut engine).await, TickOutcome::Idle);

    let h = host("pushed, never acked");
    let id = h.id;
    a.with(|v| v.upsert(h).unwrap());
    engine.crash_before_next_ack();
    engine.request_sync();
    assert_eq!(run_until_idle(&mut engine).await, TickOutcome::Stopped);
    drop(engine);
    // The server has it; locally it is still unpushed.
    assert_eq!(a.with(|v| v.outbox_len().unwrap()), 1);

    let mut engine = Engine::new(a.vault.clone(), a.tokens()).unwrap().unwrap();
    engine.request_sync();
    assert_eq!(run_until_idle(&mut engine).await, TickOutcome::Idle);
    assert_eq!(a.with(|v| v.outbox_len().unwrap()), 0, "row clean");
    assert!(
        a.with(|v| v.list_conflicts().unwrap()).is_empty(),
        "no conflict"
    );
    assert!(a.with(|v| v.get::<Host>(id).unwrap()).is_some());
    let status = engine.status().borrow().clone();
    assert!(matches!(status, SyncStatus::Idle { .. }), "{status:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn older_server_state_triggers_resync_and_repush() {
    let srv = spawn_tls().await;
    let dir = tempfile::tempdir().unwrap();
    let mut a = local_device(dir.path(), "a");
    let h1 = host("h1");
    a.with(|v| v.upsert(h1.clone()).unwrap());
    enable(&srv, &mut a).await;
    let mut b = join(&srv, dir.path(), "b", &a).await;
    let backup = dir.path().join("server.backup");
    srv.backup_to(&backup).await.unwrap();

    // After the backup: A edits h1 and adds h2, B adds h3; everyone syncs.
    let h2 = host("h2");
    a.with(|v| {
        let mut h = v.get::<Host>(h1.id).unwrap().unwrap();
        h.label = "h1 edited".into();
        v.upsert(h).unwrap();
        v.upsert(h2.clone()).unwrap();
    });
    a.settle().await;
    let h3 = host("h3");
    b.with(|v| v.upsert(h3.clone()).unwrap());
    b.settle().await;
    a.settle().await;
    assert_eq!(a.hosts().len(), 3);
    a.stop().await;
    b.stop().await;

    // The server is restored from the older backup (new epoch; the test
    // server also comes back on a new port).
    let srv = srv.restore_from(&backup).await.unwrap();
    for dev in [&a, &b] {
        dev.with(|v| {
            let mut cfg = SyncConfig::load(v).unwrap().unwrap();
            cfg.url = srv.url.clone();
            cfg.save(v).unwrap();
        });
    }
    a.start();
    a.settle().await;
    b.start();
    b.settle().await;
    a.settle().await;

    let expect = |dev: &Dev| {
        let mut labels: Vec<String> = dev.hosts().into_iter().map(|h| h.label).collect();
        labels.sort();
        labels
    };
    assert_eq!(expect(&a), vec!["h1 edited", "h2", "h3"]);
    assert_eq!(expect(&b), vec!["h1 edited", "h2", "h3"]);
    for dev in [&a, &b] {
        assert!(dev.with(|v| v.list_conflicts().unwrap()).is_empty());
        assert_eq!(dev.with(|v| v.outbox_len().unwrap()), 0);
    }
    let on_server: Vec<Id> = server_rows(&a)
        .await
        .into_iter()
        .map(|r| r.0.parse().unwrap())
        .collect();
    assert_eq!(on_server.len(), 3);
    for id in [h1.id, h2.id, h3.id] {
        assert!(on_server.contains(&id));
    }
    a.stop().await;
    b.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn enable_sync_uploads_large_vault_in_batches() {
    let srv = spawn_tls().await;
    let dir = tempfile::tempdir().unwrap();
    let mut a = local_device(dir.path(), "a");
    let big = "k".repeat(8 * 1024);
    a.with(|v| {
        v.upsert_all((0..2000).map(|i| SshKey {
            id: Id::now_v7(),
            label: format!("key {i}"),
            algorithm: "ssh-ed25519".into(),
            private_openssh: SecretString::from(big.clone()),
            passphrase: None,
            public_openssh: "ssh-ed25519 AAAA".into(),
            certificate: None,
            extra: Default::default(),
        }))
        .unwrap()
    });

    let code = core_sync::parse_join_code(&srv.make_invite().await).unwrap();
    core_sync::flows::enable_with_invite(
        &a.vault,
        a.tokens(),
        &code,
        USER,
        pw(PW),
        core_sync::flows::RecoveryChoice::Existing(SecretString::from(a.recovery.clone())),
    )
    .await
    .unwrap();
    a.start();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let mut rx = a.handle().status();
    let log = seen.clone();
    let watcher = tokio::spawn(async move {
        while rx.changed().await.is_ok() {
            let s = rx.borrow_and_update().clone();
            log.lock().unwrap().push(s);
        }
    });
    let end = tokio::time::timeout(Duration::from_secs(120), a.handle().synced())
        .await
        .unwrap();
    assert!(matches!(end, SyncStatus::Idle { .. }), "{end:?}");
    assert_eq!(server_rows(&a).await.len(), 2000);

    let seen = seen.lock().unwrap().clone();
    let pushing: Vec<usize> = seen
        .iter()
        .filter_map(|s| match s {
            SyncStatus::Syncing {
                done,
                total: Some(2000),
            } => Some(*done),
            _ => None,
        })
        .collect();
    assert!(pushing.len() >= 2, "progress is reported: {seen:?}");
    assert!(pushing.windows(2).all(|w| w[0] <= w[1]), "{pushing:?}");
    assert!(
        !seen.iter().any(|s| matches!(s, SyncStatus::Error { .. })),
        "{seen:?}"
    );
    a.stop().await;
    watcher.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn engine_reports_clock_skew() {
    let srv = spawn_tls().await;
    let dir = tempfile::tempdir().unwrap();
    let mut a = local_device(dir.path(), "a");
    enable(&srv, &mut a).await;
    srv.set_clock(tether_server::util::system_now_ms() - 2 * 60 * 60 * 1000);
    a.with(|v| v.upsert(host("from the future")).unwrap());
    let status = a.sync().await;
    match status {
        SyncStatus::Error { kind, message } => {
            assert_eq!(kind, SyncErrorKind::ClockSkew);
            assert_eq!(message, "this device's clock is wrong");
        }
        other => panic!("{other:?}"),
    }
    srv.set_clock(0);
    a.settle().await;
    a.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn revoked_device_stops_with_status() {
    let srv = spawn_tls().await;
    let dir = tempfile::tempdir().unwrap();
    let mut a = local_device(dir.path(), "a");
    enable(&srv, &mut a).await;
    let mut b = join(&srv, dir.path(), "b", &a).await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    a.handle()
        .client()
        .revoke_device(b.device_id())
        .await
        .unwrap();
    let status = status_becomes(&b, |s| matches!(s, SyncStatus::Error { .. })).await;
    assert!(
        matches!(&status, SyncStatus::Error { kind: SyncErrorKind::Revoked, message } if message == "device revoked"),
        "{status:?}"
    );
    eventually("B's engine stops", || b.handle().is_finished()).await;
    // The local vault is untouched and keeps working.
    b.with(|v| v.upsert(host("still local")).unwrap());
    assert_eq!(b.hosts().len(), 1);
    a.stop().await;
    b.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn certificate_changed_status_and_local_vault_still_works() {
    let srv = spawn_tls().await;
    let dir = tempfile::tempdir().unwrap();
    let mut a = local_device(dir.path(), "a");
    enable(&srv, &mut a).await;
    a.stop().await;
    let real = a.with(|v| SyncConfig::load(v).unwrap().unwrap());
    a.with(|v| {
        let mut cfg = real.clone();
        cfg.pin = Some(core_sync::Pin::new([7; 32]));
        cfg.save(v).unwrap();
    });

    a.start();
    let status = status_becomes(&a, |s| matches!(s, SyncStatus::Error { .. })).await;
    assert!(
        matches!(&status, SyncStatus::Error { kind: SyncErrorKind::CertificateChanged, message } if message == "server certificate changed"),
        "{status:?}"
    );
    a.with(|v| v.upsert(host("offline edit")).unwrap());
    assert_eq!(a.hosts().len(), 1);
    // "Sync now" retries, and still refuses the certificate.
    let again = a.sync().await;
    assert!(
        matches!(
            again,
            SyncStatus::Error {
                kind: SyncErrorKind::CertificateChanged,
                ..
            }
        ),
        "{again:?}"
    );
    a.stop().await;

    a.with(|v| real.save(v).unwrap());
    a.start();
    a.settle().await;
    assert_eq!(server_rows(&a).await.len(), 1);
    a.stop().await;
}

// -- fix round 1 ---------------------------------------------------------

fn material_json(dev: &Dev) -> String {
    serde_json::to_string(&dev.with(|v| v.account_material().unwrap())).unwrap()
}

fn partial_message(status: &SyncStatus) -> String {
    match status {
        SyncStatus::Error {
            kind: SyncErrorKind::Partial,
            message,
        } => message.clone(),
        other => panic!("expected a partial-sync status, got {other:?}"),
    }
}

/// A row the server sent that does not parse is skipped (and reported); the
/// cursor moves past it and local edits still go up.
#[tokio::test(flavor = "multi_thread")]
async fn malformed_server_row_is_skipped_and_pushes_continue() {
    let dir = tempfile::tempdir().unwrap();
    let material = Arc::new(Mutex::new(String::new()));
    let device = Arc::new(Mutex::new(Id::nil()));
    let (m, d) = (material.clone(), device.clone());
    let stub = net::stub(move |req| {
        let (m, d) = (m.lock().unwrap().clone(), *d.lock().unwrap());
        match req.route().as_str() {
            "GET /v1/sync/changes" if req.path.contains("since=0") => net::json(
                200,
                r#"{"changes":[{"id":"not-a-uuid","kind":"host","keyVersion":1,"hlc":"garbage","rev":1,"seq":1,"deleted":false,"envelope":"AQID"}],"next":1,"more":false,"epoch":"stub-epoch"}"#,
            ),
            "POST /v1/sync/push" => accept_push(req),
            "GET /v1/events" => net::error(429, "rate_limited"),
            _ => healthy(req, d, &m).unwrap_or_else(|| net::error(404, "not_found")),
        }
    })
    .await;
    let (mut dev, id) = stub_device(dir.path(), &stub.url);
    *material.lock().unwrap() = material_json(&dev);
    *device.lock().unwrap() = id;
    let h = host("local edit");
    dev.with(|v| v.upsert(h.clone()).unwrap());

    dev.start();
    let status = dev.sync().await;
    assert!(
        partial_message(&status).contains("could not be read"),
        "{status:?}"
    );
    assert_eq!(
        pushed_ids(&stub),
        vec![h.id.to_string()],
        "local edits still go up"
    );
    assert_eq!(dev.with(|v| v.outbox_len().unwrap()), 0);
    assert_eq!(
        dev.with(|v| v.sync_get("cursor").unwrap()).as_deref(),
        Some(&b"1"[..])
    );
    // Reported once: the next sync is clean.
    let again = dev.sync().await;
    assert!(matches!(again, SyncStatus::Idle { .. }), "{again:?}");
    dev.stop().await;
}

/// A record whose envelope is over the server's 1 MiB limit is held back and
/// reported; the other records sync.
#[tokio::test(flavor = "multi_thread")]
async fn oversize_record_is_held_back_and_others_sync() {
    let srv = spawn_tls().await;
    let dir = tempfile::tempdir().unwrap();
    let mut a = local_device(dir.path(), "a");
    let huge = SshKey {
        id: Id::now_v7(),
        label: "huge".into(),
        algorithm: "ssh-ed25519".into(),
        private_openssh: SecretString::from("x".repeat(1024 * 1024 + 10)),
        passphrase: None,
        public_openssh: "ssh-ed25519 AAAA".into(),
        certificate: None,
        extra: Default::default(),
    };
    let (h1, h2) = (host("one"), host("two"));
    a.with(|v| {
        v.upsert(huge.clone()).unwrap();
        v.upsert(h1.clone()).unwrap();
        v.upsert(h2.clone()).unwrap();
    });
    let code = core_sync::parse_join_code(&srv.make_invite().await).unwrap();
    core_sync::flows::enable_with_invite(
        &a.vault,
        a.tokens(),
        &code,
        USER,
        pw(PW),
        core_sync::flows::RecoveryChoice::Existing(SecretString::from(a.recovery.clone())),
    )
    .await
    .unwrap();
    a.start();
    let status = a.sync().await;
    assert!(
        partial_message(&status).contains("could not be uploaded"),
        "{status:?}"
    );
    let mut on_server: Vec<String> = server_rows(&a).await.into_iter().map(|r| r.0).collect();
    on_server.sort();
    let mut expect = vec![h1.id.to_string(), h2.id.to_string()];
    expect.sort();
    assert_eq!(on_server, expect);
    assert_eq!(a.with(|v| v.outbox_len().unwrap()), 1, "the big one waits");
    a.stop().await;
}

/// A whole-request 400 is narrowed down to the item that causes it; the
/// rest of the batch syncs.
#[tokio::test(flavor = "multi_thread")]
async fn refused_push_is_bisected_to_the_bad_item() {
    let dir = tempfile::tempdir().unwrap();
    let hosts: Vec<Host> = (0..5).map(|i| host(&format!("h{i}"))).collect();
    let bad = hosts[2].id.to_string();
    let material = Arc::new(Mutex::new(String::new()));
    let device = Arc::new(Mutex::new(Id::nil()));
    let (m, d, b) = (material.clone(), device.clone(), bad.clone());
    let stub = net::stub(move |req| {
        let (m, d) = (m.lock().unwrap().clone(), *d.lock().unwrap());
        match req.route().as_str() {
            "POST /v1/sync/push" => {
                if String::from_utf8_lossy(&req.body).contains(&b) {
                    net::error(400, "bad_request")
                } else {
                    accept_push(req)
                }
            }
            "GET /v1/events" => net::error(429, "rate_limited"),
            _ => healthy(req, d, &m).unwrap_or_else(|| net::error(404, "not_found")),
        }
    })
    .await;
    let (mut dev, id) = stub_device(dir.path(), &stub.url);
    *material.lock().unwrap() = material_json(&dev);
    *device.lock().unwrap() = id;
    dev.with(|v| v.upsert_all(hosts.clone()).unwrap());

    dev.start();
    let status = dev.sync().await;
    assert!(
        partial_message(&status).contains("could not be uploaded"),
        "{status:?}"
    );
    // Rows of the accepted pushes (refused requests are logged too).
    let mut pushed: Vec<String> = stub
        .log
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r.route() == "POST /v1/sync/push")
        .filter(|r| !String::from_utf8_lossy(&r.body).contains(&bad))
        .flat_map(|r| {
            let push: core_proto::PushReq = serde_json::from_slice(&r.body).unwrap();
            push.items.into_iter().map(|i| i.row.id)
        })
        .collect();
    pushed.sort();
    let mut expect: Vec<String> = hosts
        .iter()
        .map(|h| h.id.to_string())
        .filter(|i| *i != bad)
        .collect();
    expect.sort();
    assert_eq!(pushed, expect);
    assert_eq!(dev.with(|v| v.outbox_len().unwrap()), 1);
    dev.stop().await;
}

/// Errors other than the halting ones back off (socket and requests): a
/// server answering 429/500 is not hammered.
#[tokio::test(flavor = "multi_thread")]
async fn failing_server_is_retried_with_backoff() {
    let dir = tempfile::tempdir().unwrap();
    let material = Arc::new(Mutex::new(String::new()));
    let device = Arc::new(Mutex::new(Id::nil()));
    let (m, d) = (material.clone(), device.clone());
    let stub = net::stub(move |req| {
        let (m, d) = (m.lock().unwrap().clone(), *d.lock().unwrap());
        match req.route().as_str() {
            "GET /v1/sync/changes" => net::error(500, "internal"),
            "GET /v1/events" => net::error(429, "rate_limited"),
            _ => healthy(req, d, &m).unwrap_or_else(|| net::error(404, "not_found")),
        }
    })
    .await;
    let (mut dev, id) = stub_device(dir.path(), &stub.url);
    *material.lock().unwrap() = material_json(&dev);
    *device.lock().unwrap() = id;
    dev.start();
    tokio::time::sleep(Duration::from_secs(3)).await;
    let (events, pulls) = (
        stub.count("GET /v1/events"),
        stub.count("GET /v1/sync/changes"),
    );
    assert!(events <= 4, "socket reconnects back off: {events}");
    assert!((1..=4).contains(&pulls), "requests back off: {pulls}");
    // Between retries the status is the error; a retry may be running right
    // now (`Syncing`), so wait for it to end rather than sample once.
    status_becomes(&dev, |s| matches!(s, SyncStatus::Error { .. })).await;
    dev.stop().await;
}

/// A device that was offline while the password changed picks up the new
/// account material when it reconnects, without a restart or "Sync now".
#[tokio::test(flavor = "multi_thread")]
async fn reconnecting_device_fetches_changed_account() {
    let srv = spawn_tls().await;
    let dir = tempfile::tempdir().unwrap();
    let mut a = local_device(dir.path(), "a");
    enable(&srv, &mut a).await;
    let mut c = join(&srv, dir.path(), "c", &a).await;
    c.stop().await;
    let link = net::proxy(srv.addr()).await;
    c.with(|v| {
        let mut cfg = SyncConfig::load(v).unwrap().unwrap();
        cfg.url = link.url.clone();
        cfg.save(v).unwrap();
    });
    c.start();
    c.settle().await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let mut notices = c.handle().notices();

    link.set_down(true);
    tokio::time::sleep(Duration::from_millis(300)).await;
    core_sync::flows::change_password_synced(&a.vault, a.handle(), pw(PW), pw(NEW_PW))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(c.with(|v| v.is_unlocked()), "offline: nothing heard yet");
    link.set_down(false);

    let notice = tokio::time::timeout(WAIT, notices.recv())
        .await
        .expect("no notice")
        .unwrap();
    assert_eq!(notice, core_sync::SyncNotice::PasswordChangedElsewhere);
    c.with(|v| v.unlock(pw(NEW_PW))).unwrap();
    a.stop().await;
    c.stop().await;
}

/// A dropped event socket shows: the device turns Offline within seconds,
/// with no edit or "Sync now", and back to idle once the server returns.
#[tokio::test(flavor = "multi_thread")]
async fn lost_event_socket_shows_offline_without_a_request() {
    let srv = spawn_tls().await;
    let dir = tempfile::tempdir().unwrap();
    let mut a = local_device(dir.path(), "a");
    enable(&srv, &mut a).await;
    a.stop().await;
    let link = net::proxy(srv.addr()).await;
    a.with(|v| {
        let mut cfg = SyncConfig::load(v).unwrap().unwrap();
        cfg.url = link.url.clone();
        cfg.save(v).unwrap();
    });
    a.start();
    a.settle().await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(matches!(
        *a.handle().status().borrow(),
        SyncStatus::Idle { .. }
    ));

    link.set_down(true);
    let start = Instant::now();
    status_becomes(&a, |s| matches!(s, SyncStatus::Offline { .. })).await;
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "{:?}",
        start.elapsed()
    );

    link.set_down(false);
    status_becomes(&a, |s| matches!(s, SyncStatus::Idle { .. })).await;
    a.stop().await;
}

/// An `account_changed` heard while a cycle runs on a locked device is not
/// lost when that cycle ends `Locked` at apply: the next cycle fetches the
/// new material, so the old password stops working without a reconnect.
#[tokio::test(flavor = "multi_thread")]
async fn account_change_heard_mid_cycle_on_a_locked_device_is_fetched() {
    let srv = spawn_tls().await;
    let dir = tempfile::tempdir().unwrap();
    let mut a = local_device(dir.path(), "a");
    enable(&srv, &mut a).await;
    let mut c = join(&srv, dir.path(), "c", &a).await;
    c.stop().await;
    c.with(|v| v.lock());
    let rev = c.with(|v| v.account_rev().unwrap().unwrap());

    let mut engine = Engine::new(c.vault.clone(), c.tokens())
        .unwrap()
        .expect("sync on");
    // Locked: every cycle ends at apply, quietly.
    assert_eq!(run_until_idle(&mut engine).await, TickOutcome::Idle);

    // A record cycle starts (idle → pull).
    engine.on_event(core_proto::Event::Changed { seq: 0 });
    assert_eq!(engine.tick().await, TickOutcome::Progress);
    // The password changes elsewhere while it runs.
    core_sync::flows::change_password_synced(&a.vault, a.handle(), pw(PW), pw(NEW_PW))
        .await
        .unwrap();
    engine.on_event(core_proto::Event::AccountChanged {
        account_rev: rev + 1,
    });
    assert_eq!(run_until_idle(&mut engine).await, TickOutcome::Idle);

    assert_eq!(c.with(|v| v.account_rev().unwrap()), Some(rev + 1));
    c.with(|v| {
        v.unlock(pw(NEW_PW)).unwrap();
        v.lock();
        assert!(matches!(
            v.unlock(pw(PW)),
            Err(core_api::vault::VaultError::WrongPassword)
        ));
    });
    a.stop().await;
}

/// The iOS keychain while the phone is locked: every access refused with
/// [`core_sync::SyncError::StorageLocked`] while `locked` is set. With
/// `saves_only`, reads and the pre-refresh check still pass and only the
/// save fails (the lock came between the check and the save).
struct LockableStore {
    inner: Arc<core_sync::MemoryTokenStore>,
    locked: std::sync::atomic::AtomicBool,
    saves_only: std::sync::atomic::AtomicBool,
}

impl LockableStore {
    fn new(inner: Arc<core_sync::MemoryTokenStore>) -> Arc<Self> {
        Arc::new(Self {
            inner,
            locked: false.into(),
            saves_only: false.into(),
        })
    }
    fn set(&self, locked: bool, saves_only: bool) {
        use std::sync::atomic::Ordering::SeqCst;
        self.saves_only.store(saves_only, SeqCst);
        self.locked.store(locked, SeqCst);
    }
    fn refuses_reads(&self) -> bool {
        use std::sync::atomic::Ordering::SeqCst;
        self.locked.load(SeqCst) && !self.saves_only.load(SeqCst)
    }
}

impl core_sync::TokenStore for LockableStore {
    fn load(
        &self,
        device: Id,
    ) -> Result<Option<core_sync::DeviceCredentials>, core_sync::SyncError> {
        if self.refuses_reads() {
            return Err(core_sync::SyncError::StorageLocked);
        }
        self.inner.load(device)
    }
    fn save(
        &self,
        device: Id,
        creds: &core_sync::DeviceCredentials,
    ) -> Result<(), core_sync::SyncError> {
        if self.locked.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(core_sync::SyncError::StorageLocked);
        }
        self.inner.save(device, creds)
    }
    fn delete(&self, device: Id) -> Result<(), core_sync::SyncError> {
        self.inner.delete(device)
    }
    fn ready(&self, _: Id) -> Result<(), core_sync::SyncError> {
        if self.refuses_reads() {
            return Err(core_sync::SyncError::StorageLocked);
        }
        Ok(())
    }
}

/// A stub server whose access tokens are always expired (every call
/// refreshes) and that, like the real one, revokes the device when a
/// rotated-out refresh token comes back. Returns the stub, the device, and
/// the refresh token the stub issued last.
async fn rotating_stub(dir: &std::path::Path) -> (net::Stub, Dev, Id, Arc<Mutex<String>>) {
    let material = Arc::new(Mutex::new(String::new()));
    let device = Arc::new(Mutex::new(Id::nil()));
    let current = Arc::new(Mutex::new(String::from("refresh")));
    let (m, d, c) = (material.clone(), device.clone(), current.clone());
    let stub = net::stub(move |req| {
        let (m, d) = (m.lock().unwrap().clone(), *d.lock().unwrap());
        match req.route().as_str() {
            "POST /v1/refresh" => {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
                let mut c = c.lock().unwrap();
                if body["refreshToken"] != c.as_str() {
                    return net::error(401, "revoked");
                }
                *c = format!("refresh-{}", Id::now_v7().simple());
                net::json(
                    200,
                    format!(
                        r#"{{"accessToken":"access","refreshToken":"{c}","expiresIn":0,"deviceId":"{d}","node":1,"epoch":"stub-epoch"}}"#
                    ),
                )
            }
            "GET /v1/events" => net::error(429, "rate_limited"),
            _ => healthy(req, d, &m).unwrap_or_else(|| net::error(404, "not_found")),
        }
    })
    .await;
    let (dev, id) = stub_device(dir, &stub.url);
    *material.lock().unwrap() = material_json(&dev);
    *device.lock().unwrap() = id;
    (stub, dev, id, current)
}

fn saved_refresh(dev: &Dev, id: Id) -> String {
    use core_sync::TokenStore;
    use secrecy::ExposeSecret;
    dev.tokens
        .load(id)
        .unwrap()
        .unwrap()
        .refresh
        .expose_secret()
        .to_owned()
}

/// Sync credentials stayed in the keychain: nothing in the vault database.
fn no_fallback(dev: &Dev) {
    assert_ne!(
        dev.with(|v| v.sync_get("token_store").unwrap()).as_deref(),
        Some(&b"file"[..]),
        "switched to the vault-database fallback"
    );
    assert_eq!(dev.with(|v| v.sync_get("refresh_token").unwrap()), None);
    assert_eq!(dev.with(|v| v.sync_get("device_secret").unwrap()), None);
}

/// iOS, phone locked with Tether kept alive: the keychain refuses, so sync
/// pauses (no refresh is even sent, so no token is rotated away), never
/// moves the credentials to the vault database, and picks up again once the
/// app is back in front.
#[tokio::test(flavor = "multi_thread")]
async fn a_locked_keychain_pauses_sync_without_rotating_or_falling_back() {
    let dir = tempfile::tempdir().unwrap();
    let (stub, mut dev, id, current) = rotating_stub(dir.path()).await;
    let store = LockableStore::new(dev.tokens.clone());
    dev.handle = Some(core_sync::start(dev.vault.clone(), store.clone()).expect("sync on"));
    dev.settle().await;
    assert_eq!(saved_refresh(&dev, id), *current.lock().unwrap());

    store.set(true, false);
    let refreshes = stub.count("POST /v1/refresh");
    let status = dev.sync().await;
    assert!(matches!(status, SyncStatus::Idle { .. }), "{status:?}");
    // Still locked: the app coming to the front cannot be (Face ID shows
    // first), but a stray wake-up must not rotate either.
    dev.handle().notify_foreground();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        stub.count("POST /v1/refresh"),
        refreshes,
        "rotated while locked"
    );
    assert!(!dev.handle().client().credentials_unsaved());
    assert_eq!(saved_refresh(&dev, id), *current.lock().unwrap());
    no_fallback(&dev);
    let synced = |s: &SyncStatus| matches!(s, SyncStatus::Idle { last_sync } if *last_sync > 0);
    let paused_at = match &*dev.handle().status().borrow() {
        SyncStatus::Idle { last_sync } => *last_sync,
        other => panic!("{other:?}"),
    };

    // Unlocked, back in front: sync carries on with the same credentials.
    store.set(false, false);
    dev.handle().notify_foreground();
    let after = status_becomes(
        &dev,
        |s| matches!(s, SyncStatus::Idle { last_sync } if *last_sync > paused_at),
    )
    .await;
    assert!(synced(&after), "{after:?}");
    assert!(stub.count("POST /v1/refresh") > refreshes);
    assert_eq!(saved_refresh(&dev, id), *current.lock().unwrap());
    no_fallback(&dev);
    dev.stop().await;
}

/// The phone locked between the pre-refresh check and the save: the server
/// already rotated, so the new token is kept in memory (not in the vault
/// database), sync pauses, and the token is saved once the keychain answers
/// again; the server never sees the rotated-out token.
#[tokio::test(flavor = "multi_thread")]
async fn a_rotation_the_locked_keychain_missed_is_saved_after_unlock() {
    let dir = tempfile::tempdir().unwrap();
    let (stub, mut dev, id, current) = rotating_stub(dir.path()).await;
    let store = LockableStore::new(dev.tokens.clone());
    dev.handle = Some(core_sync::start(dev.vault.clone(), store.clone()).expect("sync on"));
    dev.settle().await;
    let before = saved_refresh(&dev, id);

    store.set(true, true);
    let status = dev.sync().await;
    assert!(matches!(status, SyncStatus::Idle { .. }), "{status:?}");
    assert!(dev.handle().client().credentials_unsaved());
    assert_ne!(*current.lock().unwrap(), before, "the server rotated");
    assert_eq!(saved_refresh(&dev, id), before, "the save was refused");
    no_fallback(&dev);
    let refreshes = stub.count("POST /v1/refresh");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(stub.count("POST /v1/refresh"), refreshes, "paused");

    store.set(false, false);
    dev.handle().notify_foreground();
    eventually("the rotated token is saved", || {
        !dev.handle().client().credentials_unsaved()
            && saved_refresh(&dev, id) == *current.lock().unwrap()
    })
    .await;
    dev.settle().await;
    assert_eq!(saved_refresh(&dev, id), *current.lock().unwrap());
    no_fallback(&dev);
    dev.stop().await;
}
