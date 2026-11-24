//! The Tether sync server.
//!
//! It stores ciphertext, wrapped key material and the cleartext sync metadata
//! documented in `docs/crypto.md`, and nothing else: no plaintext record
//! field, password or key ever reaches its database or its logs.
//!
//! * [`config`]: environment / `server.toml` settings.
//! * [`tls`]: self-signed (persisted, SPKI-pinned), operator files, or off.
//! * [`db`]: SQLite pool and migrations.
//! * [`health`]: the `health` subcommand (container healthcheck).
//! * [`http`]: router, middleware, `/v1/health`.
//! * [`account`]: key material, password change, recovery, schema gate.
//! * [`accounts`]: usernames, verifiers, invites, prelogin, register.
//! * [`auth`]: login, recover-login, refresh, logout, tokens and extractors.
//! * [`devices`]: node ids, device list/rename/revoke, add-device link codes.
//! * [`admin`]: the `users` / `devices` admin subcommands; [`backup`]: backup
//!   and restore; [`lock`]: the data directory lock a running server holds.
//! * [`limits`]: rate limiting and lockout bookkeeping.
//! * [`sync`]: record changes and push; [`compact`]: tombstone compaction;
//!   [`events`]: the change-notification WebSocket.
//! * [`start`] / [`serve`]: run the server.
//! * [`testing`] (feature `test-util`): in-process servers for tests.

pub mod account;
pub mod accounts;
pub mod admin;
pub mod auth;
pub mod backup;
pub mod cli;
pub mod compact;
pub mod config;
pub mod db;
pub mod devices;
pub mod error;
pub mod events;
pub mod health;
pub mod http;
pub mod limits;
pub mod lock;
pub mod setup;
pub mod state;
pub mod sync;
pub mod tls;
pub mod util;

#[cfg(feature = "test-util")]
pub mod testing;

use std::net::SocketAddr;
use std::time::Duration;

use anyhow::Context;
use axum_server::Handle;
use axum_server::tls_rustls::RustlsConfig;
use tokio::task::JoinHandle;

pub use config::{Config, TlsMode};
pub use http::router;
pub use state::{AppState, Clock, Events, Limits, Target};

/// How long in-flight requests get to finish on shutdown.
const GRACE: Duration = Duration::from_secs(10);

/// A started server.
pub struct Running {
    /// The address actually bound (useful with port 0).
    pub addr: SocketAddr,
    /// The shared state the server is using.
    pub state: AppState,
    /// SPKI pin of the served certificate; `None` with TLS off.
    pub pin: Option<[u8; 32]>,
    handle: Handle<SocketAddr>,
    /// Held while serving; `restore` refuses to run against a locked directory.
    _lock: lock::DataLock,
    task: JoinHandle<std::io::Result<()>>,
    compactor: JoinHandle<()>,
}

impl Running {
    /// Stop accepting connections, let in-flight requests finish (up to ten
    /// seconds) and close the database.
    pub async fn shutdown(self) {
        self.compactor.abort();
        self.handle.graceful_shutdown(Some(GRACE));
        match self.task.await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!(error = %e, "server stopped with an error"),
            Err(e) => tracing::warn!(error = %e, "server task failed"),
        }
        self.state.db.close().await;
    }

    /// Wait until the server task exits. `Ok` means it returned cleanly, which
    /// without a shutdown request is itself unexpected; see [`supervise`].
    /// Call at most once (a finished task cannot be awaited again).
    pub async fn wait(&mut self) -> anyhow::Result<()> {
        (&mut self.task).await??;
        Ok(())
    }
}

/// Start serving with the system clock.
pub async fn start(cfg: Config) -> anyhow::Result<Running> {
    start_with_clock(cfg, None).await
}

/// Start serving; `clock` replaces the system clock (tests).
pub async fn start_with_clock(cfg: Config, clock: Option<Clock>) -> anyhow::Result<Running> {
    start_inner(cfg, clock, |_| {}).await
}

/// Like [`start_with_clock`], letting `tweak` adjust the assembled state
/// (limits, intervals) before the server starts. For tests.
#[cfg(feature = "test-util")]
pub async fn start_with_state(
    cfg: Config,
    clock: Option<Clock>,
    tweak: impl FnOnce(&mut AppState),
) -> anyhow::Result<Running> {
    start_inner(cfg, clock, tweak).await
}

async fn start_inner(
    cfg: Config,
    clock: Option<Clock>,
    tweak: impl FnOnce(&mut AppState),
) -> anyhow::Result<Running> {
    cfg.check()?;
    let data_lock = lock::DataLock::acquire(&cfg.data_dir)?;
    let tls = match cfg.tls {
        TlsMode::Off => None,
        _ => Some(tls::load_or_create(&cfg).context("TLS setup")?),
    };
    let pin = tls.as_ref().map(tls::TlsMaterial::spki_pin);
    let rustls_cfg = match &tls {
        Some(m) => Some(RustlsConfig::from_config(m.server_config()?)),
        None => None,
    };

    let listener = std::net::TcpListener::bind(cfg.bind)
        .with_context(|| format!("cannot listen on {}", cfg.bind))?;
    listener.set_nonblocking(true)?;
    let addr = listener.local_addr()?;

    let mut state = AppState::open(cfg, clock).await?;
    state.code_pin = match state.cfg.tls {
        TlsMode::SelfSigned => pin,
        _ => None,
    };
    state.own_spki = pin;
    tweak(&mut state);
    let compactor = compact::spawn(state.db.clone(), state.clock.clone()).await;
    // Connect info gives the handlers the peer address for per-IP limits.
    let app = router(state.clone()).into_make_service_with_connect_info::<SocketAddr>();
    let handle = Handle::<SocketAddr>::new();

    let task = match rustls_cfg {
        Some(c) => {
            let server = axum_server::from_tcp_rustls(listener, c)?.handle(handle.clone());
            tokio::spawn(async move { server.serve(app).await })
        }
        None => {
            let server = axum_server::from_tcp(listener)?.handle(handle.clone());
            tokio::spawn(async move { server.serve(app).await })
        }
    };

    tracing::info!(
        %addr,
        tls = match state.cfg.tls { TlsMode::SelfSigned => "self-signed", TlsMode::Files { .. } => "files", TlsMode::Off => "off" },
        "termlyx-server listening"
    );
    Ok(Running {
        addr,
        state,
        pin,
        handle,
        _lock: data_lock,
        task,
        compactor,
    })
}

/// Run until SIGINT or SIGTERM, then shut down gracefully. Returns an error if
/// the server task stops on its own.
pub async fn serve(cfg: Config) -> anyhow::Result<()> {
    let running = start(cfg).await?;
    // println!, never tracing: the setup code is a secret (see `setup::banner`).
    if let Some(code) = setup::ensure(&running.state.db, util::system_now_ms()).await? {
        println!("{}", setup::banner(&code));
    }
    if let Some(pin) = running.pin {
        tracing::info!(pin = %core_proto::fingerprint_text(&pin), "certificate pin");
    }
    supervise(running, shutdown_signal()).await
}

/// Keep `running` alive until `shutdown` completes (then stop it gracefully and
/// return `Ok`), or fail as soon as the server task exits by itself: a dead
/// listener must not leave a live process that serves nothing.
pub async fn supervise(
    running: Running,
    shutdown: impl std::future::Future<Output = ()>,
) -> anyhow::Result<()> {
    let mut running = running;
    tokio::select! {
        () = shutdown => {
            tracing::info!("shutting down");
            running.shutdown().await;
            Ok(())
        }
        exit = running.wait() => {
            running.compactor.abort();
            running.state.db.close().await;
            match exit {
                Ok(()) => Err(anyhow::anyhow!("server stopped unexpectedly")),
                Err(e) => Err(e.context("server stopped")),
            }
        }
    }
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = signal(SignalKind::terminate()).expect("install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(dir: &std::path::Path) -> Config {
        let mut c = Config::new(dir.to_path_buf());
        c.tls = TlsMode::Off;
        c.bind = "127.0.0.1:0".parse().unwrap();
        c
    }

    #[tokio::test]
    async fn supervise_fails_when_the_server_task_exits_on_its_own() {
        let dir = tempfile::tempdir().unwrap();
        let running = start(cfg(dir.path())).await.unwrap();
        // Kill the listener from underneath; the shutdown future never fires.
        running.handle.shutdown();
        let res = tokio::time::timeout(
            Duration::from_secs(5),
            supervise(running, std::future::pending()),
        )
        .await
        .expect("supervise must notice the dead server, not hang");
        let err = res.expect_err("a dead server task is an error");
        assert!(err.to_string().contains("stopped"), "{err}");
    }

    #[tokio::test]
    async fn supervise_returns_ok_on_requested_shutdown() {
        let dir = tempfile::tempdir().unwrap();
        let running = start(cfg(dir.path())).await.unwrap();
        supervise(running, std::future::ready(())).await.unwrap();
    }
}
