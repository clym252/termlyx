//! `GET /v1/sync/changes`, `POST /v1/sync/push`, compaction and the 410 rule.

mod common;

use std::sync::{Arc, Mutex};

use common::*;
use core_proto::{ErrorCode, PushResult, WireRow};

const DAY_MS: i64 = 24 * 60 * 60 * 1000;

fn ok_of(r: &PushResult) -> (i64, i64) {
    match r {
        PushResult::Ok { rev, seq, .. } => (*rev, *seq),
        other => panic!("expected ok, got {other:?}"),
    }
}

fn conflict_of(r: &PushResult) -> &WireRow {
    match r {
        PushResult::Conflict { current, .. } => current,
        other => panic!("expected conflict, got {other:?}"),
    }
}

#[tokio::test]
async fn push_new_then_changes_returns_it() {
    let srv = spawn().await;
    let a = register(&srv, "alice").await;
    let tok = &a.tokens.access_token;
    let id = new_id();

    let resp = push_ok(
        &srv,
        tok,
        vec![item(row(&id, "host", now_ms(), b"sealed-1"), None)],
    )
    .await;
    assert_eq!(resp.results.len(), 1);
    assert_eq!(ok_of(&resp.results[0]), (1, 1));
    assert!(matches!(&resp.results[0], PushResult::Ok { id: i, .. } if *i == id));

    let page = pull(&srv, tok, 0, None).await;
    assert_eq!(page.epoch, srv.state.epoch);
    assert_eq!((page.next, page.more), (1, false));
    assert_eq!(page.changes.len(), 1);
    let got = &page.changes[0];
    assert_eq!((got.id.as_str(), got.kind.as_str()), (id.as_str(), "host"));
    assert_eq!((got.rev, got.seq, got.deleted), (Some(1), Some(1), false));
    assert_eq!(got.envelope, b"sealed-1");
    assert_eq!(got.key_version, 1);

    // The cursor is exclusive; an empty page keeps the cursor.
    let page = pull(&srv, tok, 1, None).await;
    assert!(page.changes.is_empty());
    assert_eq!((page.next, page.more), (1, false));
}

#[tokio::test]
async fn update_with_matching_base_advances_rev_and_seq() {
    let srv = spawn().await;
    let a = register(&srv, "alice").await;
    let tok = &a.tokens.access_token;
    let (id, other) = (new_id(), new_id());
    push_ok(
        &srv,
        tok,
        vec![item(row(&id, "host", now_ms(), b"v1"), None)],
    )
    .await;
    push_ok(
        &srv,
        tok,
        vec![item(row(&other, "host", now_ms(), b"o1"), None)],
    )
    .await;

    let resp = push_ok(
        &srv,
        tok,
        vec![item(row(&id, "host", now_ms(), b"v2"), Some(1))],
    )
    .await;
    assert_eq!(ok_of(&resp.results[0]), (2, 3));

    // The log shows the record once, at its newest position.
    let page = pull(&srv, tok, 0, None).await;
    let seqs: Vec<_> = page.changes.iter().map(|r| (r.id.clone(), r.seq)).collect();
    assert_eq!(seqs, vec![(other, Some(2)), (id, Some(3))]);
}

#[tokio::test]
async fn stale_base_rev_conflicts_with_current() {
    let srv = spawn().await;
    let a = register(&srv, "alice").await;
    let tok = &a.tokens.access_token;
    let id = new_id();
    push_ok(
        &srv,
        tok,
        vec![item(row(&id, "host", now_ms(), b"v1"), None)],
    )
    .await;
    push_ok(
        &srv,
        tok,
        vec![item(row(&id, "host", now_ms(), b"v2"), Some(1))],
    )
    .await;

    // Based on rev 1, but the server is at 2.
    let resp = push_ok(
        &srv,
        tok,
        vec![item(row(&id, "host", now_ms(), b"mine"), Some(1))],
    )
    .await;
    let cur = conflict_of(&resp.results[0]);
    assert_eq!(cur.envelope, b"v2");
    assert_eq!((cur.rev, cur.seq), (Some(2), Some(2)));
    // A new record pushed over an existing one conflicts too.
    let resp = push_ok(
        &srv,
        tok,
        vec![item(row(&id, "host", now_ms(), b"mine"), None)],
    )
    .await;
    assert_eq!(conflict_of(&resp.results[0]).envelope, b"v2");

    // Nothing was written by the refused pushes.
    let page = pull(&srv, tok, 0, None).await;
    assert_eq!(page.changes.len(), 1);
    assert_eq!(page.changes[0].envelope, b"v2");
    assert_eq!(page.next, 2);
}

#[tokio::test]
async fn mixed_batch_keeps_the_accepted_items() {
    let srv = spawn().await;
    let a = register(&srv, "alice").await;
    let tok = &a.tokens.access_token;
    let (x, y) = (new_id(), new_id());
    push_ok(
        &srv,
        tok,
        vec![item(row(&x, "host", now_ms(), b"x1"), None)],
    )
    .await;

    let resp = push_ok(
        &srv,
        tok,
        vec![
            item(row(&y, "host", now_ms(), b"y1"), None),
            item(row(&x, "host", now_ms(), b"x-stale"), None),
            // The same id again in the same request sees the first write.
            item(row(&y, "host", now_ms(), b"y2"), Some(1)),
        ],
    )
    .await;
    assert_eq!(ok_of(&resp.results[0]), (1, 2));
    assert_eq!(conflict_of(&resp.results[1]).envelope, b"x1");
    assert_eq!(ok_of(&resp.results[2]), (2, 3));
    let page = pull(&srv, tok, 0, None).await;
    assert_eq!(page.changes.len(), 2);
}

#[tokio::test]
async fn repush_of_identical_write_is_ok() {
    let srv = spawn().await;
    let a = register(&srv, "alice").await;
    let tok = &a.tokens.access_token;
    let id = new_id();
    let r = row(&id, "host", now_ms(), b"sealed");

    let first = push_ok(&srv, tok, vec![item(r.clone(), None)]).await;
    assert_eq!(ok_of(&first.results[0]), (1, 1));
    // The response was lost; the client sends the very same write again.
    let again = push_ok(&srv, tok, vec![item(r.clone(), None)]).await;
    assert_eq!(ok_of(&again.results[0]), (1, 1));
    // The same holds for a retried update.
    let upd = row(&id, "host", now_ms(), b"sealed-2");
    let a1 = push_ok(&srv, tok, vec![item(upd.clone(), Some(1))]).await;
    let a2 = push_ok(&srv, tok, vec![item(upd, Some(1))]).await;
    assert_eq!(ok_of(&a1.results[0]), (2, 2));
    assert_eq!(ok_of(&a2.results[0]), (2, 2));
    // No extra log entries were made.
    assert_eq!(pull(&srv, tok, 0, None).await.next, 2);

    // Same hlc but a different envelope (or flag) is not a retry.
    let different = row(&id, "host", now_ms(), b"other");
    let resp = push_ok(&srv, tok, vec![item(different, Some(1))]).await;
    assert_eq!(conflict_of(&resp.results[0]).envelope, b"sealed-2");
}

#[tokio::test]
async fn push_rejects_far_future_hlc() {
    let srv = spawn().await;
    let a = register(&srv, "alice").await;
    let tok = &a.tokens.access_token;
    let (good, late) = (new_id(), new_id());

    // One minute ahead is fine; two hours is not, and takes the whole request
    // down with it.
    let res = push(
        &srv,
        tok,
        vec![
            item(row(&good, "host", now_ms() + 60_000, b"ok"), None),
            item(
                row(&late, "host", now_ms() + 2 * 60 * 60 * 1000, b"late"),
                None,
            ),
        ],
    )
    .await;
    assert_err(&res, 400, ErrorCode::ClockSkew);
    assert!(pull(&srv, tok, 0, None).await.changes.is_empty());

    // The bound follows the server's clock, not the system's.
    jump_clock(&srv, now_ms() + 3 * 60 * 60 * 1000).await;
    let resp = push_ok(
        &srv,
        tok,
        vec![item(
            row(&late, "host", now_ms() + 2 * 60 * 60 * 1000, b"late"),
            None,
        )],
    )
    .await;
    assert_eq!(ok_of(&resp.results[0]), (1, 1));
}

#[tokio::test]
async fn changes_pages_with_more_flag() {
    let srv = spawn().await;
    let a = register(&srv, "alice").await;
    let tok = &a.tokens.access_token;
    let ids: Vec<String> = (0..5).map(|_| new_id()).collect();
    let items = ids
        .iter()
        .map(|id| item(row(id, "host", now_ms(), id.as_bytes()), None))
        .collect();
    push_ok(&srv, tok, items).await;

    let p1 = pull(&srv, tok, 0, Some(2)).await;
    assert_eq!((p1.changes.len(), p1.next, p1.more), (2, 2, true));
    let p2 = pull(&srv, tok, p1.next, Some(2)).await;
    assert_eq!((p2.changes.len(), p2.next, p2.more), (2, 4, true));
    let p3 = pull(&srv, tok, p2.next, Some(2)).await;
    assert_eq!((p3.changes.len(), p3.next, p3.more), (1, 5, false));
    let got: Vec<_> = [p1, p2, p3]
        .into_iter()
        .flat_map(|p| p.changes)
        .map(|r| r.id)
        .collect();
    assert_eq!(got, ids);

    // Exactly a page's worth left: no spurious `more`.
    let p = pull(&srv, tok, 3, Some(2)).await;
    assert_eq!((p.changes.len(), p.next, p.more), (2, 5, false));
}

#[tokio::test]
async fn changes_parameters_are_checked() {
    let srv = spawn().await;
    let a = register(&srv, "alice").await;
    let tok = &a.tokens.access_token;
    for bad in [
        "/v1/sync/changes?since=-1",
        "/v1/sync/changes?since=0&limit=0",
        "/v1/sync/changes?since=0&limit=-5",
        "/v1/sync/changes?since=abc",
    ] {
        assert_err(&get_auth(&srv, bad, tok).await, 400, ErrorCode::BadRequest);
    }
    // `since` defaults to 0 and an over-large limit is capped at 1000.
    let res = get_auth(&srv, "/v1/sync/changes", tok).await;
    assert_eq!(res.status, 200);
    let ids: Vec<String> = (0..3).map(|_| new_id()).collect();
    let items = ids
        .iter()
        .map(|id| item(row(id, "host", now_ms(), b"e"), None))
        .collect();
    push_ok(&srv, tok, items).await;
    let page = pull(&srv, tok, 0, Some(100_000)).await;
    assert_eq!(page.changes.len(), 3);
}

#[tokio::test]
async fn tombstone_round_trip() {
    let srv = spawn().await;
    let a = register(&srv, "alice").await;
    let tok = &a.tokens.access_token;
    let id = new_id();
    push_ok(
        &srv,
        tok,
        vec![item(row(&id, "host", now_ms(), b"alive"), None)],
    )
    .await;

    let resp = push_ok(
        &srv,
        tok,
        vec![item(tombstone(&id, "host", now_ms()), Some(1))],
    )
    .await;
    assert_eq!(ok_of(&resp.results[0]), (2, 2));
    let page = pull(&srv, tok, 1, None).await;
    assert_eq!(page.changes.len(), 1);
    let t = &page.changes[0];
    assert!(t.deleted);
    assert!(t.envelope.is_empty());
    assert_eq!((t.rev, t.seq), (Some(2), Some(2)));

    // A deleted record can be written again on top of its tombstone.
    let resp = push_ok(
        &srv,
        tok,
        vec![item(row(&id, "host", now_ms(), b"back"), Some(2))],
    )
    .await;
    assert_eq!(ok_of(&resp.results[0]), (3, 3));
    let page = pull(&srv, tok, 2, None).await;
    assert!(!page.changes[0].deleted);
}

#[tokio::test]
async fn since_before_compaction_is_gone() {
    let srv = spawn().await;
    let a = register(&srv, "alice").await;
    let tok = &a.tokens.access_token;
    let t0 = now_ms();
    let (keep, dead, recent) = (new_id(), new_id(), new_id());
    push_ok(
        &srv,
        tok,
        vec![
            item(row(&keep, "host", t0, b"keep"), None),
            item(row(&dead, "host", t0, b"dead"), None),
            item(row(&recent, "host", t0, b"recent"), None),
        ],
    )
    .await;
    push_ok(&srv, tok, vec![item(tombstone(&dead, "host", t0), Some(1))]).await; // seq 4

    jump_clock(&srv, t0 + 170 * DAY_MS).await;
    let now = srv.state.now_ms();
    push_ok(
        &srv,
        tok,
        vec![item(tombstone(&recent, "host", now), Some(1))],
    )
    .await; // seq 5

    // 185 days after the first tombstone, 15 after the second.
    jump_clock(&srv, t0 + 185 * DAY_MS).await;
    let n = tether_server::compact::compact(&srv.state.db, srv.state.now_ms(), 180)
        .await
        .unwrap();
    assert_eq!(n, 1);

    // The cursor is behind the horizon (seq 4): the caller must resync.
    for since in [1, 2, 3] {
        assert_err(&changes(&srv, tok, since, None).await, 410, ErrorCode::Gone);
    }
    // At the horizon or beyond, the log is complete again.
    let page = pull(&srv, tok, 4, None).await;
    assert_eq!(page.changes.len(), 1);
    assert_eq!(page.changes[0].id, recent);
    assert!(page.changes[0].deleted);
    // Cursor 0 is "everything there is", so a resync (and a brand-new device)
    // can always read the whole vault.
    let all = pull(&srv, tok, 0, None).await;
    let ids: Vec<_> = all.changes.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, vec![keep.as_str(), recent.as_str()]);
    assert_eq!(all.next, 5);

    // The dead record is gone for good: a write based on it needs a resync.
    let res = push(
        &srv,
        tok,
        vec![item(row(&dead, "host", now, b"zombie"), Some(2))],
    )
    .await;
    assert_err(&res, 410, ErrorCode::Gone);
}

#[tokio::test]
async fn push_based_on_a_missing_record_is_gone() {
    let srv = spawn().await;
    let a = register(&srv, "alice").await;
    let res = push(
        &srv,
        &a.tokens.access_token,
        vec![item(row(&new_id(), "host", now_ms(), b"x"), Some(3))],
    )
    .await;
    // The client's view is ahead of the server's (a restored backup, say):
    // it must resync, with or without any compaction.
    assert_err(&res, 410, ErrorCode::Gone);
}

#[tokio::test]
async fn vaults_are_isolated() {
    let srv = spawn().await;
    let a = register_with(&srv, "alice", 1, 1).await;
    let b = register_with(&srv, "bob", 2, 1).await;
    let id = new_id();
    push_ok(
        &srv,
        &a.tokens.access_token,
        vec![item(row(&id, "host", now_ms(), b"alice-secret"), None)],
    )
    .await;

    // Bob sees nothing of Alice's...
    let page = pull(&srv, &b.tokens.access_token, 0, None).await;
    assert!(page.changes.is_empty());
    assert_eq!(page.next, 0);
    // ...and the same record id in his vault is a different, new record.
    let resp = push_ok(
        &srv,
        &b.tokens.access_token,
        vec![item(row(&id, "host", now_ms(), b"bob-own"), None)],
    )
    .await;
    assert_eq!(ok_of(&resp.results[0]), (1, 1));
    let page = pull(&srv, &a.tokens.access_token, 0, None).await;
    assert_eq!(page.changes.len(), 1);
    assert_eq!(page.changes[0].envelope, b"alice-secret");
    // Two devices of one account share the vault.
    let second = add_device(&srv, &a, "Laptop").await;
    assert_eq!(
        pull(&srv, &second.access_token, 0, None)
            .await
            .changes
            .len(),
        1
    );
}

#[tokio::test]
async fn push_validates_every_item() {
    let srv = spawn().await;
    let a = register(&srv, "alice").await;
    let tok = &a.tokens.access_token;
    let t = now_ms();
    let base = || row(&new_id(), "host", t, b"env");

    let mut cases: Vec<(&str, WireRow)> = vec![];
    for kind in ["", "Host", "ho st", "host1", "ho-st", &"x".repeat(33)] {
        cases.push((
            "kind",
            WireRow {
                kind: kind.into(),
                ..base()
            },
        ));
    }
    cases.push((
        "id",
        WireRow {
            id: "not-a-uuid".into(),
            ..base()
        },
    ));
    cases.push((
        "id case",
        WireRow {
            id: new_id().to_uppercase(),
            ..base()
        },
    ));
    cases.push((
        "key_version",
        WireRow {
            key_version: 0,
            ..base()
        },
    ));
    cases.push((
        "huge envelope",
        WireRow {
            envelope: vec![1; 1024 * 1024 + 1],
            ..base()
        },
    ));
    cases.push((
        "tombstone with envelope",
        WireRow {
            deleted: true,
            ..base()
        },
    ));
    cases.push((
        "empty live envelope",
        WireRow {
            envelope: vec![],
            ..base()
        },
    ));
    for bad in [
        "",
        "zz",
        "0000000000000001-00000000-0001x",
        "0000000000000001-00000000",
    ] {
        cases.push((
            "hlc",
            WireRow {
                hlc: bad.into(),
                ..base()
            },
        ));
    }
    cases.push((
        "hlc uppercase hex",
        WireRow {
            hlc: hlc(t, 0, 1).to_uppercase(),
            ..base()
        },
    ));
    for (what, r) in cases {
        let res = push(&srv, tok, vec![item(r, None)]).await;
        assert!(
            res.status == 400 && error_of(&res).code == ErrorCode::BadRequest,
            "{what}: {} {}",
            res.status,
            String::from_utf8_lossy(&res.body)
        );
    }
    // The largest allowed envelope and the longest kind are fine.
    let ok = WireRow {
        kind: "x".repeat(32),
        envelope: vec![9; 1024 * 1024],
        ..base()
    };
    push_ok(&srv, tok, vec![item(ok, None)]).await;
    let page = pull(&srv, tok, 0, None).await;
    assert_eq!(page.changes.len(), 1);

    // 500 items are accepted, 501 are not.
    let many = |n: usize| -> Vec<_> {
        (0..n)
            .map(|_| item(row(&new_id(), "host", t, b"e"), None))
            .collect()
    };
    let resp = push_ok(&srv, tok, many(500)).await;
    assert_eq!(resp.results.len(), 500);
    assert_err(
        &push(&srv, tok, many(501)).await,
        400,
        ErrorCode::BadRequest,
    );
    // An invalid item anywhere rejects the whole request: nothing is written.
    let mut items = many(2);
    items.push(item(
        WireRow {
            key_version: 0,
            ..base()
        },
        None,
    ));
    assert_err(&push(&srv, tok, items).await, 400, ErrorCode::BadRequest);
    assert_eq!(pull(&srv, tok, 0, Some(1000)).await.changes.len(), 501);
}

#[tokio::test]
async fn sync_needs_a_device_session() {
    let srv = spawn().await;
    let a = register(&srv, "alice").await;
    let id = new_id();

    // No token.
    let res = call(
        &srv,
        "POST",
        "/v1/sync/push",
        None,
        None,
        Some(&core_proto::PushReq { items: vec![] }),
    )
    .await;
    assert_err(&res, 401, ErrorCode::Unauthorized);
    let res = call::<()>(&srv, "GET", "/v1/sync/changes?since=0", None, None, None).await;
    assert_err(&res, 401, ErrorCode::Unauthorized);

    // The password-reset token cannot read or write records.
    let rec = recover_token(&srv, &a).await;
    let res = push(
        &srv,
        &rec.access_token,
        vec![item(row(&id, "host", now_ms(), b"x"), None)],
    )
    .await;
    assert_err(&res, 403, ErrorCode::Forbidden);
    assert_err(
        &changes(&srv, &rec.access_token, 0, None).await,
        403,
        ErrorCode::Forbidden,
    );

    // A revoked device is told so.
    let second = add_device(&srv, &a, "Laptop").await;
    let path = format!("/v1/devices/{}", second.device_id);
    assert_eq!(
        delete_auth(&srv, &path, &a.tokens.access_token)
            .await
            .status,
        204
    );
    let res = push(&srv, &second.access_token, vec![]).await;
    assert_err(&res, 401, ErrorCode::Revoked);
}

#[tokio::test]
async fn old_clients_cannot_push_but_can_pull() {
    let srv = spawn().await;
    let a = register(&srv, "alice").await;
    let tok = &a.tokens.access_token;
    let auth = format!("Bearer {tok}");
    let send = |schema: &'static str, body: Vec<u8>| {
        let auth = auth.clone();
        let srv = &srv;
        async move {
            srv.request(
                "POST",
                "/v1/sync/push",
                &[
                    ("Authorization", auth.as_str()),
                    ("Content-Type", "application/json"),
                    ("X-Tether-Schema", schema),
                ],
                Some(body),
            )
            .await
        }
    };
    let body = serde_json::to_vec(&core_proto::PushReq { items: vec![] }).unwrap();
    assert_eq!(send("2", body.clone()).await.status, 200);
    // A device on kind set 1 is now behind.
    assert_err(&send("1", body).await, 400, ErrorCode::UpdateClient);
    assert_eq!(changes(&srv, tok, 0, None).await.status, 200);
}

/// A log sink for `tracing`.
#[derive(Clone, Default)]
struct Sink(Arc<Mutex<Vec<u8>>>);
impl std::io::Write for Sink {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Sink {
    type Writer = Sink;
    fn make_writer(&'a self) -> Sink {
        self.clone()
    }
}

/// Everything every test of this binary has logged. A global subscriber (not a
/// per-test one) so that the capture is complete whatever runs in parallel.
fn captured_logs() -> Sink {
    static SINK: std::sync::OnceLock<Sink> = std::sync::OnceLock::new();
    SINK.get_or_init(|| {
        let sink = Sink::default();
        let sub = tracing_subscriber::fmt()
            .with_writer(sink.clone())
            .with_ansi(false)
            .with_max_level(tracing::Level::TRACE)
            .finish();
        tracing::subscriber::set_global_default(sub).expect("no other global subscriber");
        sink
    })
    .clone()
}

#[tokio::test]
async fn server_db_holds_no_plaintext() {
    const CANARY: &str = "CANARY-7f3a";
    let sink = captured_logs();

    let srv = spawn().await;
    let a = register(&srv, "alice").await;
    let tok = &a.tokens.access_token;
    let key = [7u8; 32];

    // Real envelopes, sealed exactly as a client would, around the canary.
    let mut envelopes = vec![];
    let mut items = vec![];
    for i in 0..4 {
        let id = new_id();
        let plaintext = format!(
            "{{\"label\":\"{CANARY} host {i}\",\"address\":\"{CANARY}.example.com\",\"password\":\"{CANARY}\"}}"
        );
        let env = core_crypto::envelope::seal(
            &key,
            &core_crypto::envelope::Aad {
                id: &id,
                vault_id: &a.vault_id,
                kind: "host",
                key_version: 1,
            },
            plaintext.as_bytes(),
        );
        envelopes.push(env.clone());
        items.push(item(row(&id, "host", now_ms(), &env), None));
    }
    push_ok(&srv, tok, items).await;
    let page = pull(&srv, tok, 0, None).await;
    assert_eq!(page.changes.len(), 4);

    // What must not leak. The canary never reaches the server in the clear,
    // so what can actually go wrong is ciphertext or secrets being written
    // where they do not belong: envelopes in the logs (in any encoding), and
    // session secrets in the database or the logs.
    let canary = Needles::default().canaries(&[CANARY, "example.com"]);
    let envs = Needles::default().envelopes(&envelopes);
    let secrets = Needles::default()
        .secret("access token", &a.tokens.access_token)
        .secret("refresh token", &a.tokens.refresh_token)
        .secret("device secret", &a.tokens.device_secret);

    let on_disk = db_bytes(srv.dir.path());
    // Positive control: the sealed bytes are in the database file set (as they
    // must be), so a search over it can find things.
    assert_eq!(
        envs.found_in(&on_disk)
            .iter()
            .filter(|w| w.ends_with("(raw prefix)"))
            .count(),
        envelopes.len(),
        "sealed envelopes should be in the database or its WAL"
    );
    canary.assert_absent("the database", &on_disk);
    secrets.assert_absent("the database", &on_disk);

    let logs = sink.0.lock().unwrap().clone();
    // Positive control: the push and the register were logged.
    assert!(
        contains(&logs, b"sync push"),
        "the push should have been logged: {}",
        String::from_utf8_lossy(&logs)
    );
    canary.assert_absent("the logs", &logs);
    envs.assert_absent("the logs", &logs);
    secrets.assert_absent("the logs", &logs);
}

/// The leak search must actually find what it looks for, in each encoding.
#[test]
fn leak_needles_find_every_encoding() {
    let env: Vec<u8> = (0u8..80).collect();
    let needles = Needles::default()
        .canaries(&["CANARY-x"])
        .envelopes(std::slice::from_ref(&env))
        .secret("token", &data_encoding::BASE64URL_NOPAD.encode(&[9u8; 32]));
    let b64 = data_encoding::BASE64.encode(&env);
    for (what, hay) in [
        ("canary", b"..CANARY-x..".to_vec()),
        ("raw", [b"a".as_slice(), &env[..32], b"b"].concat()),
        (
            "hex",
            data_encoding::HEXLOWER.encode(&env[..32]).into_bytes(),
        ),
        ("base64", format!("body={b64}").into_bytes()),
        ("base64 prefix", format!("body={}", &b64[..40]).into_bytes()),
        (
            "token text",
            data_encoding::BASE64URL_NOPAD
                .encode(&[9u8; 32])
                .into_bytes(),
        ),
        (
            "token bytes",
            [
                1u8, 2, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9,
                9, 9, 9, 9, 9, 9, 9, 3,
            ]
            .to_vec(),
        ),
    ] {
        assert!(
            !needles.found_in(&hay).is_empty(),
            "{what} should have been detected"
        );
        let caught = std::panic::catch_unwind(|| needles.assert_absent("test", &hay));
        assert!(caught.is_err(), "{what} should have been detected");
    }
    needles.assert_absent("test", b"nothing to see here");
}
