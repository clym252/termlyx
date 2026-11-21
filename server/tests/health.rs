//! `/v1/health`, protocol-version middleware and error-body normalisation.

mod common;

use common::*;
use core_proto::{ErrorBody, ErrorCode, PROTOCOL_VERSION};

#[tokio::test]
async fn health_ok() {
    let srv = spawn().await;
    let r = srv.get("/v1/health").await;
    assert_eq!(r.status, 200);
    let v: serde_json::Value = r.json();
    assert_eq!(v["status"], "ok");
    assert_eq!(v["protocol"], PROTOCOL_VERSION);
    let epoch = v["epoch"].as_str().expect("epoch is a string");
    assert_eq!(epoch.len(), 32, "16 random bytes, hex");
    assert_eq!(epoch, srv.state.epoch);
}

#[tokio::test]
async fn epoch_is_created_once_and_survives_restart() {
    let srv = spawn().await;
    let before = srv.state.epoch.clone();
    let srv = srv.restart().await;
    assert_eq!(srv.state.epoch, before);
    let v: serde_json::Value = srv.get("/v1/health").await.json();
    assert_eq!(v["epoch"], before);
}

#[tokio::test]
async fn protocol_header_on_every_response() {
    let srv = spawn().await;
    // Success, unknown route, wrong method and a rejected version all carry it.
    let mut all = vec![
        srv.get("/v1/health").await,
        srv.get("/v1/nope").await,
        srv.request("DELETE", "/v1/health", &[], None).await,
        srv.request("GET", "/v1/health", &[("X-Tether-Protocol", "2")], None)
            .await,
    ];
    all.push(
        srv.request("GET", "/v1/health", &[("X-Tether-Protocol", "1")], None)
            .await,
    );
    for r in all {
        assert_eq!(
            r.headers
                .get("x-tether-protocol")
                .map(|v| v.to_str().unwrap()),
            Some("1"),
            "status {}",
            r.status
        );
    }
}

#[tokio::test]
async fn client_with_newer_major_gets_update_server() {
    let srv = spawn().await;
    let r = srv
        .request("GET", "/v1/health", &[("X-Tether-Protocol", "2")], None)
        .await;
    assert_eq!(r.status, 400);
    let body: ErrorBody = r.json();
    assert_eq!(body.code, ErrorCode::UpdateServer);
    assert!(!body.message.is_empty());
    // "2.1" is major 2 as well.
    let r = srv
        .request("GET", "/v1/health", &[("X-Tether-Protocol", "2.1")], None)
        .await;
    assert_eq!(r.json::<ErrorBody>().code, ErrorCode::UpdateServer);
}

#[tokio::test]
async fn current_missing_and_malformed_protocol_headers() {
    let srv = spawn().await;
    // Missing header (curl, health checks) and the current major are fine.
    assert_eq!(srv.get("/v1/health").await.status, 200);
    let r = srv
        .request("GET", "/v1/health", &[("X-Tether-Protocol", "1")], None)
        .await;
    assert_eq!(r.status, 200);
    // Garbage is a bad request, not a crash.
    let r = srv
        .request(
            "GET",
            "/v1/health",
            &[("X-Tether-Protocol", "banana")],
            None,
        )
        .await;
    assert_eq!(r.status, 400);
    assert_eq!(r.json::<ErrorBody>().code, ErrorCode::BadRequest);
}

#[tokio::test]
async fn errors_are_json_error_bodies() {
    let srv = spawn().await;
    let r = srv.get("/v1/nope").await;
    assert_eq!(r.status, 404);
    assert_eq!(r.json::<ErrorBody>().code, ErrorCode::NotFound);
    let r = srv.request("DELETE", "/v1/health", &[], None).await;
    assert_eq!(r.status, 405);
    assert_eq!(r.json::<ErrorBody>().code, ErrorCode::BadRequest);
}

#[tokio::test]
async fn oversized_body_is_payload_too_large() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let srv = spawn().await;
    // Declare a body over the 4 MiB limit and send no bytes: the limit is
    // enforced from Content-Length before anything is read.
    let mut s = tokio::net::TcpStream::connect(srv.addr()).await.unwrap();
    s.write_all(
        b"POST /v1/health HTTP/1.1\r\nHost: x\r\nContent-Length: 5242880\r\nConnection: close\r\n\r\n",
    )
    .await
    .unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).await.unwrap();
    assert!(out.starts_with("HTTP/1.1 413"), "{out}");
    assert!(out.contains("payload_too_large"), "{out}");
}
