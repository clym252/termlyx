//! `GET /v1/events`: authentication, targeting and revocation.

mod common;

use std::time::Duration;

use common::*;
use core_proto::{ErrorBody, ErrorCode, Event};
use tokio_tungstenite::tungstenite::Error as WsError;

const WAIT: Duration = Duration::from_secs(5);
const QUIET: Duration = Duration::from_millis(400);

async fn push_one(srv: &TestServer, token: &str) {
    push_ok(
        srv,
        token,
        vec![item(row(&new_id(), "host", now_ms(), b"env"), None)],
    )
    .await;
}

/// The error a refused upgrade answered with.
async fn refused(srv: &TestServer, protocols: Option<&str>) -> (u16, ErrorBody) {
    match ws_open_with(srv, protocols).await {
        Err(WsError::Http(res)) => {
            let body = res.body().clone().unwrap_or_default();
            (
                res.status().as_u16(),
                serde_json::from_slice(&body).expect("ErrorBody"),
            )
        }
        Err(e) => panic!("expected an HTTP refusal, got {e}"),
        Ok(_) => panic!("upgrade should have been refused"),
    }
}

#[tokio::test]
async fn other_device_gets_changed_event() {
    let srv = spawn().await;
    let a = register(&srv, "alice").await;
    let second = add_device(&srv, &a, "Laptop").await;
    let mut ws = ws_connect(&srv, &second.access_token).await;

    push_one(&srv, &a.tokens.access_token).await;
    assert_eq!(
        ws_next(&mut ws, WAIT).await,
        WsNext::Event(Event::Changed { seq: 1 })
    );
    push_one(&srv, &a.tokens.access_token).await;
    assert_eq!(
        ws_next(&mut ws, WAIT).await,
        WsNext::Event(Event::Changed { seq: 2 })
    );
}

#[tokio::test]
async fn pusher_gets_no_event() {
    let srv = spawn().await;
    let a = register(&srv, "alice").await;
    let second = add_device(&srv, &a, "Laptop").await;
    let mut mine = ws_connect(&srv, &a.tokens.access_token).await;
    let mut theirs = ws_connect(&srv, &second.access_token).await;

    push_one(&srv, &a.tokens.access_token).await;
    assert_eq!(
        ws_next(&mut theirs, WAIT).await,
        WsNext::Event(Event::Changed { seq: 1 })
    );
    assert_eq!(ws_next(&mut mine, QUIET).await, WsNext::Silence);

    // A push that was refused entirely (conflict only) tells nobody.
    let id = new_id();
    push_ok(
        &srv,
        &a.tokens.access_token,
        vec![item(row(&id, "host", now_ms(), b"v1"), None)],
    )
    .await;
    assert_eq!(
        ws_next(&mut theirs, WAIT).await,
        WsNext::Event(Event::Changed { seq: 2 })
    );
    push_ok(
        &srv,
        &a.tokens.access_token,
        vec![item(row(&id, "host", now_ms(), b"v2"), None)],
    )
    .await;
    assert_eq!(ws_next(&mut theirs, QUIET).await, WsNext::Silence);
}

#[tokio::test]
async fn events_stay_inside_the_account() {
    let srv = spawn().await;
    let a = register_with(&srv, "alice", 1, 1).await;
    let b = register_with(&srv, "bob", 2, 1).await;
    let mut bob = ws_connect(&srv, &b.tokens.access_token).await;
    push_one(&srv, &a.tokens.access_token).await;
    assert_eq!(ws_next(&mut bob, QUIET).await, WsNext::Silence);
}

#[tokio::test]
async fn revoked_device_socket_closes() {
    let srv = spawn().await;
    let a = register(&srv, "alice").await;
    let second = add_device(&srv, &a, "Laptop").await;
    let mut gone = ws_connect(&srv, &second.access_token).await;
    let mut kept = ws_connect(&srv, &a.tokens.access_token).await;

    let path = format!("/v1/devices/{}", second.device_id);
    assert_eq!(
        delete_auth(&srv, &path, &a.tokens.access_token)
            .await
            .status,
        204
    );

    assert_eq!(
        ws_next(&mut gone, WAIT).await,
        WsNext::Event(Event::Revoked)
    );
    assert_eq!(ws_next(&mut gone, WAIT).await, WsNext::Closed);
    // Only the revoked device hears it.
    assert_eq!(ws_next(&mut kept, QUIET).await, WsNext::Silence);
}

#[tokio::test]
async fn account_changes_reach_every_device() {
    let srv = spawn().await;
    let a = register(&srv, "alice").await;
    let second = add_device(&srv, &a, "Laptop").await;
    let mut mine = ws_connect(&srv, &a.tokens.access_token).await;
    let mut theirs = ws_connect(&srv, &second.access_token).await;

    // Targeting `All` includes the caller (Task 6 publishes this way).
    srv.state.events.publish(
        &a.vault_id,
        tether_server::state::Target::All,
        Event::AccountChanged { account_rev: 2 },
    );
    for ws in [&mut mine, &mut theirs] {
        assert_eq!(
            ws_next(ws, WAIT).await,
            WsNext::Event(Event::AccountChanged { account_rev: 2 })
        );
    }
}

#[tokio::test]
async fn upgrade_needs_a_valid_device_token() {
    let srv = spawn().await;
    let a = register(&srv, "alice").await;
    let bearer = |t: &str| format!("tether.v1, {}", core_proto::ws_bearer_protocol(t));

    // No token, a wrong one, a malformed one.
    let (status, body) = refused(&srv, Some("tether.v1")).await;
    assert_eq!((status, body.code), (401, ErrorCode::Unauthorized));
    let (status, body) = refused(&srv, None).await;
    assert_eq!((status, body.code), (401, ErrorCode::Unauthorized));
    let forged = data_encoding::BASE64URL_NOPAD.encode(&[5u8; 32]);
    let (status, body) = refused(&srv, Some(&bearer(&forged))).await;
    assert_eq!((status, body.code), (401, ErrorCode::Unauthorized));
    let (status, body) = refused(&srv, Some(&bearer("short"))).await;
    assert_eq!((status, body.code), (401, ErrorCode::Unauthorized));

    // The password-reset token is not a device session.
    let rec = recover_token(&srv, &a).await;
    let (status, body) = refused(&srv, Some(&bearer(&rec.access_token))).await;
    assert_eq!((status, body.code), (403, ErrorCode::Forbidden));

    // A revoked device is told so.
    let second = add_device(&srv, &a, "Laptop").await;
    let path = format!("/v1/devices/{}", second.device_id);
    assert_eq!(
        delete_auth(&srv, &path, &a.tokens.access_token)
            .await
            .status,
        204
    );
    let (status, body) = refused(&srv, Some(&bearer(&second.access_token))).await;
    assert_eq!((status, body.code), (401, ErrorCode::Revoked));

    // The token must be offered as the subprotocol, the app protocol too.
    let (status, body) = refused(
        &srv,
        Some(&core_proto::ws_bearer_protocol(&a.tokens.access_token)),
    )
    .await;
    assert_eq!((status, body.code), (400, ErrorCode::BadRequest));
}

#[tokio::test]
async fn upgrade_selects_the_app_protocol_never_the_bearer() {
    let srv = spawn().await;
    let a = register(&srv, "alice").await;
    let token = &a.tokens.access_token;
    // Either order of offers.
    for protocols in [
        format!("tether.v1, bearer.{token}"),
        format!("bearer.{token},tether.v1"),
    ] {
        let (_ws, res) = ws_open_with(&srv, Some(&protocols)).await.unwrap();
        let chosen: Vec<_> = res
            .headers()
            .get_all("sec-websocket-protocol")
            .iter()
            .map(|v| v.to_str().unwrap().to_owned())
            .collect();
        assert_eq!(chosen, vec!["tether.v1".to_owned()]);
        assert!(!format!("{:?}", res.headers()).contains(token.as_str()));
    }
}

#[tokio::test]
async fn socket_is_pinged_and_closed_when_revoked_without_a_message() {
    // A short ping interval, and a revoke that publishes nothing (as if the
    // message had been missed): the next tick notices.
    let srv = spawn_with_state(|st| st.limits.ws_ping = Duration::from_millis(150)).await;
    let a = register(&srv, "alice").await;
    let second = add_device(&srv, &a, "Laptop").await;
    let mut ws = ws_connect(&srv, &second.access_token).await;

    sqlx::query("UPDATE devices SET revoked = 1 WHERE id = ?")
        .bind(&second.device_id)
        .execute(&srv.state.db)
        .await
        .unwrap();
    assert_eq!(ws_next(&mut ws, WAIT).await, WsNext::Event(Event::Revoked));
    assert_eq!(ws_next(&mut ws, WAIT).await, WsNext::Closed);
}

#[tokio::test]
async fn silent_peer_is_dropped() {
    use futures_util::StreamExt;
    use tokio_tungstenite::tungstenite::Message;
    let srv = spawn_with_state(|st| st.limits.ws_ping = Duration::from_millis(100)).await;
    let a = register(&srv, "alice").await;
    let mut ws = ws_connect(&srv, &a.tokens.access_token).await;
    // The client never reads, so it never answers a ping. The server gives up
    // after a couple of intervals and the socket ends.
    tokio::time::sleep(Duration::from_millis(700)).await;
    let mut ended = false;
    for _ in 0..50 {
        match tokio::time::timeout(Duration::from_secs(2), ws.next()).await {
            Ok(None) | Ok(Some(Err(_))) | Ok(Some(Ok(Message::Close(_)))) => {
                ended = true;
                break;
            }
            Ok(Some(Ok(_))) => {}
            Err(_) => break,
        }
    }
    assert!(ended, "the idle socket should have been closed");
}
