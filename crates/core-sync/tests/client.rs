//! The transport against a real in-process server (`spawn_tls`), plus a few
//! stub servers for answers the real one never gives (a newer protocol, a
//! 401 that keeps coming).

use std::pin::Pin as StdPin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use core_model::Id;
use core_proto::{
    AccountMaterial, DeviceInfo, ErrorCode, Event, KdfWire, LoginReq, PasswordChangeReq,
    PreloginReq, PushItem, PushReq, PushResult, RecoverReq, RecoveryResetReq, RecoveryUpdateReq,
    RegisterReq, TokenResp, WireRow,
};
use core_sync::{
    Client, DeviceCredentials, MemoryTokenStore, Pin, SyncError, TokenStore, parse_join_code, ws,
};
use futures_util::{Stream, StreamExt};
use secrecy::{ExposeSecret, SecretString};
use tether_server::testing::{TestServer, spawn, spawn_tls, spawn_with_state};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const WAIT: Duration = Duration::from_secs(5);
const AUTH_KEY: [u8; 32] = [1; 32];
const RECOVERY_KEY: [u8; 32] = [2; 32];

fn pin_of(srv: &TestServer) -> Option<Pin> {
    (srv.pin != [0; 32]).then(|| Pin::new(srv.pin))
}

fn material() -> AccountMaterial {
    AccountMaterial {
        vault_id: uuid::Uuid::now_v7().to_string(),
        key_version: 1,
        kdf: kdf(),
        account_public: vec![1; 32],
        secret_by_kek: vec![2; 73],
        secret_by_recovery: vec![3; 73],
        sealed_vault_key: vec![4; 81],
    }
}

fn kdf() -> KdfWire {
    KdfWire {
        m_kib: 65536,
        t: 3,
        p: 4,
        salt: vec![7; 16],
    }
}

fn device_info(name: &str) -> DeviceInfo {
    DeviceInfo {
        name: name.into(),
        platform: "macos".into(),
    }
}

/// A signed-in device: its client and the store holding its credentials.
struct Dev {
    client: Client,
    store: Arc<MemoryTokenStore>,
    id: Id,
    /// The response that created the device (access/refresh/device secret).
    tokens: TokenResp,
}

fn new_client(srv: &TestServer) -> (Client, Arc<MemoryTokenStore>) {
    let store = Arc::new(MemoryTokenStore::new());
    let client = Client::new(&srv.url, pin_of(srv))
        .unwrap()
        .with_store(store.clone());
    (client, store)
}

/// Register `alice` with a fresh invite; the first device.
async fn register(srv: &TestServer) -> Dev {
    let (client, store) = new_client(srv);
    let invite = parse_join_code(&srv.make_invite().await).unwrap();
    let resp = client
        .register(&RegisterReq {
            invite_token: invite.token.to_vec(),
            username: "alice".into(),
            auth_key: AUTH_KEY.to_vec(),
            device: device_info("First"),
            material: material(),
            recovery_auth_key: RECOVERY_KEY.to_vec(),
            node_hint: 1,
            code_proof: None,
        })
        .await
        .expect("register");
    assert!(!resp.device_secret.is_empty());
    client.adopt(&resp).await.expect("adopt");
    Dev {
        client,
        store,
        id: resp.device_id.parse().unwrap(),
        tokens: resp,
    }
}

/// Add a second device through an add-device code.
async fn add_device(srv: &TestServer, first: &Dev) -> Dev {
    let link = first.client.create_link().await.expect("create_link");
    let code = parse_join_code(&link.code).unwrap();
    let (client, store) = new_client(srv);
    let resp = client
        .login(&LoginReq {
            username: "alice".into(),
            auth_key: AUTH_KEY.to_vec(),
            device: device_info("Second"),
            link_token: Some(code.token.to_vec()),
        })
        .await
        .expect("link login");
    assert!(!resp.device_secret.is_empty());
    client.adopt(&resp).await.expect("adopt");
    Dev {
        client,
        store,
        id: resp.device_id.parse().unwrap(),
        tokens: resp,
    }
}

fn creds(store: &MemoryTokenStore, id: Id) -> (String, String) {
    let c = store.load(id).unwrap().expect("credentials saved");
    (
        c.refresh.expose_secret().to_owned(),
        c.device_secret.expose_secret().to_owned(),
    )
}

fn row(hlc_wall: i64, envelope: &[u8]) -> WireRow {
    WireRow {
        id: uuid::Uuid::now_v7().to_string(),
        kind: "host".into(),
        key_version: 1,
        hlc: format!("{hlc_wall:016x}-{:08x}-{:04x}", 0, 1),
        rev: None,
        seq: None,
        deleted: false,
        envelope: envelope.to_vec(),
    }
}

fn now_ms() -> i64 {
    tether_server::util::system_now_ms()
}

async fn next<S: Stream + Unpin>(s: &mut S) -> Option<S::Item> {
    tokio::time::timeout(WAIT, s.next())
        .await
        .expect("timed out waiting for the stream")
}

// -- stub server ------------------------------------------------------------

struct Reply {
    status: u16,
    headers: Vec<(&'static str, &'static str)>,
    body: String,
}

fn reply(status: u16, body: &str) -> Reply {
    Reply {
        status,
        headers: vec![],
        body: body.into(),
    }
}

fn error_reply(status: u16, code: &str) -> Reply {
    reply(
        status,
        &format!(r#"{{"code":"{code}","message":"stub message"}}"#),
    )
}

/// A plain-HTTP loopback server answering every request through `handler`
/// (given "METHOD path"); returns its URL and the log of requests.
async fn stub(
    handler: impl Fn(&str) -> Reply + Send + Sync + 'static,
) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let log = Arc::new(Mutex::new(vec![]));
    let handler = Arc::new(handler);
    let seen = log.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let (handler, seen) = (handler.clone(), seen.clone());
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                let head_end = loop {
                    let n = sock.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        break i + 4;
                    }
                };
                let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
                let want: usize = head
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(str::to_owned)
                    })
                    .and_then(|v| v.trim().parse().ok())
                    .unwrap_or(0);
                while buf.len() < head_end + want {
                    let n = sock.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                let mut parts = head.split_whitespace();
                let line = format!(
                    "{} {}",
                    parts.next().unwrap_or(""),
                    parts.next().unwrap_or("")
                );
                seen.lock().unwrap().push(line.clone());
                let r = handler(&line);
                let mut out = format!(
                    "HTTP/1.1 {} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
                    r.status,
                    r.body.len()
                );
                for (k, v) in r.headers {
                    out.push_str(&format!("{k}: {v}\r\n"));
                }
                out.push_str("\r\n");
                out.push_str(&r.body);
                let _ = sock.write_all(out.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    (url, log)
}

fn token_json(access: &str, refresh: &str) -> String {
    serde_json::to_string(&TokenResp {
        access_token: access.into(),
        refresh_token: refresh.into(),
        expires_in: 900,
        device_id: uuid::Uuid::from_u128(9).to_string(),
        node: 1,
        epoch: "e".into(),
        device_secret: String::new(),
    })
    .unwrap()
}

/// A client on a stub server that already holds a session.
async fn stub_session(url: &str) -> Client {
    let client = Client::new(url, None).unwrap();
    client
        .adopt(&TokenResp {
            access_token: "access-0".into(),
            refresh_token: "refresh-0".into(),
            expires_in: 900,
            device_id: uuid::Uuid::from_u128(9).to_string(),
            node: 1,
            epoch: "e".into(),
            device_secret: "secret-0".into(),
        })
        .await
        .unwrap();
    client
}

// -- tests ------------------------------------------------------------------

#[tokio::test]
async fn pinned_client_talks_to_server() {
    let srv = spawn_tls().await;
    assert!(srv.url.starts_with("https://"));
    let dev = register(&srv).await;

    // Unauthenticated and authenticated calls both work over the pinned TLS.
    let pre = dev
        .client
        .prelogin(&PreloginReq {
            username: "alice".into(),
        })
        .await
        .unwrap();
    assert_eq!(pre.kdf, kdf());
    let devices = dev.client.devices().await.unwrap();
    assert_eq!(devices.len(), 1);
    assert!(devices[0].current);
    assert_eq!(devices[0].id, dev.id.to_string());
    assert_eq!(dev.client.account().await.unwrap().account_rev, 1);

    // The credentials went to the store: a refresh token and the secret.
    let (refresh, secret) = creds(&dev.store, dev.id);
    assert!(!refresh.is_empty() && !secret.is_empty());
    assert!(!format!("{:?}", dev.client).contains(&refresh));
}

#[tokio::test]
async fn pin_mismatch_is_distinct_error() {
    let a = spawn_tls().await;
    let b = spawn_tls().await;
    assert_ne!(a.pin, b.pin);

    // A's pin against B: not "unreachable", and not any other error.
    let wrong = Client::new(&b.url, Some(Pin::new(a.pin))).unwrap();
    let err = wrong
        .prelogin(&PreloginReq {
            username: "alice".into(),
        })
        .await
        .unwrap_err();
    assert_eq!(err, SyncError::CertificateChanged);
    assert_eq!(err.to_string(), "server certificate changed");

    // The right pin works on the same server.
    let right = Client::new(&b.url, Some(Pin::new(b.pin))).unwrap();
    right
        .prelogin(&PreloginReq {
            username: "alice".into(),
        })
        .await
        .unwrap();

    // The WebSocket path reports it the same way.
    let dev = register(&b).await;
    let attacker = Client::new(&b.url, Some(Pin::new(a.pin))).unwrap();
    attacker
        .adopt(&TokenResp {
            access_token: "x".into(),
            refresh_token: "y".into(),
            expires_in: 900,
            device_id: dev.id.to_string(),
            node: 1,
            epoch: "e".into(),
            device_secret: "z".into(),
        })
        .await
        .unwrap();
    let mut stream = Box::pin(ws::connect(&attacker));
    assert_eq!(
        next(&mut stream).await.unwrap().unwrap_err(),
        SyncError::CertificateChanged
    );
    assert!(next(&mut stream).await.is_none());
}

#[tokio::test]
async fn unreachable_is_distinct_error() {
    // A port nothing listens on.
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    for url in [
        format!("https://127.0.0.1:{port}"),
        format!("http://127.0.0.1:{port}"),
    ] {
        let pin = url.starts_with("https").then_some(Pin::new([9; 32]));
        let client = Client::new(&url, pin).unwrap();
        let err = client
            .prelogin(&PreloginReq {
                username: "alice".into(),
            })
            .await
            .unwrap_err();
        let SyncError::Unreachable(reason) = &err else {
            panic!("expected Unreachable, got {err:?}");
        };
        assert_eq!(err.to_string(), "server unreachable");
        // A short reason; no address, URL or port leaks into it.
        assert!(!reason.is_empty() && reason.len() < 80, "{reason}");
        assert!(!reason.contains("127.0.0.1") && !reason.contains(&port.to_string()));
    }

    // Same for the WebSocket.
    let client = Client::new(&format!("http://127.0.0.1:{port}"), None).unwrap();
    client
        .adopt(&TokenResp {
            access_token: "x".into(),
            refresh_token: "y".into(),
            expires_in: 900,
            device_id: uuid::Uuid::from_u128(1).to_string(),
            node: 1,
            epoch: "e".into(),
            device_secret: "z".into(),
        })
        .await
        .unwrap();
    let mut stream = Box::pin(ws::connect(&client));
    assert!(matches!(
        next(&mut stream).await.unwrap(),
        Err(SyncError::Unreachable(_))
    ));
}

#[tokio::test]
async fn refresh_on_expired_access_token() {
    let srv = spawn_tls().await;
    let dev = register(&srv).await;
    let before = creds(&dev.store, dev.id);
    dev.client.devices().await.unwrap();
    assert_eq!(creds(&dev.store, dev.id), before, "no refresh needed yet");

    // 20 minutes later the 15 minute access token is dead on the server.
    srv.set_clock(now_ms() + 20 * 60 * 1000);
    let devices = dev.client.devices().await.expect("refreshed and retried");
    assert_eq!(devices.len(), 1);

    // The refresh token rotated and was saved; the device secret is kept.
    let after = creds(&dev.store, dev.id);
    assert_ne!(after.0, before.0, "refresh token rotated");
    assert_eq!(after.1, before.1, "device secret unchanged");
    assert!(!dev.client.credentials_unsaved());

    // The new session keeps working (and rotated tokens are not reused).
    dev.client.devices().await.unwrap();
    dev.client.account().await.unwrap();
    assert_eq!(creds(&dev.store, dev.id), after);
}

#[tokio::test]
async fn concurrent_requests_share_one_refresh() {
    let srv = spawn_tls().await;
    let dev = register(&srv).await;
    srv.set_clock(now_ms() + 20 * 60 * 1000);
    // Two refreshes with one single-use token would revoke the device.
    let (a, b, c) = tokio::join!(
        dev.client.devices(),
        dev.client.account(),
        dev.client.create_link()
    );
    a.unwrap();
    b.unwrap();
    c.unwrap();
    dev.client.devices().await.unwrap();
}

#[tokio::test]
async fn revoked_is_not_retried() {
    // Real server: the device is revoked, every call says so.
    let srv = spawn_tls().await;
    let first = register(&srv).await;
    let second = add_device(&srv, &first).await;
    first.client.revoke_device(second.id).await.unwrap();
    assert_eq!(
        second.client.devices().await.unwrap_err(),
        SyncError::Revoked
    );
    assert_eq!(
        second.client.refresh().await.unwrap_err(),
        SyncError::Revoked
    );
    assert_eq!(SyncError::Revoked.to_string(), "device revoked");
    // The first device is unaffected.
    assert_eq!(first.client.devices().await.unwrap().len(), 1);

    // Stub: one request, no refresh, no retry.
    let (url, log) = stub(|_| error_reply(401, "revoked")).await;
    let client = stub_session(&url).await;
    assert_eq!(client.devices().await.unwrap_err(), SyncError::Revoked);
    assert_eq!(*log.lock().unwrap(), vec!["GET /v1/devices"]);
}

#[tokio::test]
async fn unauthorized_refreshes_once_then_gives_up() {
    // The refresh itself is refused: Unauthorized, two requests.
    let (url, log) = stub(|_| error_reply(401, "unauthorized")).await;
    let client = stub_session(&url).await;
    assert_eq!(client.devices().await.unwrap_err(), SyncError::Unauthorized);
    assert_eq!(
        *log.lock().unwrap(),
        vec!["GET /v1/devices", "POST /v1/refresh"]
    );

    // The refresh works but the retry is refused again: one refresh only.
    let (url, log) = stub(|line| match line {
        "POST /v1/refresh" => reply(200, &token_json("access-1", "refresh-1")),
        _ => error_reply(401, "unauthorized"),
    })
    .await;
    let client = stub_session(&url).await;
    assert_eq!(client.devices().await.unwrap_err(), SyncError::Unauthorized);
    assert_eq!(
        *log.lock().unwrap(),
        vec!["GET /v1/devices", "POST /v1/refresh", "GET /v1/devices"]
    );

    // A client with no session cannot refresh at all.
    let (url, log) = stub(|_| error_reply(401, "unauthorized")).await;
    let anonymous = Client::new(&url, None).unwrap();
    assert_eq!(
        anonymous.devices().await.unwrap_err(),
        SyncError::Unauthorized
    );
    assert!(log.lock().unwrap().len() <= 1);
}

#[tokio::test]
async fn newer_server_protocol_is_update_client() {
    // A server whose answers carry a newer protocol major.
    let (url, _) = stub(|_| Reply {
        status: 200,
        headers: vec![("X-Tether-Protocol", "2")],
        body: r#"{"kdf":{"mKib":1,"t":1,"p":1,"salt":"AAAAAAAAAAAAAAAAAAAAAA=="}}"#.into(),
    })
    .await;
    let client = Client::new(&url, None).unwrap();
    let err = client
        .prelogin(&PreloginReq {
            username: "alice".into(),
        })
        .await
        .unwrap_err();
    assert_eq!(err, SyncError::UpdateClient);
    assert_eq!(err.to_string(), "update Termlyx");

    // The explicit error code means the same.
    let (url, _) = stub(|_| error_reply(400, "update_client")).await;
    let client = Client::new(&url, None).unwrap();
    assert_eq!(
        client
            .prelogin(&PreloginReq {
                username: "alice".into()
            })
            .await
            .unwrap_err(),
        SyncError::UpdateClient
    );

    // A join code from a newer app.
    let mut bytes = vec![2u8, 0, 0];
    bytes.extend_from_slice(&[7; 32]);
    bytes.extend_from_slice(&[0, 0]);
    let code = format!(
        "tether1-{}",
        data_encoding::BASE32_NOPAD.encode(&bytes).to_lowercase()
    );
    assert_eq!(parse_join_code(&code).unwrap_err(), SyncError::UpdateClient);
    assert!(matches!(
        parse_join_code("hello"),
        Err(SyncError::InvalidCode(_))
    ));
}

#[tokio::test]
async fn server_error_codes_map_to_variants() {
    for (status, code, want) in [
        (400, "update_server", SyncError::UpdateServer),
        (400, "clock_skew", SyncError::ClockSkew),
        (410, "gone", SyncError::Gone),
        (429, "rate_limited", SyncError::RateLimited),
        (
            409,
            "conflict",
            SyncError::Server {
                code: ErrorCode::Conflict,
                message: "stub message".into(),
            },
        ),
    ] {
        let (url, _) = stub(move |_| error_reply(status, code)).await;
        let client = Client::new(&url, None).unwrap();
        let err = client
            .prelogin(&PreloginReq {
                username: "alice".into(),
            })
            .await
            .unwrap_err();
        assert_eq!(err, want, "{code}");
    }
    // Not an error body at all (a proxy's page).
    let (url, _) = stub(|_| reply(502, "<html>bad gateway</html>")).await;
    let err = Client::new(&url, None)
        .unwrap()
        .prelogin(&PreloginReq {
            username: "alice".into(),
        })
        .await
        .unwrap_err();
    assert!(matches!(err, SyncError::Protocol(_)), "{err:?}");
}

#[tokio::test]
async fn link_login_adds_a_device_and_relogin_needs_the_secret() {
    let srv = spawn_tls().await;
    let first = register(&srv).await;
    let second = add_device(&srv, &first).await;
    assert_eq!(first.client.devices().await.unwrap().len(), 2);
    let (_, secret) = creds(&second.store, second.id);
    assert!(!secret.is_empty());

    // Sign out, then sign back in with the password and the device secret.
    second.client.logout().await.unwrap();
    let login = LoginReq {
        username: "alice".into(),
        auth_key: AUTH_KEY.to_vec(),
        device: device_info("Second"),
        link_token: None,
    };
    let resp = second.client.login(&login).await.expect("re-login");
    assert!(resp.device_secret.is_empty());
    assert_eq!(resp.device_id, second.id.to_string());
    second.client.adopt(&resp).await.unwrap();
    assert_eq!(creds(&second.store, second.id).1, secret, "secret kept");
    assert_eq!(second.client.devices().await.unwrap().len(), 2);

    // A fresh process: resume from the store, then log in.
    let (fresh, _) = new_client(&srv);
    fresh.set_store(second.store.clone());
    assert!(fresh.resume(second.id).await.unwrap());
    fresh.login(&login).await.expect("re-login after restart");
    assert!(!fresh.resume(Id::from_u128(1)).await.unwrap());

    // The password alone never works: no device, no secret.
    let (bare, _) = new_client(&srv);
    let err = bare.login(&login).await.unwrap_err();
    assert!(
        matches!(
            err,
            SyncError::Server {
                code: ErrorCode::Forbidden,
                ..
            }
        ),
        "{err:?}"
    );

    // A wrong key is Unauthorized.
    let wrong = LoginReq {
        auth_key: vec![9; 32],
        ..login
    };
    assert_eq!(
        fresh.login(&wrong).await.unwrap_err(),
        SyncError::Unauthorized
    );
}

/// A store that fails saves while `fail` is set.
struct FlakyStore {
    inner: MemoryTokenStore,
    fail: Arc<AtomicBool>,
    /// Fail this many more saves, then work again.
    fail_next: AtomicUsize,
}

impl TokenStore for FlakyStore {
    fn load(&self, device: Id) -> Result<Option<DeviceCredentials>, SyncError> {
        self.inner.load(device)
    }
    fn save(&self, device: Id, creds: &DeviceCredentials) -> Result<(), SyncError> {
        let counted = self
            .fail_next
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok();
        if counted || self.fail.load(Ordering::SeqCst) {
            return Err(SyncError::Storage("keychain locked".into()));
        }
        self.inner.save(device, creds)
    }
    fn delete(&self, device: Id) -> Result<(), SyncError> {
        self.inner.delete(device)
    }
}

#[tokio::test]
async fn a_failed_save_does_not_lose_the_rotated_token() {
    let srv = spawn_tls().await;
    let fail = Arc::new(AtomicBool::new(false));
    let store = Arc::new(FlakyStore {
        inner: MemoryTokenStore::new(),
        fail: fail.clone(),
        fail_next: AtomicUsize::new(0),
    });
    let client = Client::new(&srv.url, pin_of(&srv))
        .unwrap()
        .with_store(store.clone());
    let invite = parse_join_code(&srv.make_invite().await).unwrap();
    let resp = client
        .register(&RegisterReq {
            invite_token: invite.token.to_vec(),
            username: "alice".into(),
            auth_key: AUTH_KEY.to_vec(),
            device: device_info("First"),
            material: material(),
            recovery_auth_key: RECOVERY_KEY.to_vec(),
            node_hint: 1,
            code_proof: None,
        })
        .await
        .unwrap();
    let id: Id = resp.device_id.parse().unwrap();

    // adopt saves first: when that fails, nothing is installed.
    fail.store(true, Ordering::SeqCst);
    assert!(matches!(
        client.adopt(&resp).await,
        Err(SyncError::Storage(_))
    ));
    assert!(client.device().is_none());
    fail.store(false, Ordering::SeqCst);
    client.adopt(&resp).await.unwrap();
    let saved = store.load(id).unwrap().unwrap();

    // A rotation whose save fails still takes effect in memory, but the
    // caller is told...
    fail.store(true, Ordering::SeqCst);
    assert!(matches!(client.refresh().await, Err(SyncError::Storage(_))));
    assert!(client.credentials_unsaved());
    // ...and calls do not carry on while the store keeps refusing (the store
    // still holds the rotated-out token).
    assert!(matches!(client.devices().await, Err(SyncError::Storage(_))));
    assert_eq!(
        store.load(id).unwrap().unwrap().refresh.expose_secret(),
        saved.refresh.expose_secret()
    );
    assert!(matches!(client.persist().await, Err(SyncError::Storage(_))));
    // Once the store works, the next call saves first and goes through.
    fail.store(false, Ordering::SeqCst);
    assert_eq!(client.devices().await.unwrap().len(), 1);
    assert!(!client.credentials_unsaved());
    let now = store.load(id).unwrap().unwrap();
    assert_ne!(now.refresh.expose_secret(), saved.refresh.expose_secret());

    // The saved token is the live one: a restart can refresh with it.
    let (restarted, _) = new_client(&srv);
    restarted.set_store(store.clone());
    assert!(restarted.resume(id).await.unwrap());
    assert_eq!(restarted.devices().await.unwrap().len(), 1);
}

#[tokio::test]
async fn account_and_sync_endpoints_round_trip() {
    let srv = spawn_tls().await;
    let first = register(&srv).await;
    let second = add_device(&srv, &first).await;
    let c = &first.client;

    // Devices.
    c.rename_device(second.id, "Laptop").await.unwrap();
    let names: Vec<_> = c
        .devices()
        .await
        .unwrap()
        .into_iter()
        .map(|d| d.name)
        .collect();
    assert_eq!(names, ["First", "Laptop"]);

    // Records.
    assert!(c.changes(0, 500).await.unwrap().changes.is_empty());
    let pushed = row(now_ms(), b"sealed-1");
    let resp = c
        .push(&PushReq {
            items: vec![PushItem {
                row: pushed.clone(),
                base_rev: None,
            }],
        })
        .await
        .unwrap();
    assert!(matches!(
        resp.results[0],
        PushResult::Ok { rev: 1, seq: 1, .. }
    ));
    let page = c.changes(0, 500).await.unwrap();
    assert_eq!(page.changes.len(), 1);
    assert_eq!(page.changes[0].id, pushed.id);
    assert_eq!(page.changes[0].envelope, b"sealed-1");
    assert!(!page.more);
    // A clock far ahead is refused with its own error.
    let skewed = c
        .push(&PushReq {
            items: vec![PushItem {
                row: row(now_ms() + 3 * 60 * 60 * 1000, b"x"),
                base_rev: None,
            }],
        })
        .await
        .unwrap_err();
    assert_eq!(skewed, SyncError::ClockSkew);
    // A base revision for a record the server lacks: gone.
    let gone = c
        .push(&PushReq {
            items: vec![PushItem {
                row: row(now_ms(), b"x"),
                base_rev: Some(3),
            }],
        })
        .await
        .unwrap_err();
    assert_eq!(gone, SyncError::Gone);

    // Account material and password change.
    let account = c.account().await.unwrap();
    assert_eq!(account.account_rev, 1);
    let changed = c
        .change_password(&PasswordChangeReq {
            old_auth_key: AUTH_KEY.to_vec(),
            new_auth_key: vec![5; 32],
            kdf: kdf(),
            secret_by_kek: vec![6; 73],
            account_rev_base: 1,
        })
        .await
        .unwrap();
    assert_eq!(changed.account_rev, 2);
    let stale = c
        .change_password(&PasswordChangeReq {
            old_auth_key: vec![5; 32],
            new_auth_key: vec![6; 32],
            kdf: kdf(),
            secret_by_kek: vec![6; 73],
            account_rev_base: 1,
        })
        .await
        .unwrap_err();
    assert!(matches!(
        stale,
        SyncError::Server {
            code: ErrorCode::Conflict,
            ..
        }
    ));

    // Replace the recovery key (proves the current auth key).
    let updated = c
        .put_recovery(&RecoveryUpdateReq {
            auth_key: vec![5; 32],
            recovery_auth_key: vec![8; 32],
            secret_by_recovery: vec![9; 73],
        })
        .await
        .unwrap();
    assert_eq!(updated.account_rev, 3);

    // Recovery: limited token, read the account, reset the password.
    let (recovering, _) = new_client(&srv);
    let limited = recovering
        .recover_login(&RecoverReq {
            username: "alice".into(),
            recovery_auth_key: vec![8; 32],
            device: device_info("Recovery"),
        })
        .await
        .unwrap();
    assert!(limited.refresh_token.is_empty());
    recovering.adopt(&limited).await.unwrap();
    assert!(recovering.device().is_none());
    assert_eq!(recovering.account().await.unwrap().account_rev, 3);
    // The limited token is not good for anything else.
    assert!(matches!(
        recovering.devices().await.unwrap_err(),
        SyncError::Server {
            code: ErrorCode::Forbidden,
            ..
        }
    ));
    let reset = recovering
        .recovery_reset(&RecoveryResetReq {
            recovery_auth_key: vec![8; 32],
            new_auth_key: vec![10; 32],
            kdf: kdf(),
            secret_by_kek: vec![11; 73],
        })
        .await
        .unwrap();
    assert_eq!(reset.account_rev, 4);

    // Other devices keep their sessions through all of it.
    assert_eq!(second.client.devices().await.unwrap().len(), 2);
    // Revoking a device: revoking itself works too.
    c.revoke_device(second.id).await.unwrap();
    assert_eq!(c.devices().await.unwrap().len(), 1);
}

#[tokio::test]
async fn ws_receives_changed_event() {
    let srv = spawn_tls().await;
    let first = register(&srv).await;
    let second = add_device(&srv, &first).await;

    let mut events = Box::pin(ws::connect(&second.client));
    // Give the upgrade a moment, then push from the other device.
    let pusher = tokio::spawn({
        let client = first.client.clone();
        async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            for n in 0..2 {
                client
                    .push(&PushReq {
                        items: vec![PushItem {
                            row: row(now_ms(), format!("v{n}").as_bytes()),
                            base_rev: None,
                        }],
                    })
                    .await
                    .unwrap();
            }
        }
    });
    assert_eq!(
        next(&mut events).await.unwrap().unwrap(),
        Event::Changed { seq: 1 }
    );
    assert_eq!(
        next(&mut events).await.unwrap().unwrap(),
        Event::Changed { seq: 2 }
    );
    pusher.await.unwrap();

    // Account changes reach every device.
    first
        .client
        .change_password(&PasswordChangeReq {
            old_auth_key: AUTH_KEY.to_vec(),
            new_auth_key: vec![5; 32],
            kdf: kdf(),
            secret_by_kek: vec![6; 73],
            account_rev_base: 1,
        })
        .await
        .unwrap();
    assert_eq!(
        next(&mut events).await.unwrap().unwrap(),
        Event::AccountChanged { account_rev: 2 }
    );

    // Revoking the device: `revoked`, then the stream ends.
    first.client.revoke_device(second.id).await.unwrap();
    assert_eq!(next(&mut events).await.unwrap().unwrap(), Event::Revoked);
    assert!(next(&mut events).await.is_none());
}

#[tokio::test]
async fn ws_refreshes_an_expired_token_and_reports_refusals() {
    let srv = spawn_tls().await;
    let first = register(&srv).await;
    let second = add_device(&srv, &first).await;
    let before = creds(&second.store, second.id);

    // The server's clock moved on: the upgrade is refused as unauthorized,
    // the client refreshes and retries once.
    srv.set_clock(now_ms() + 20 * 60 * 1000);
    let mut events = Box::pin(ws::connect(&second.client));
    let pusher = tokio::spawn({
        let client = first.client.clone();
        async move {
            tokio::time::sleep(Duration::from_millis(500)).await;
            client
                .push(&PushReq {
                    items: vec![PushItem {
                        row: row(now_ms() + 20 * 60 * 1000, b"v"),
                        base_rev: None,
                    }],
                })
                .await
                .unwrap();
        }
    });
    assert_eq!(
        next(&mut events).await.unwrap().unwrap(),
        Event::Changed { seq: 1 }
    );
    pusher.await.unwrap();
    assert_ne!(creds(&second.store, second.id).0, before.0);

    // A revoked device gets `Revoked` as the stream's error, then it ends.
    first.client.revoke_device(second.id).await.unwrap();
    let mut again = Box::pin(ws::connect(&second.client));
    assert_eq!(
        next(&mut again).await.unwrap().unwrap_err(),
        SyncError::Revoked
    );
    assert!(next(&mut again).await.is_none());
}

#[tokio::test]
async fn ws_stays_open_by_answering_pings() {
    // The server pings every 100 ms and drops a peer silent for two
    // intervals: 600 ms of quiet only survives if pongs go out.
    let srv = spawn_with_state(|st| st.limits.ws_ping = Duration::from_millis(100)).await;
    let first = register(&srv).await;
    let second = add_device(&srv, &first).await;
    let mut events: StdPin<Box<dyn Stream<Item = Result<Event, SyncError>> + Send>> =
        Box::pin(ws::connect(&second.client));

    tokio::time::sleep(Duration::from_millis(300)).await;
    let pushing = {
        let client = first.client.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(600)).await;
            client
                .push(&PushReq {
                    items: vec![PushItem {
                        row: row(now_ms(), b"late"),
                        base_rev: None,
                    }],
                })
                .await
                .unwrap();
        })
    };
    assert_eq!(
        next(&mut events).await.unwrap().unwrap(),
        Event::Changed { seq: 1 }
    );
    pushing.await.unwrap();
}

#[tokio::test]
async fn plain_http_is_only_for_loopback_without_a_pin() {
    let srv = spawn().await;
    Client::new(&srv.url, None).unwrap();
    for (url, pin) in [
        ("http://example.com", None),
        ("http://127.0.0.1:1", Some(Pin::new([1; 32]))),
        ("https://user:pw@example.com", None),
        ("ftp://example.com", None),
        ("not a url", None),
    ] {
        assert!(Client::new(url, pin).is_err(), "{url}");
    }
    let client = Client::new("https://sync.example.com:8443/tether/", None).unwrap();
    assert_eq!(client.base_url(), "https://sync.example.com:8443/tether");
    // No pin: WebPKI applies, and a self-signed server fails it.
    let tls = spawn_tls().await;
    let err = Client::new(&tls.url, None)
        .unwrap()
        .prelogin(&PreloginReq {
            username: "alice".into(),
        })
        .await
        .unwrap_err();
    assert!(matches!(err, SyncError::Unreachable(_)), "{err:?}");
}

#[test]
fn secrets_do_not_show_in_debug() {
    let c = DeviceCredentials {
        refresh: SecretString::from("CANARY-R".to_owned()),
        device_secret: SecretString::from("CANARY-S".to_owned()),
    };
    assert!(!format!("{c:?}").contains("CANARY"));
}

#[tokio::test]
async fn unsaved_credentials_are_persisted_on_next_call() {
    let srv = spawn_tls().await;
    let fail = Arc::new(AtomicBool::new(false));
    let store = Arc::new(FlakyStore {
        inner: MemoryTokenStore::new(),
        fail,
        fail_next: AtomicUsize::new(0),
    });
    let client = Client::new(&srv.url, pin_of(&srv))
        .unwrap()
        .with_store(store.clone());
    let invite = parse_join_code(&srv.make_invite().await).unwrap();
    let resp = client
        .register(&RegisterReq {
            invite_token: invite.token.to_vec(),
            username: "alice".into(),
            auth_key: AUTH_KEY.to_vec(),
            device: device_info("First"),
            material: material(),
            recovery_auth_key: RECOVERY_KEY.to_vec(),
            node_hint: 1,
            code_proof: None,
        })
        .await
        .unwrap();
    let id: Id = resp.device_id.parse().unwrap();
    client.adopt(&resp).await.unwrap();
    let before = store.load(id).unwrap().unwrap();

    // The automatic rotation's save fails, and so does its immediate retry:
    // the call that rotated reports it (the engine must see it)...
    store.fail_next.store(2, Ordering::SeqCst);
    srv.set_clock(now_ms() + 20 * 60 * 1000);
    assert!(matches!(client.devices().await, Err(SyncError::Storage(_))));
    assert!(client.credentials_unsaved());
    assert_eq!(
        store.load(id).unwrap().unwrap().refresh.expose_secret(),
        before.refresh.expose_secret(),
        "the store still holds the rotated-out token"
    );

    // ...and the next call saves before doing anything else.
    assert_eq!(client.devices().await.unwrap().len(), 1);
    assert!(!client.credentials_unsaved());
    let after = store.load(id).unwrap().unwrap();
    assert_ne!(
        after.refresh.expose_secret(),
        before.refresh.expose_secret()
    );

    // So a restart does not present a dead token (which would revoke us).
    let (restarted, _) = new_client(&srv);
    restarted.set_store(store.clone());
    assert!(restarted.resume(id).await.unwrap());
    assert_eq!(restarted.devices().await.unwrap().len(), 1);
}

// -- logging ----------------------------------------------------------------

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
    type Writer = Capture;
    fn make_writer(&'a self) -> Capture {
        self.clone()
    }
}

/// Everything the log would hold for a session that registers, links a
/// device, listens on the WebSocket, refreshes and reconnects, under the
/// filter `RUST_LOG=trace` gives (plus [`core_sync::LOG_DIRECTIVES`] when
/// asked). Returns the captured text, the secrets that were in play and one
/// token known to be on the wire.
async fn logged_session(apply_directives: bool) -> (String, Vec<String>, String) {
    use tracing_subscriber::EnvFilter;
    // The `log` crate (tungstenite, reqwest) feeds the same subscriber.
    let _ = tracing_log::LogTracer::init();
    let capture = Capture::default();
    let mut filter = EnvFilter::new("trace");
    if apply_directives {
        for d in core_sync::LOG_DIRECTIVES {
            filter = filter.add_directive(d.parse().unwrap());
        }
    }
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(capture.clone())
        .with_ansi(false)
        .finish();
    // Thread-local: the runtime of #[tokio::test] is single-threaded.
    let _guard = tracing::subscriber::set_default(subscriber);

    let srv = spawn_tls().await;
    let first = register(&srv).await;
    let second = add_device(&srv, &first).await;
    let push = |client: Client, wall: i64| async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        client
            .push(&PushReq {
                items: vec![PushItem {
                    row: row(wall, b"sealed"),
                    base_rev: None,
                }],
            })
            .await
            .unwrap();
    };

    let mut events = Box::pin(ws::connect(&second.client));
    let pusher = tokio::spawn(push(first.client.clone(), now_ms()));
    assert!(matches!(
        next(&mut events).await.unwrap().unwrap(),
        Event::Changed { .. }
    ));
    pusher.await.unwrap();
    drop(events);

    // Refresh both devices (rotating tokens) and reconnect with new ones.
    let later = now_ms() + 20 * 60 * 1000;
    srv.set_clock(later);
    first.client.devices().await.unwrap();
    let mut events = Box::pin(ws::connect(&second.client));
    let pusher = tokio::spawn(push(first.client.clone(), later));
    assert!(matches!(
        next(&mut events).await.unwrap().unwrap(),
        Event::Changed { .. }
    ));
    pusher.await.unwrap();

    let mut secrets = vec![];
    for dev in [&first, &second] {
        secrets.push(dev.tokens.access_token.clone());
        secrets.push(dev.tokens.refresh_token.clone());
        secrets.push(dev.tokens.device_secret.clone());
        secrets.push(creds(&dev.store, dev.id).0); // the rotated refresh token
    }
    let on_the_wire = second.tokens.access_token.clone();
    let text = String::from_utf8_lossy(&capture.0.lock().unwrap()).into_owned();
    (text, secrets, on_the_wire)
}

#[tokio::test]
async fn tokens_never_reach_logs() {
    // Control: at `trace` without our directives tungstenite logs the upgrade
    // request, bearer token included. This proves the capture sees the
    // hazard (if a tungstenite upgrade ever stops logging it, drop the
    // directive's reason, not this assertion's point).
    let (text, _, on_the_wire) = logged_session(false).await;
    assert!(
        text.contains(&on_the_wire),
        "control failed: the access token should show up at trace without LOG_DIRECTIVES"
    );

    // With `RUST_LOG=trace` and LOG_DIRECTIVES applied after it: nothing.
    let (text, secrets, _) = logged_session(true).await;
    assert!(!text.is_empty(), "the capture saw no log output at all");
    for secret in secrets {
        assert!(secret.len() >= 20, "test bug: not a real secret");
        assert!(!text.contains(&secret), "a credential reached the log");
    }
}

/// A free loopback port nothing listens on (yet).
async fn closed_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    l.local_addr().unwrap().port()
}

#[tokio::test]
async fn reach_waits_until_the_server_takes_connections() {
    let port = closed_port().await;
    let client = Client::new(&format!("http://127.0.0.1:{port}"), None).unwrap();
    // The server comes up after the first attempts were refused (iOS: the
    // user tapping Allow on the local network prompt).
    let server = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let l = TcpListener::bind(("127.0.0.1", port)).await.unwrap();
        let (mut sock, _) = l.accept().await.unwrap();
        let mut buf = [0u8; 1024];
        let _ = sock.read(&mut buf).await;
        sock.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
            .await
            .unwrap();
    });
    client.reach(Duration::from_secs(10)).await.unwrap();
    server.await.unwrap();
}

#[tokio::test]
async fn reach_gives_up_after_its_patience() {
    let port = closed_port().await;
    let client = Client::new(&format!("http://127.0.0.1:{port}"), None).unwrap();
    let start = std::time::Instant::now();
    let err = client.reach(Duration::from_secs(2)).await.unwrap_err();
    assert!(matches!(err, SyncError::Unreachable(_)), "{err:?}");
    assert!(
        start.elapsed() < Duration::from_secs(4),
        "{:?}",
        start.elapsed()
    );
}

#[tokio::test]
async fn reach_takes_any_answer_as_reached() {
    let srv = spawn().await;
    let client = Client::new(&srv.url, pin_of(&srv)).unwrap();
    client.reach(Duration::ZERO).await.unwrap();
}
