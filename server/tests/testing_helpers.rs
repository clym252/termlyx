//! The `testing` helpers later tasks build on.

mod common;

use common::*;
use core_proto::{CodeKind, JoinCode};
use sha2::{Digest, Sha256};

#[tokio::test]
async fn make_invite_stores_hash_and_encodes_url_and_pin() {
    for srv in [spawn().await, spawn_tls().await] {
        let code = JoinCode::parse(&srv.make_invite().await).unwrap();
        assert_eq!(code.kind, CodeKind::Invite);
        assert_eq!(code.url, srv.url);
        assert_eq!(code.pin.is_some(), srv.pin != [0; 32]);
        let stored: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM invites WHERE token_hash = ? AND used_at IS NULL",
        )
        .bind(Sha256::digest(code.token).to_vec())
        .fetch_one(&srv.state.db)
        .await
        .unwrap();
        assert_eq!(stored, 1, "only the hash of the token is stored");
        let raw: i64 = sqlx::query_scalar("SELECT count(*) FROM invites WHERE token_hash = ?")
            .bind(code.token.to_vec())
            .fetch_one(&srv.state.db)
            .await
            .unwrap();
        assert_eq!(raw, 0);
    }
}

#[tokio::test]
async fn clock_override_survives_restart_and_data_dir_is_reused() {
    let srv = spawn().await;
    srv.set_clock(1_000_000);
    assert_eq!(srv.state.now_ms(), 1_000_000);
    srv.make_invite().await;
    let dir = srv.dir.path().to_path_buf();
    let srv = srv.restart().await;
    assert_eq!(srv.dir.path(), dir);
    assert_eq!(srv.state.now_ms(), 1_000_000);
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM invites")
        .fetch_one(&srv.state.db)
        .await
        .unwrap();
    assert_eq!(n, 1, "same database after restart");
    srv.set_clock(0);
    assert!(srv.state.now_ms() > 1_000_000);
}
