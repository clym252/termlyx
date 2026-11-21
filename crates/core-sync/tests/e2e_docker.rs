//! Two vaults syncing through a real `tether-server` in a Docker container.
//!
//! Everything else in this crate runs against the in-process server. This
//! test is the check that the shipped image (its TLS, its SQLite, its
//! environment) speaks the protocol to the real client engine. It is
//! `#[ignore]`d and also skips itself unless the container's details are in
//! the environment; `scripts/acceptance.sh server` starts the container,
//! creates an invite and sets:
//!
//! * `TETHER_SYNC_SERVER`: the server's URL (`https://localhost:18443`),
//! * `TETHER_SYNC_PIN`: the certificate pin as `tether-server fingerprint`
//!   prints it (`sha256:<64 hex>`),
//! * `TETHER_SYNC_INVITE`: a fresh invite code (single use).
//!
//! The invite carries the URL and the pin; the other two variables are what
//! the operator would read off the server, and the test insists the code
//! agrees with them.
//!
//! Run by hand: `TETHER_SYNC_SERVER=... TETHER_SYNC_PIN=... TETHER_SYNC_INVITE=...
//! cargo test -p core-sync --test e2e_docker -- --include-ignored --nocapture`.

mod common;

use std::time::{Duration, Instant};

use common::*;
use core_model::{Host, Id, Identity};
use core_proto::fingerprint_text;
use core_sync::flows::{self, RecoveryChoice};
use core_sync::parse_join_code;
use secrecy::ExposeSecret;

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

/// The (plaintext) password of every identity on `dev`, sorted by label.
fn identity_passwords(dev: &Dev) -> Vec<(String, Option<String>)> {
    let mut all: Vec<_> = dev
        .with(|v| v.list::<Identity>().unwrap())
        .into_iter()
        .map(|i| (i.label, i.password.map(|p| p.expose_secret().to_owned())))
        .collect();
    all.sort();
    all
}

fn relabel(dev: &Dev, id: Id, label: &str) {
    dev.with(|v| {
        let mut h = v.get::<Host>(id).unwrap().unwrap();
        h.label = label.into();
        v.upsert(h).unwrap();
    });
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a tether-server container; run scripts/acceptance.sh server"]
async fn two_vaults_sync_through_real_server() {
    let (Some(server), Some(pin), Some(invite)) = (
        env("TETHER_SYNC_SERVER"),
        env("TETHER_SYNC_PIN"),
        env("TETHER_SYNC_INVITE"),
    ) else {
        eprintln!(
            "skipping two_vaults_sync_through_real_server: set TETHER_SYNC_SERVER, \
             TETHER_SYNC_PIN and TETHER_SYNC_INVITE (scripts/acceptance.sh server does)"
        );
        return;
    };

    // The code must name the server we were told about, with the pin the
    // operator reads off it.
    let invite = parse_join_code(&invite).expect("invite code");
    assert_eq!(
        invite.url.trim_end_matches('/'),
        server.trim_end_matches('/')
    );
    assert_eq!(fingerprint_text(&invite.pin.expect("pinned code")), pin);

    let user = format!("e2e-{}", Id::now_v7().simple());
    let dir = tempfile::tempdir().unwrap();

    // -- A: an existing vault turns sync on -----------------------------
    let mut a = local_device(dir.path(), "a");
    let recovery = a.recovery.clone();
    let enabled = flows::enable_with_invite(
        &a.vault,
        a.tokens(),
        &invite,
        &user,
        pw(PW),
        RecoveryChoice::Existing(recovery.into()),
    )
    .await
    .expect("enable sync");
    assert!(enabled.new_recovery_text.is_none());
    a.start();
    a.settle().await;

    // A host and an identity with a password, after sync is on.
    let first = host("e2e-web");
    let first_id = first.id;
    let login = identity("e2e-s3cret-password");
    a.with(|v| {
        v.upsert(first).unwrap();
        v.upsert(login).unwrap();
    });
    a.handle().notify_local_change();
    a.settle().await;

    // -- B: joins with an add-device code from A ----------------------------
    let (code, _, expires) = flows::create_link_code(a.handle(), None)
        .await
        .expect("link code");
    assert!(expires > 0);
    let link = parse_join_code(&code).unwrap();
    assert_eq!(link.url.trim_end_matches('/'), server.trim_end_matches('/'));
    assert_eq!(fingerprint_text(&link.pin.expect("pinned code")), pin);
    let mut b = blank_device(dir.path(), "b");
    flows::join_with_code(&b.vault, b.tokens(), &link, &user, pw(PW), false)
        .await
        .expect("join");
    b.start();
    b.settle().await;

    assert_eq!(b.hosts(), a.hosts(), "B sees A's hosts, decrypted");
    assert_eq!(b.hosts().len(), 1);
    assert_eq!(
        identity_passwords(&b),
        identity_passwords(&a),
        "B sees A's identity password"
    );
    assert_eq!(
        identity_passwords(&b)[0].1.as_deref(),
        Some("e2e-s3cret-password")
    );
    assert_ne!(a.device_id(), b.device_id());

    // -- live edits, both ways -------------------------------------------
    // Let both event sockets connect.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let live = host("e2e-live");
    let live_id = live.id;
    a.with(|v| v.upsert(live).unwrap());
    let start = Instant::now();
    a.handle().notify_local_change();
    eventually("B sees A's new host", || {
        b.with(|v| v.get::<Host>(live_id).unwrap().is_some())
    })
    .await;
    let took = start.elapsed();
    println!("A -> B live update: {took:?}");
    assert!(took < Duration::from_secs(2), "slow live update: {took:?}");

    relabel(&b, live_id, "e2e-live edited on b");
    let start = Instant::now();
    b.handle().notify_local_change();
    eventually("A sees B's edit", || {
        a.with(|v| v.get::<Host>(live_id).unwrap().unwrap().label == "e2e-live edited on b")
    })
    .await;
    let took = start.elapsed();
    println!("B -> A live update: {took:?}");
    assert!(took < Duration::from_secs(2), "slow live update: {took:?}");
    assert_eq!(a.hosts(), b.hosts());
    assert!(a.with(|v| v.list_conflicts().unwrap()).is_empty());
    assert!(b.with(|v| v.list_conflicts().unwrap()).is_empty());

    // -- the same host edited offline on both sides: one conflict ---------
    a.stop().await;
    b.stop().await;
    relabel(&a, first_id, "e2e-web from a");
    // B's edit must be later by the clock, not only by a node-id tie-break.
    tokio::time::sleep(Duration::from_millis(5)).await;
    relabel(&b, first_id, "e2e-web from b");
    a.start();
    a.settle().await;
    b.start();
    b.settle().await;
    a.settle().await;

    for (name, dev) in [("A", &a), ("B", &b)] {
        let conflicts = dev.with(|v| v.list_conflicts().unwrap());
        assert_eq!(conflicts.len(), 1, "{name}: exactly one conflict");
        assert_eq!(conflicts[0].record_id, first_id, "{name}");
    }
    assert_eq!(a.hosts(), b.hosts(), "both converge");
    let winner = a.with(|v| v.get::<Host>(first_id).unwrap().unwrap().label);
    assert_eq!(winner, "e2e-web from b", "the later edit wins");

    a.stop().await;
    b.stop().await;
}
