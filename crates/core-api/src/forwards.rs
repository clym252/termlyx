//! Port-forward rules at run time: [`ForwardManager`] starts and stops
//! [`ForwardRule`]s on an [`SshConnection`] and tracks each rule's
//! [`ForwardStatus`].
//!
//! * A rule goes `Starting` → `Active { bound }` once it listens (port 0
//!   reports the port actually bound), or `Starting` → `Error(..)` if it
//!   cannot. A local bind address that is taken gives exactly
//!   `Error("address in use")`.
//! * Each rule runs in its own task with its own listener and connections,
//!   so one rule failing never affects another.
//! * When the SSH connection closes, an active rule releases its listener
//!   and moves to `Error("connection closed")` (not `Stopped`, which only
//!   [`ForwardManager::stop`] sets: the rule did not end because the user
//!   asked). Starting it again on a new connection is the caller's call.
//! * [`ForwardManager::stop`] waits until the listener is closed (a remote
//!   forward is cancelled on the server) and every connection in flight has
//!   ended, then reports `Stopped`. Concurrent stops all wait for that, and
//!   a `start` during a stop is queued until it has finished.
//! * Every status change is also sent on the events stream returned by
//!   [`ForwardManager::new`], in order per rule.
//!
//! Dropping the manager aborts every rule's task (listeners and connections
//! close; remote forwards are cancelled in the background).

use std::collections::HashMap;
use std::future::Future;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};

use core_model::{ForwardKind, ForwardRule, Id};
use core_ssh::{Forward, ForwardSpec, SshConnection};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;

/// Where a rule stands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "detail", rename_all = "camelCase")]
pub enum ForwardStatus {
    /// Binding (or asking the server to bind).
    Starting,
    /// Listening on `bound` (for a remote forward: the server's port).
    Active { bound: SocketAddr },
    /// Could not start, or stopped by itself (for example
    /// `"address in use"`, `"connection closed"`).
    Error(String),
    /// Stopped by [`ForwardManager::stop`].
    Stopped,
}

impl ForwardStatus {
    /// Starting or active.
    pub fn is_running(&self) -> bool {
        matches!(self, ForwardStatus::Starting | ForwardStatus::Active { .. })
    }
}

/// A rule's status changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForwardEvent {
    pub rule_id: Id,
    pub status: ForwardStatus,
}

/// Follows one rule's status (across restarts of the same rule id).
#[derive(Debug, Clone)]
pub struct RuleHandle {
    rule_id: Id,
    status: watch::Receiver<ForwardStatus>,
}

impl RuleHandle {
    pub fn rule_id(&self) -> Id {
        self.rule_id
    }

    /// The current status.
    pub fn status(&self) -> ForwardStatus {
        self.status.borrow().clone()
    }

    /// Waits until the rule is no longer `Starting` and returns that status.
    pub async fn wait_settled(&mut self) -> ForwardStatus {
        match self
            .status
            .wait_for(|s| *s != ForwardStatus::Starting)
            .await
        {
            Ok(s) => s.clone(),
            // The manager is gone, and the rule with it.
            Err(_) => ForwardStatus::Stopped,
        }
    }

    /// Waits for the next status change; `None` once the manager is gone.
    pub async fn changed(&mut self) -> Option<ForwardStatus> {
        self.status.changed().await.ok()?;
        Some(self.status.borrow_and_update().clone())
    }
}

/// Runs port-forward rules; see the module docs.
pub struct ForwardManager {
    shared: Arc<Shared>,
}

struct Shared {
    rules: Mutex<HashMap<Id, Entry>>,
    events: mpsc::UnboundedSender<ForwardEvent>,
    next_run: AtomicU64,
}

struct Entry {
    /// Which run of the rule may set its status; a stale task's updates
    /// are ignored.
    run_id: u64,
    status: watch::Sender<ForwardStatus>,
    run: Option<Run>,
    /// Set while a stop is in progress; turns true when it has finished.
    stopping: Option<watch::Receiver<bool>>,
    /// A `start` that arrived during the stop; it runs once the stop has
    /// finished.
    restart: Option<Restart>,
}

struct Run {
    stop: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

struct Restart {
    spec: Result<ForwardSpec, String>,
    conn: SshConnection,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, HashMap<Id, Entry>> {
        self.rules.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Sets the status of run `run_id` of `rule_id` and reports a change.
    fn set_status(&self, rule_id: Id, run_id: u64, status: ForwardStatus) {
        let rules = self.lock();
        let Some(entry) = rules.get(&rule_id) else {
            return;
        };
        if entry.run_id == run_id {
            Self::publish(&self.events, rule_id, entry, status);
        }
    }

    fn publish(
        events: &mpsc::UnboundedSender<ForwardEvent>,
        rule_id: Id,
        entry: &Entry,
        status: ForwardStatus,
    ) {
        if *entry.status.borrow() == status {
            return;
        }
        entry.status.send_replace(status.clone());
        // Nobody listening is fine.
        let _ = events.send(ForwardEvent { rule_id, status });
    }
}

impl Shared {
    /// Starts a new run of `rule_id`: `Starting`, then its task.
    fn launch(
        shared: &Arc<Shared>,
        rule_id: Id,
        entry: &mut Entry,
        spec: Result<ForwardSpec, String>,
        conn: SshConnection,
    ) {
        let run_id = shared.next_run.fetch_add(1, Ordering::Relaxed);
        entry.run_id = run_id;
        Self::publish(&shared.events, rule_id, entry, ForwardStatus::Starting);
        let (stop, stopped) = oneshot::channel();
        let task = tokio::spawn(run_rule(
            Arc::downgrade(shared),
            (rule_id, run_id),
            spec,
            conn,
            stopped,
        ));
        entry.run = Some(Run { stop, task });
    }
}

/// Finishes a stop: ends the run (listener closed, remote forward
/// cancelled, connections ended), then reports `Stopped`, starts a restart
/// that was asked for meanwhile, and signals `done`. Runs as its own task,
/// so a stop caller that goes away cannot leave it half done.
async fn finish_stop(
    shared: Arc<Shared>,
    rule_id: Id,
    run_id: u64,
    run: Run,
    done: watch::Sender<bool>,
) {
    let _ = run.stop.send(());
    // The task never panics; if it did, it is over either way.
    let _ = run.task.await;
    {
        let mut rules = shared.lock();
        if let Some(entry) = rules.get_mut(&rule_id) {
            entry.stopping = None;
            if entry.run_id == run_id {
                Shared::publish(&shared.events, rule_id, entry, ForwardStatus::Stopped);
            }
            if let Some(Restart { spec, conn }) = entry.restart.take() {
                Shared::launch(&shared, rule_id, entry, spec, conn);
            }
        }
    }
    done.send_replace(true);
}

impl Drop for Shared {
    fn drop(&mut self) {
        let rules = self.rules.get_mut().unwrap_or_else(|p| p.into_inner());
        for entry in rules.values_mut() {
            if let Some(run) = entry.run.take() {
                run.task.abort();
            }
        }
    }
}

impl ForwardManager {
    /// A manager and its stream of status changes.
    pub fn new() -> (Self, mpsc::UnboundedReceiver<ForwardEvent>) {
        let (events, rx) = mpsc::unbounded_channel();
        let shared = Arc::new(Shared {
            rules: Mutex::default(),
            events,
            next_run: AtomicU64::new(1),
        });
        (Self { shared }, rx)
    }

    /// Starts `rule` on `conn` (must be called inside a tokio runtime). The
    /// rule is `Starting` when this returns; follow it with the handle or
    /// the events.
    ///
    /// * A rule that is already starting or active is left as it is (stop
    ///   it first to restart it with other settings).
    /// * While the rule is being stopped, the start is queued: it runs
    ///   (with these settings and `conn`) once the stop has finished and
    ///   the rule went `Stopped`, so it never races the old listener. A
    ///   later `start` replaces the queued one; a later `stop` drops it.
    pub fn start(&self, rule: &ForwardRule, conn: SshConnection) -> RuleHandle {
        let mut rules = self.shared.lock();
        let entry = rules.entry(rule.id).or_insert_with(|| Entry {
            run_id: 0,
            status: watch::Sender::new(ForwardStatus::Stopped),
            run: None,
            stopping: None,
            restart: None,
        });
        let handle = RuleHandle {
            rule_id: rule.id,
            status: entry.status.subscribe(),
        };
        if entry.stopping.is_some() {
            entry.restart = Some(Restart {
                spec: spec_for(rule),
                conn,
            });
            return handle;
        }
        if entry.status.borrow().is_running() {
            return handle;
        }
        // A run that ended by itself (Error) has already released its
        // listener; its task is done or about to be.
        Shared::launch(&self.shared, rule.id, entry, spec_for(rule), conn);
        handle
    }

    /// Stops the rule (must be called inside a tokio runtime). The stop
    /// begins when this is called; the returned future resolves once the
    /// listener is closed (a remote forward cancelled on the server) and
    /// every connection has ended, and the rule went `Stopped`. Every
    /// caller, including concurrent ones, waits for that same point, and
    /// the stop completes even if the future is dropped. Resolves to false
    /// if the rule was never started.
    pub fn stop(&self, rule_id: Id) -> impl Future<Output = bool> + Send + 'static {
        let done = self.begin_stop(rule_id);
        async move {
            let Some(mut done) = done else {
                return false;
            };
            // An error means the stop task is gone: it is over either way.
            let _ = done.wait_for(|d| *d).await;
            true
        }
    }

    /// Starts stopping `rule_id` (or joins a stop in progress); the
    /// receiver turns true when it has finished.
    fn begin_stop(&self, rule_id: Id) -> Option<watch::Receiver<bool>> {
        let mut rules = self.shared.lock();
        let entry = rules.get_mut(&rule_id)?;
        // The latest call wins: a start queued during the stop is dropped.
        entry.restart = None;
        if let Some(stopping) = &entry.stopping {
            return Some(stopping.clone());
        }
        let Some(run) = entry.run.take() else {
            // Nothing runs (never ran, or already stopped).
            Shared::publish(&self.shared.events, rule_id, entry, ForwardStatus::Stopped);
            return Some(watch::channel(true).1);
        };
        let (done, stopping) = watch::channel(false);
        entry.stopping = Some(stopping.clone());
        tokio::spawn(finish_stop(
            self.shared.clone(),
            rule_id,
            entry.run_id,
            run,
            done,
        ));
        Some(stopping)
    }

    /// The rule's current status; `None` if it was never started.
    pub fn status(&self, rule_id: Id) -> Option<ForwardStatus> {
        self.shared
            .lock()
            .get(&rule_id)
            .map(|e| e.status.borrow().clone())
    }

    /// A handle following the rule; `None` if it was never started.
    pub fn handle(&self, rule_id: Id) -> Option<RuleHandle> {
        self.shared.lock().get(&rule_id).map(|e| RuleHandle {
            rule_id,
            status: e.status.subscribe(),
        })
    }
}

/// The engine's description of `rule`, or why it cannot run.
fn spec_for(rule: &ForwardRule) -> Result<ForwardSpec, String> {
    let dest = || match (&rule.dest_host, rule.dest_port) {
        (Some(host), Some(port)) if !host.is_empty() && port != 0 => Ok((host.clone(), port)),
        _ => Err("missing destination host or port".to_owned()),
    };
    let (bind_addr, bind_port) = (rule.bind_addr.clone(), rule.bind_port);
    Ok(match rule.kind {
        ForwardKind::Local => {
            let (dest_host, dest_port) = dest()?;
            ForwardSpec::Local {
                bind_addr,
                bind_port,
                dest_host,
                dest_port,
            }
        }
        ForwardKind::Remote => {
            let (dest_host, dest_port) = dest()?;
            ForwardSpec::Remote {
                bind_addr,
                bind_port,
                dest_host,
                dest_port,
            }
        }
        ForwardKind::Dynamic => ForwardSpec::Dynamic {
            bind_addr,
            bind_port,
        },
    })
}

/// One run of a rule: bind, serve until stopped or the connection closes,
/// then release everything.
async fn run_rule(
    shared: Weak<Shared>,
    (rule_id, run_id): (Id, u64),
    spec: Result<ForwardSpec, String>,
    conn: SshConnection,
    mut stop: oneshot::Receiver<()>,
) {
    let set = |status| {
        if let Some(shared) = shared.upgrade() {
            shared.set_status(rule_id, run_id, status);
        }
    };
    let spec = match spec {
        Ok(spec) => spec,
        Err(msg) => return set(ForwardStatus::Error(msg)),
    };
    // Not raced against `stop`: dropping a half-done remote request would
    // leave the server listening. Starting is bounded (a local bind is
    // immediate; a remote request by the connection's setup timeout).
    let mut forward = match Forward::start(conn, &spec).await {
        Ok(forward) => forward,
        Err(e) => return set(ForwardStatus::Error(e.to_string())),
    };
    if !matches!(stop.try_recv(), Err(oneshot::error::TryRecvError::Empty)) {
        // Stopped (or the manager went away) while binding.
        forward.shutdown().await;
        return;
    }
    set(ForwardStatus::Active {
        bound: forward.bound(),
    });
    let ended = tokio::select! {
        end = forward.serve() => Some(end),
        _ = &mut stop => None,
    };
    forward.shutdown().await;
    if let Some(end) = ended {
        set(ForwardStatus::Error(end.to_string()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(kind: ForwardKind, dest: (Option<&str>, Option<u16>)) -> ForwardRule {
        ForwardRule {
            id: uuid::Uuid::now_v7(),
            host_id: uuid::Uuid::now_v7(),
            label: "r".into(),
            kind,
            bind_addr: "127.0.0.1".into(),
            bind_port: 8080,
            dest_host: dest.0.map(str::to_owned),
            dest_port: dest.1,
            autostart: false,
            extra: Default::default(),
        }
    }

    #[test]
    fn specs_need_a_destination_except_dynamic() {
        for kind in [ForwardKind::Local, ForwardKind::Remote] {
            for dest in [
                (None, Some(80)),
                (Some("web"), None),
                (Some(""), Some(80)),
                (Some("web"), Some(0)),
            ] {
                assert_eq!(
                    spec_for(&rule(kind, dest)),
                    Err("missing destination host or port".to_owned())
                );
            }
        }
        assert_eq!(
            spec_for(&rule(ForwardKind::Local, (Some("web"), Some(80)))),
            Ok(ForwardSpec::Local {
                bind_addr: "127.0.0.1".into(),
                bind_port: 8080,
                dest_host: "web".into(),
                dest_port: 80,
            })
        );
        assert_eq!(
            spec_for(&rule(ForwardKind::Dynamic, (None, None))),
            Ok(ForwardSpec::Dynamic {
                bind_addr: "127.0.0.1".into(),
                bind_port: 8080,
            })
        );
    }

    #[test]
    fn statuses_serialize_for_the_ui() {
        let event = ForwardEvent {
            rule_id: uuid::Uuid::nil(),
            status: ForwardStatus::Active {
                bound: "127.0.0.1:8080".parse().unwrap(),
            },
        };
        assert_eq!(
            serde_json::to_value(&event).unwrap(),
            serde_json::json!({
                "ruleId": "00000000-0000-0000-0000-000000000000",
                "status": { "state": "active", "detail": { "bound": "127.0.0.1:8080" } }
            })
        );
        assert_eq!(
            serde_json::to_value(ForwardStatus::Error("address in use".into())).unwrap(),
            serde_json::json!({ "state": "error", "detail": "address in use" })
        );
        assert_eq!(
            serde_json::to_value(ForwardStatus::Stopped).unwrap(),
            serde_json::json!({ "state": "stopped" })
        );
    }
}
