//! Shared helpers for the integration tests: the crate's own `testing` module
//! plus a small account/session client.
#![allow(unused_imports, dead_code)]

pub use tether_server::testing::*;

use core_proto::{
    AccountMaterial, ChangesResp, DeviceInfo, ErrorBody, ErrorCode, Event, JoinCode, KdfWire,
    LoginReq, PushItem, PushReq, PushResp, RecoverReq, RegisterReq, TokenResp, WireRow,
};
use data_encoding::BASE64URL_NOPAD;
use serde::Serialize;

/// Current system time, Unix milliseconds.
pub fn now_ms() -> i64 {
    tether_server::util::system_now_ms()
}

/// A syntactically valid `AccountMaterial` (random-looking but fixed bytes of
/// the documented sizes).
pub fn material(vault_id: &str) -> AccountMaterial {
    AccountMaterial {
        vault_id: vault_id.into(),
        key_version: 1,
        kdf: KdfWire {
            m_kib: 65536,
            t: 3,
            p: 4,
            salt: vec![7; 16],
        },
        account_public: vec![1; 32],
        secret_by_kek: vec![2; 73],
        secret_by_recovery: vec![3; 73],
        sealed_vault_key: vec![4; 81],
    }
}

pub fn device(name: &str) -> DeviceInfo {
    DeviceInfo {
        name: name.into(),
        platform: "macos".into(),
    }
}

/// The 32 raw token bytes inside a join code.
pub fn code_token(code: &str) -> Vec<u8> {
    JoinCode::parse(code)
        .expect("valid join code")
        .token
        .to_vec()
}

/// A registered account and its first device's session.
pub struct Account {
    pub username: String,
    pub auth_key: Vec<u8>,
    pub recovery_key: Vec<u8>,
    pub vault_id: String,
    pub tokens: TokenResp,
}

/// Build a register request (auth key byte `k`, recovery key byte `k + 100`).
pub fn register_req(invite: &str, username: &str, k: u8, node_hint: u16) -> RegisterReq {
    RegisterReq {
        invite_token: code_token(invite),
        username: username.into(),
        auth_key: vec![k; 32],
        device: device("First"),
        material: material(&uuid::Uuid::now_v7().to_string()),
        recovery_auth_key: vec![k.wrapping_add(100); 32],
        node_hint,
        code_proof: None,
    }
}

/// Register `username` with a fresh invite; `k` seeds the keys.
pub async fn register_with(srv: &TestServer, username: &str, k: u8, node_hint: u16) -> Account {
    let invite = srv.make_invite().await;
    let req = register_req(&invite, username, k, node_hint);
    let res = srv.post_json("/v1/register", &req).await;
    assert_eq!(
        res.status,
        200,
        "register: {}",
        String::from_utf8_lossy(&res.body)
    );
    Account {
        username: username.into(),
        auth_key: req.auth_key,
        recovery_key: req.recovery_auth_key,
        vault_id: req.material.vault_id,
        tokens: res.json(),
    }
}

/// Register with defaults (keys seeded with 1, node hint 1).
pub async fn register(srv: &TestServer, username: &str) -> Account {
    register_with(srv, username, 1, 1).await
}

/// Any request with optional bearer, device header and JSON body.
pub async fn call<T: Serialize>(
    srv: &TestServer,
    method: &str,
    path: &str,
    bearer: Option<&str>,
    device: Option<&str>,
    body: Option<&T>,
) -> Response {
    let auth;
    let mut headers: Vec<(&str, &str)> = vec![];
    if let Some(t) = bearer {
        auth = format!("Bearer {t}");
        headers.push(("Authorization", &auth));
    }
    if let Some(d) = device {
        headers.push(("X-Tether-Device", d));
    }
    let body = body.map(|b| {
        headers.push(("Content-Type", "application/json"));
        serde_json::to_vec(b).unwrap()
    });
    srv.request(method, path, &headers, body).await
}

pub async fn get_auth(srv: &TestServer, path: &str, token: &str) -> Response {
    call::<()>(srv, "GET", path, Some(token), None, None).await
}

pub async fn post_auth<T: Serialize>(
    srv: &TestServer,
    path: &str,
    token: &str,
    body: Option<&T>,
) -> Response {
    call(srv, "POST", path, Some(token), None, body).await
}

pub async fn delete_auth(srv: &TestServer, path: &str, token: &str) -> Response {
    call::<()>(srv, "DELETE", path, Some(token), None, None).await
}

/// `POST /v1/login` for an existing device. Sends the secret of the account's
/// first device when `device_id` is that device (use [`login_device_with`] for
/// any other).
pub async fn login_device(srv: &TestServer, a: &Account, device_id: &str, key: &[u8]) -> Response {
    let secret = if device_id == a.tokens.device_id {
        a.tokens.device_secret.as_str()
    } else {
        ""
    };
    login_device_with(srv, &a.username, device_id, Some(secret), key).await
}

/// `POST /v1/login` naming `device_id` and (optionally) presenting `secret`.
pub async fn login_device_with(
    srv: &TestServer,
    username: &str,
    device_id: &str,
    secret: Option<&str>,
    key: &[u8],
) -> Response {
    let req = LoginReq {
        username: username.into(),
        auth_key: key.to_vec(),
        device: device("Again"),
        link_token: None,
    };
    let mut headers = vec![
        ("Content-Type", "application/json"),
        ("X-Tether-Device", device_id),
    ];
    if let Some(s) = secret {
        headers.push(("X-Tether-Device-Secret", s));
    }
    srv.request(
        "POST",
        "/v1/login",
        &headers,
        Some(serde_json::to_vec(&req).unwrap()),
    )
    .await
}

/// `POST /v1/login` redeeming `link_token`.
pub async fn login_link(
    srv: &TestServer,
    username: &str,
    key: &[u8],
    link_token: Vec<u8>,
    name: &str,
) -> Response {
    let req = LoginReq {
        username: username.into(),
        auth_key: key.to_vec(),
        device: device(name),
        link_token: Some(link_token),
    };
    srv.post_json("/v1/login", &req).await
}

/// Ask the signed-in device for an add-device code.
pub async fn make_link(srv: &TestServer, token: &str) -> core_proto::LinkResp {
    let res = post_auth::<()>(srv, "/v1/link", token, None).await;
    assert_eq!(
        res.status,
        200,
        "link: {}",
        String::from_utf8_lossy(&res.body)
    );
    res.json()
}

/// Add a device to `a` through a link code; returns its session.
pub async fn add_device(srv: &TestServer, a: &Account, name: &str) -> TokenResp {
    let link = make_link(srv, &a.tokens.access_token).await;
    let res = login_link(srv, &a.username, &a.auth_key, code_token(&link.code), name).await;
    assert_eq!(
        res.status,
        200,
        "add device: {}",
        String::from_utf8_lossy(&res.body)
    );
    res.json()
}

pub fn error_of(res: &Response) -> ErrorBody {
    res.json()
}

/// Assert `res` is the error `code` with HTTP `status`.
pub fn assert_err(res: &Response, status: u16, code: ErrorCode) {
    assert_eq!(
        (res.status, error_of(res).code),
        (status, code),
        "{}",
        String::from_utf8_lossy(&res.body)
    );
}

/// Decode an access/refresh token string the way the server does.
pub fn token_bytes(token: &str) -> Option<Vec<u8>> {
    BASE64URL_NOPAD.decode(token.as_bytes()).ok()
}

// ------------------------------------------------------------------ sync

/// The text form of an HLC: `{wall_ms:016x}-{counter:08x}-{node:04x}`.
pub fn hlc(wall_ms: i64, counter: u32, node: u16) -> String {
    format!("{wall_ms:016x}-{counter:08x}-{node:04x}")
}

/// A fresh record id.
pub fn new_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

/// A live row of kind `kind` with the HLC `(wall_ms, 0, 1)`.
pub fn row(id: &str, kind: &str, wall_ms: i64, envelope: &[u8]) -> WireRow {
    WireRow {
        id: id.into(),
        kind: kind.into(),
        key_version: 1,
        hlc: hlc(wall_ms, 0, 1),
        rev: None,
        seq: None,
        deleted: false,
        envelope: envelope.to_vec(),
    }
}

/// A tombstone for `id`.
pub fn tombstone(id: &str, kind: &str, wall_ms: i64) -> WireRow {
    WireRow {
        deleted: true,
        envelope: vec![],
        ..row(id, kind, wall_ms, b"")
    }
}

/// A push item.
pub fn item(row: WireRow, base_rev: Option<i64>) -> PushItem {
    PushItem { row, base_rev }
}

/// `POST /v1/sync/push`.
pub async fn push(srv: &TestServer, token: &str, items: Vec<PushItem>) -> Response {
    post_auth(srv, "/v1/sync/push", token, Some(&PushReq { items })).await
}

/// Push and expect 200.
pub async fn push_ok(srv: &TestServer, token: &str, items: Vec<PushItem>) -> PushResp {
    let res = push(srv, token, items).await;
    assert_eq!(
        res.status,
        200,
        "push: {}",
        String::from_utf8_lossy(&res.body)
    );
    res.json()
}

/// `GET /v1/sync/changes?since=N[&limit=M]`.
pub async fn changes(srv: &TestServer, token: &str, since: i64, limit: Option<i64>) -> Response {
    let mut path = format!("/v1/sync/changes?since={since}");
    if let Some(l) = limit {
        path.push_str(&format!("&limit={l}"));
    }
    get_auth(srv, &path, token).await
}

/// One page of changes, expecting 200.
pub async fn pull(srv: &TestServer, token: &str, since: i64, limit: Option<i64>) -> ChangesResp {
    let res = changes(srv, token, since, limit).await;
    assert_eq!(
        res.status,
        200,
        "changes: {}",
        String::from_utf8_lossy(&res.body)
    );
    res.json()
}

/// The limited token `recover-login` issues.
pub async fn recover_token(srv: &TestServer, a: &Account) -> TokenResp {
    let req = RecoverReq {
        username: a.username.clone(),
        recovery_auth_key: a.recovery_key.clone(),
        device: device("Fresh"),
    };
    let res = srv.post_json("/v1/recover-login", &req).await;
    assert_eq!(res.status, 200, "{}", String::from_utf8_lossy(&res.body));
    res.json()
}

// ------------------------------------------------------------- websocket

pub type Ws = tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>;

/// Open `/v1/events` offering exactly the subprotocol list `protocols`.
pub async fn ws_open_with(
    srv: &TestServer,
    protocols: Option<&str>,
) -> Result<
    (
        Ws,
        tokio_tungstenite::tungstenite::handshake::client::Response,
    ),
    tokio_tungstenite::tungstenite::Error,
> {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let mut req = format!("ws://{}/v1/events", srv.addr())
        .into_client_request()
        .unwrap();
    if let Some(p) = protocols {
        req.headers_mut()
            .insert("Sec-WebSocket-Protocol", p.parse().unwrap());
    }
    let tcp = tokio::net::TcpStream::connect(srv.addr()).await.unwrap();
    tokio_tungstenite::client_async(req, tcp).await
}

/// Open `/v1/events` the way a client does: `tether.v1, bearer.<token>`.
pub async fn ws_connect(srv: &TestServer, token: &str) -> Ws {
    let protocols = format!(
        "{}, {}",
        core_proto::WS_PROTOCOL,
        core_proto::ws_bearer_protocol(token)
    );
    ws_open_with(srv, Some(&protocols))
        .await
        .expect("websocket upgrade")
        .0
}

/// What the next thing on a socket was.
#[derive(Debug, PartialEq)]
pub enum WsNext {
    Event(Event),
    Closed,
    Silence,
}

/// The next event within `wait`, skipping pings and pongs.
pub async fn ws_next(ws: &mut Ws, wait: std::time::Duration) -> WsNext {
    use futures_util::StreamExt;
    use tokio_tungstenite::tungstenite::Message;
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        match tokio::time::timeout_at(deadline, ws.next()).await {
            Err(_) => return WsNext::Silence,
            Ok(None) | Ok(Some(Err(_))) | Ok(Some(Ok(Message::Close(_)))) => return WsNext::Closed,
            Ok(Some(Ok(Message::Text(t)))) => {
                return WsNext::Event(serde_json::from_str(t.as_str()).expect("event JSON"));
            }
            Ok(Some(Ok(_))) => {}
        }
    }
}

/// Move the server's clock to `now_ms` and keep every existing access token
/// valid (they would otherwise expire when the clock jumps ahead).
pub async fn jump_clock(srv: &TestServer, now_ms: i64) {
    srv.set_clock(now_ms);
    sqlx::query("UPDATE access_tokens SET expires_at = ?")
        .bind(i64::MAX)
        .execute(&srv.state.db)
        .await
        .unwrap();
}

// ------------------------------------------------------- leak detection

/// Does `hay` contain `needle`?
pub fn contains(hay: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && hay.windows(needle.len()).any(|w| w == needle)
}

/// Byte strings that must never appear in the server's files or logs, in each
/// way they could plausibly get there: the plaintext canary, every sealed
/// envelope (raw prefix, hex prefix, base64 of the whole and of an aligned
/// prefix, as a JSON body would print it), and session secrets as text and
/// as raw bytes. Reusable for any artefact the server writes: the database,
/// its WAL, captured logs, backup files.
#[derive(Default)]
pub struct Needles(Vec<(String, Vec<u8>)>);

impl Needles {
    /// Plaintext marker strings.
    pub fn canaries(mut self, canaries: &[&str]) -> Self {
        for c in canaries {
            self.0
                .push((format!("plaintext {c}"), c.as_bytes().to_vec()));
        }
        self
    }

    /// Sealed envelopes as sent: they must not be printed anywhere (logs).
    /// The first 32 bytes are raw ciphertext; the longer forms are what a
    /// careless `{:?}` or JSON dump would write.
    pub fn envelopes(mut self, envelopes: &[Vec<u8>]) -> Self {
        for (i, e) in envelopes.iter().enumerate() {
            let b64 = data_encoding::BASE64.encode(e);
            // 30 bytes = 40 base64 characters, no padding: a true prefix of `b64`.
            let b64_prefix = data_encoding::BASE64.encode(&e[..30.min(e.len())]);
            self.0.push((
                format!("envelope {i} (raw prefix)"),
                e[..32.min(e.len())].to_vec(),
            ));
            self.0.push((
                format!("envelope {i} (hex prefix)"),
                data_encoding::HEXLOWER
                    .encode(&e[..32.min(e.len())])
                    .into_bytes(),
            ));
            self.0
                .push((format!("envelope {i} (base64)"), b64.into_bytes()));
            self.0.push((
                format!("envelope {i} (base64 prefix)"),
                b64_prefix.into_bytes(),
            ));
        }
        self
    }

    /// A secret string (access or refresh token, device secret) and, when it
    /// is base64url, the bytes it encodes.
    pub fn secret(mut self, what: &str, secret: &str) -> Self {
        self.0
            .push((format!("{what} (text)"), secret.as_bytes().to_vec()));
        if let Some(raw) = token_bytes(secret) {
            self.0.push((format!("{what} (raw bytes)"), raw));
        }
        self
    }

    /// Panic naming the first needle found in `hay`.
    pub fn assert_absent(&self, where_: &str, hay: &[u8]) {
        for (what, needle) in &self.0 {
            assert!(!contains(hay, needle), "{what} reached {where_}");
        }
    }

    /// Which needles `hay` contains (for controls that prove the search works).
    pub fn found_in(&self, hay: &[u8]) -> Vec<&str> {
        self.0
            .iter()
            .filter(|(_, n)| contains(hay, n))
            .map(|(w, _)| w.as_str())
            .collect()
    }
}

/// The server's database file and its WAL, concatenated (either may be absent).
pub fn db_bytes(dir: &std::path::Path) -> Vec<u8> {
    let db = std::fs::read(dir.join("tether.db")).unwrap_or_default();
    let wal = std::fs::read(dir.join("tether.db-wal")).unwrap_or_default();
    [db, wal].concat()
}
