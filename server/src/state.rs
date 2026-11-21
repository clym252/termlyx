//! Shared application state.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use core_proto::Event;
use sqlx::SqlitePool;
use tokio::sync::broadcast;

use crate::config::Config;
use crate::limits::RateLimiter;
use crate::util::system_now_ms;

/// Source of "now" in Unix milliseconds. Tests substitute their own.
pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

/// The system clock.
pub fn system_clock() -> Clock {
    Arc::new(system_now_ms)
}

/// Request size, rate-limit and lockout settings.
#[derive(Debug, Clone)]
pub struct Limits {
    /// Largest accepted request body, bytes.
    pub max_body_bytes: usize,
    /// Prelogin/login/register/recover-login attempts per client address per
    /// minute.
    pub ip_per_minute: u32,
    /// The same, per (lowercase) username.
    pub username_per_minute: u32,
    /// Consecutive failed logins that lock a username.
    pub lockout_failures: u32,
    /// How long a locked username stays locked, milliseconds.
    pub lockout_ms: i64,
    /// How often the events WebSocket pings its peer and re-checks that the
    /// device is still valid. A peer silent for two intervals is dropped.
    pub ws_ping: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_body_bytes: 4 * 1024 * 1024,
            ip_per_minute: 20,
            username_per_minute: 10,
            lockout_failures: 10,
            lockout_ms: 15 * 60 * 1000,
            ws_ping: Duration::from_secs(30),
        }
    }
}

/// Who a published [`Event`] is for, among the devices of one vault.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// Every connected device.
    All,
    /// Every connected device except this one (the pusher).
    Except(String),
    /// Only this device.
    Only(String),
}

impl Target {
    /// Does an event with this target go to `device_id`?
    pub fn includes(&self, device_id: &str) -> bool {
        match self {
            Target::All => true,
            Target::Except(d) => d != device_id,
            Target::Only(d) => d == device_id,
        }
    }
}

#[derive(Clone)]
struct Msg {
    target: Target,
    event: Event,
}

/// One device's view of its vault's event stream: only the events addressed
/// to it.
pub struct Subscription {
    rx: broadcast::Receiver<Msg>,
    device_id: String,
}

impl Subscription {
    /// The next event for this device. `None` when the stream can no longer be
    /// trusted to be complete: the hub went away, or this subscriber fell more
    /// than the channel capacity behind and missed events. The caller closes
    /// the socket then; the client reconnects and syncs.
    pub async fn recv(&mut self) -> Option<Event> {
        loop {
            match self.rx.recv().await {
                Ok(m) if m.target.includes(&self.device_id) => return Some(m.event),
                Ok(_) => {}
                Err(_) => return None,
            }
        }
    }
}

/// Per-vault fan-out of notifications to connected WebSockets. Each message
/// carries a [`Target`], so one channel per vault serves all its devices.
#[derive(Clone, Default)]
pub struct Events {
    channels: Arc<Mutex<HashMap<String, broadcast::Sender<Msg>>>>,
}

impl Events {
    /// An empty hub.
    pub fn new() -> Self {
        Self::default()
    }

    /// Listen on `vault_id` as `device_id`.
    pub fn subscribe(&self, vault_id: &str, device_id: &str) -> Subscription {
        let mut map = self.channels.lock().unwrap_or_else(|e| e.into_inner());
        let rx = map
            .entry(vault_id.to_owned())
            .or_insert_with(|| broadcast::channel(64).0)
            .subscribe();
        Subscription {
            rx,
            device_id: device_id.to_owned(),
        }
    }

    /// Send `event` to the devices of `vault_id` that `target` selects.
    /// Nobody listening is not an error.
    pub fn publish(&self, vault_id: &str, target: Target, event: Event) {
        let mut map = self.channels.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(tx) = map.get(vault_id)
            && tx.send(Msg { target, event }).is_err()
        {
            // No receivers left: drop the channel.
            map.remove(vault_id);
        }
    }
}

/// Everything a handler needs. Cheap to clone.
#[derive(Clone)]
pub struct AppState {
    /// The SQLite pool.
    pub db: SqlitePool,
    /// The effective configuration.
    pub cfg: Arc<Config>,
    /// WebSocket notification hub.
    pub events: Events,
    /// Request limits.
    pub limits: Limits,
    /// In-memory sliding windows for the unauthenticated endpoints.
    pub limiter: Arc<RateLimiter>,
    /// SPKI pin that goes into invite and add-device codes: set in
    /// self-signed mode only (`files` clients use normal trust, `off` has no
    /// certificate).
    pub code_pin: Option<[u8; 32]>,
    /// SPKI pin of the served certificate (`self-signed` and `files`), the
    /// binding of pinned short-code proofs; `None` with TLS off.
    pub own_spki: Option<[u8; 32]>,
    /// Database identity from `meta`; see [`crate::db::Meta`].
    pub epoch: String,
    /// Keys the fake-prelogin HMAC (from `meta.server_secret`).
    pub server_secret: Arc<[u8; 32]>,
    /// Time source, Unix milliseconds.
    pub clock: Clock,
}

impl AppState {
    /// Open the database in `cfg.data_dir` (running migrations, creating
    /// `meta` values) and assemble the state.
    pub async fn open(cfg: Config, clock: Option<Clock>) -> anyhow::Result<Self> {
        let db = crate::db::open(&cfg.data_dir).await?;
        let meta = crate::db::ensure_meta(&db).await?;
        Ok(Self {
            db,
            cfg: Arc::new(cfg),
            events: Events::new(),
            limits: Limits::default(),
            limiter: Arc::new(RateLimiter::default()),
            code_pin: None,
            own_spki: None,
            epoch: meta.epoch,
            server_secret: Arc::new(meta.server_secret),
            clock: clock.unwrap_or_else(system_clock),
        })
    }

    /// Current time, Unix milliseconds.
    pub fn now_ms(&self) -> i64 {
        (self.clock)()
    }

    /// The data directory.
    pub fn data_dir(&self) -> &Path {
        &self.cfg.data_dir
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn changed(seq: i64) -> Event {
        Event::Changed { seq }
    }

    #[tokio::test]
    async fn targets_select_devices() {
        let hub = Events::new();
        let mut a = hub.subscribe("v", "a");
        let mut b = hub.subscribe("v", "b");
        let mut other = hub.subscribe("w", "a");

        hub.publish("v", Target::All, changed(1));
        hub.publish("v", Target::Except("a".into()), changed(2));
        hub.publish("v", Target::Only("a".into()), changed(3));
        hub.publish("v", Target::All, changed(4));

        let mut got_a = vec![];
        let mut got_b = vec![];
        for _ in 0..3 {
            got_a.push(a.recv().await.unwrap());
        }
        for _ in 0..3 {
            got_b.push(b.recv().await.unwrap());
        }
        assert_eq!(got_a, vec![changed(1), changed(3), changed(4)]);
        assert_eq!(got_b, vec![changed(1), changed(2), changed(4)]);
        // Another vault's subscriber (same device id) heard nothing.
        hub.publish("w", Target::All, changed(9));
        assert_eq!(other.recv().await, Some(changed(9)));
    }

    #[tokio::test]
    async fn a_subscriber_that_falls_behind_is_cut_off() {
        let hub = Events::new();
        let mut slow = hub.subscribe("v", "a");
        for i in 0..200 {
            hub.publish("v", Target::All, changed(i));
        }
        assert_eq!(slow.recv().await, None);
    }

    #[test]
    fn publishing_to_nobody_is_fine() {
        let hub = Events::new();
        hub.publish("v", Target::All, changed(1));
        drop(hub.subscribe("v", "a"));
        hub.publish("v", Target::All, changed(2));
    }
}
