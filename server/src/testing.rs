//! In-process servers and a tiny HTTP client for tests (feature `test-util`).
//!
//! ```ignore
//! let srv = tether_server::testing::spawn().await;       // plain HTTP, loopback
//! let tls = tether_server::testing::spawn_tls().await;   // self-signed + pin
//! assert_eq!(srv.get("/v1/health").await.status, 200);
//! ```
//!
//! Not for production code: it is compiled only with `test-util`, which no
//! normal dependency graph enables.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use axum::body::Bytes;
use axum::http::HeaderMap;
use http_body_util::{BodyExt, Full};
use hyper_util::rt::TokioIo;
use rustls::pki_types::ServerName;
use sha2::{Digest, Sha256};

use crate::config::{Config, TlsMode};
use crate::state::{AppState, Clock};
use crate::util::{random_bytes, system_now_ms};
use crate::{Running, start_with_state};
use core_proto::{CodeKind, JoinCode};

pub use tempfile::TempDir;

/// Invites made by [`TestServer::make_invite`] stay valid this long.
const INVITE_TTL_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// A response from the test client.
#[derive(Debug)]
pub struct Response {
    /// HTTP status code.
    pub status: u16,
    /// Response headers.
    pub headers: HeaderMap,
    /// Raw body.
    pub body: Vec<u8>,
}

impl Response {
    /// Parse the body as JSON; panics (with the body) if it is not.
    pub fn json<T: serde::de::DeserializeOwned>(&self) -> T {
        serde_json::from_slice(&self.body).unwrap_or_else(|e| {
            panic!(
                "response is not the expected JSON ({e}): {}",
                String::from_utf8_lossy(&self.body)
            )
        })
    }
}

/// A server running in this process on a temporary data directory.
pub struct TestServer {
    /// Base URL, e.g. `http://127.0.0.1:41234` or `https://127.0.0.1:41234`.
    pub url: String,
    /// SPKI pin of the certificate; all zeros for the plain-HTTP server.
    pub pin: [u8; 32],
    /// The server's shared state (database, config, events, clock).
    pub state: AppState,
    /// The data directory; deleted when the server is dropped.
    pub dir: TempDir,
    addr: SocketAddr,
    clock: Arc<AtomicI64>,
    running: Running,
}

fn base_config(dir: &TempDir) -> Config {
    let mut cfg = Config::new(dir.path().to_path_buf());
    cfg.bind = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
    cfg.tls = TlsMode::Off;
    cfg
}

/// Plain HTTP on `127.0.0.1:0` ([`TlsMode::Off`]).
pub async fn spawn() -> TestServer {
    spawn_with(|_| {}).await
}

/// Self-signed TLS on `127.0.0.1:0`, certificate names `["localhost"]` and
/// IP `127.0.0.1`.
pub async fn spawn_tls() -> TestServer {
    spawn_with(|c| {
        c.tls = TlsMode::SelfSigned;
        c.names = vec!["localhost".into()];
        c.ips = vec![IpAddr::V4(Ipv4Addr::LOCALHOST)];
    })
    .await
}

/// Start from the [`spawn`] configuration (plain HTTP, loopback, temporary
/// data directory), let `tweak` change it, then start.
pub async fn spawn_with(tweak: impl FnOnce(&mut Config)) -> TestServer {
    let dir = TempDir::new().expect("temp dir");
    let mut cfg = base_config(&dir);
    tweak(&mut cfg);
    start_in(dir, cfg, Arc::new(AtomicI64::new(0)), |_| {}).await
}

/// Plain HTTP like [`spawn`], with `tweak` applied to the assembled
/// [`AppState`] (for example a short `limits.ws_ping`) before serving. The
/// tweak is not re-applied by [`TestServer::restart`].
pub async fn spawn_with_state(tweak: impl FnOnce(&mut AppState)) -> TestServer {
    let dir = TempDir::new().expect("temp dir");
    let cfg = base_config(&dir);
    start_in(dir, cfg, Arc::new(AtomicI64::new(0)), tweak).await
}

async fn start_in(
    dir: TempDir,
    mut cfg: Config,
    clock: Arc<AtomicI64>,
    tweak: impl FnOnce(&mut AppState),
) -> TestServer {
    cfg.bind.set_port(0);
    let c = clock.clone();
    let clock_fn: Clock = Arc::new(move || match c.load(Ordering::SeqCst) {
        0 => system_now_ms(),
        v => v,
    });
    let running = start_with_state(cfg, Some(clock_fn), tweak)
        .await
        .expect("start test server");
    let tls = running.pin.is_some();
    let addr = running.addr;
    TestServer {
        url: format!("{}://{addr}", if tls { "https" } else { "http" }),
        pin: running.pin.unwrap_or([0; 32]),
        state: running.state.clone(),
        dir,
        addr,
        clock,
        running,
    }
}

impl TestServer {
    /// The bound socket address.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Override the server's clock (Unix milliseconds). `0` restores the
    /// system clock. Survives [`TestServer::restart`].
    pub fn set_clock(&self, now_ms: i64) {
        self.clock.store(now_ms, Ordering::SeqCst);
    }

    /// Store a fresh single-use invite (valid seven days from the server's
    /// clock) and return its `tether1-...` code. The invite table keeps only the
    /// SHA-256 of the token.
    pub async fn make_invite(&self) -> String {
        let token = random_bytes::<32>();
        let now = self.state.now_ms();
        sqlx::query(
            "INSERT INTO invites (token_hash, note, created_at, expires_at) VALUES (?, NULL, ?, ?)",
        )
        .bind(Sha256::digest(token).to_vec())
        .bind(now)
        .bind(now + INVITE_TTL_MS)
        .execute(&self.state.db)
        .await
        .expect("insert invite");
        JoinCode {
            kind: CodeKind::Invite,
            url: self.url.clone(),
            pin: (self.pin != [0; 32]).then_some(self.pin),
            token,
        }
        .encode()
    }

    /// End every device session (access and refresh tokens), as a restore
    /// from a backup older than the devices' refresh tokens does. Devices
    /// stay registered and can sign in again with their password.
    pub async fn end_all_sessions(&self) {
        for sql in ["DELETE FROM access_tokens", "DELETE FROM refresh_tokens"] {
            sqlx::query(sql)
                .execute(&self.state.db)
                .await
                .expect("end sessions");
        }
    }

    /// Stop this server (finishing in-flight requests, closing the database)
    /// and start a new one on the same data directory, on a new port.
    pub async fn restart(self) -> TestServer {
        let Self {
            dir,
            clock,
            running,
            state,
            ..
        } = self;
        let cfg = (*state.cfg).clone();
        running.shutdown().await;
        start_in(dir, cfg, clock, |_| {}).await
    }

    /// Write a backup of the database to `path` while serving (see
    /// [`crate::backup::backup`]); refuses to overwrite an existing file.
    pub async fn backup_to(&self, path: impl AsRef<Path>) -> anyhow::Result<()> {
        crate::backup::backup(&self.state.db, self.dir.path(), path.as_ref(), false).await?;
        Ok(())
    }

    /// Stop this server, restore the backup at `path` into its data directory
    /// (a new `epoch` is written, as the real `restore` does) and start a new
    /// server on the same directory and clock, on a new port. On an error the
    /// server stays stopped.
    pub async fn restore_from(self, path: impl AsRef<Path>) -> anyhow::Result<TestServer> {
        let Self {
            dir,
            clock,
            running,
            state,
            ..
        } = self;
        let cfg = (*state.cfg).clone();
        running.shutdown().await;
        crate::backup::restore(dir.path(), path.as_ref(), false).await?;
        Ok(start_in(dir, cfg, clock, |_| {}).await)
    }

    /// `GET path`.
    pub async fn get(&self, path: &str) -> Response {
        self.request("GET", path, &[], None).await
    }

    /// `POST path` with a JSON body.
    pub async fn post_json<T: serde::Serialize>(&self, path: &str, body: &T) -> Response {
        let body = serde_json::to_vec(body).expect("serialise body");
        self.request(
            "POST",
            path,
            &[("Content-Type", "application/json")],
            Some(body),
        )
        .await
    }

    /// Send one request, verifying the certificate against [`Self::pin`] when
    /// the server uses TLS. Panics on transport errors.
    pub async fn request(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: Option<Vec<u8>>,
    ) -> Response {
        let pin = (self.pin != [0; 32]).then_some(self.pin);
        send(self.addr, pin, method, path, headers, body)
            .await
            .expect("request failed")
    }

    /// Like [`Self::request`] for a bodyless request but trusting `pin`
    /// instead of the server's real one; returns the transport error.
    pub async fn try_request_with_pin(
        &self,
        pin: [u8; 32],
        method: &str,
        path: &str,
    ) -> std::io::Result<Response> {
        send(self.addr, Some(pin), method, path, &[], None).await
    }
}

pub use crate::tls::pinned_client_config;

fn io_err(e: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::other(e.to_string())
}

async fn send(
    addr: SocketAddr,
    pin: Option<[u8; 32]>,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<Vec<u8>>,
) -> std::io::Result<Response> {
    let tcp = tokio::net::TcpStream::connect(addr).await?;
    match pin {
        None => exchange(TokioIo::new(tcp), addr, method, path, headers, body).await,
        Some(pin) => {
            let connector = tokio_rustls::TlsConnector::from(pinned_client_config(pin));
            let name = ServerName::try_from("localhost").map_err(io_err)?;
            let tls = connector.connect(name, tcp).await?;
            exchange(TokioIo::new(tls), addr, method, path, headers, body).await
        }
    }
}

async fn exchange<T>(
    io: TokioIo<T>,
    addr: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<Vec<u8>>,
) -> std::io::Result<Response>
where
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (mut tx, conn) = hyper::client::conn::http1::handshake(io)
        .await
        .map_err(io_err)?;
    tokio::spawn(conn);
    let mut req = hyper::Request::builder()
        .method(method)
        .uri(path)
        .header("host", addr.to_string());
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let req = req
        .body(Full::new(Bytes::from(body.unwrap_or_default())))
        .map_err(io_err)?;
    let res = tx.send_request(req).await.map_err(io_err)?;
    let (parts, body) = res.into_parts();
    let body = body.collect().await.map_err(io_err)?.to_bytes().to_vec();
    Ok(Response {
        status: parts.status.as_u16(),
        headers: parts.headers,
        body,
    })
}
