//! Background keep-alive (iOS, opt-in: Settings → Background → Keep
//! sessions alive in the background, off by default; Android: the same
//! setting, on by default, with a foreground service as the driver).
//!
//! iOS suspends an app soon after it leaves the front, and every SSH session
//! and port forward with it. An app with the `location` background mode that
//! is receiving location updates keeps running, so while keep-alive is on
//! **and** at least one session or running forward is open, Tether keeps
//! location updates running at the lowest accuracy (iOS shows its location
//! indicator). With none open they stop at once; off, they never run. The
//! fixes are dropped unread on arrival (`ios::keepalive`): never stored,
//! logged, shown or sent.
//!
//! [`KeepAliveSlot`] holds the policy ([`should_run`]): the choice (the
//! webview keeps it per device and sends it with `keepalive_set` at start-up
//! and on change), the location permission (When In Use; a denial turns the
//! choice off), and the live count, which `AppState` sets whenever a session
//! is registered or removed and on every forward status change. A
//! [`LocationDriver`] (iOS: a `CLLocationManager` on the main thread) asks
//! for the permission and starts and stops the updates; without one
//! (desktop) keep-alive cannot be turned on.
//!
//! Keep-alive only keeps the process running: the auto-lock's clock still
//! counts the time away, and nothing here reads the keychain.

use std::sync::{Arc, Mutex, OnceLock};

use serde::Serialize;
use tauri::State;
use tokio::sync::watch;

use crate::state::{AppState, CmdError, EventSink, events, lock};

/// Whether location updates should run: keep-alive on, the permission
/// given, and at least one session or port forward open.
pub fn should_run(enabled: bool, live: usize, authorized: bool) -> bool {
    enabled && authorized && live > 0
}

/// The app's location permission, as the driver reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    /// Not asked yet.
    NotDetermined,
    /// Refused or restricted (also location services off for the app).
    Denied,
    /// When In Use (or Always, if the user chose it in iOS Settings).
    Granted,
}

/// Starts and stops the location updates (on Android the driver is a
/// foreground service: updates mean the service). Called with the slot's
/// lock held, so implementations must not block on the slot (iOS: they
/// queue the work on the main thread and return).
pub trait LocationDriver: Send + Sync + 'static {
    /// Asks for When In Use permission; the answer comes back through
    /// [`KeepAliveSlot::set_permission`].
    fn request_permission(&self);
    /// Starts (`true`) or stops background location updates. `generation`
    /// grows with every start and stop (Android drops a call that lands
    /// after a newer one; iOS ignores it).
    fn set_updating(&self, on: bool, generation: u64);
    /// The live count changed while running (Android: the notification's
    /// number), with the generation of the start it belongs to. Not called
    /// at start or stop; `set_updating` says those.
    fn live_changed(&self, _live: usize, _generation: u64) {}
}

/// What `keepalive_set` and `keepalive_status` answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeepAliveStatus {
    /// Keep-alive is on.
    pub enabled: bool,
    /// Location permission is given.
    pub authorized: bool,
    /// Location updates are running now (on, permitted, something open).
    pub running: bool,
}

#[derive(Debug, Default)]
struct Policy {
    enabled: bool,
    /// Open sessions plus running forwards.
    live: usize,
    /// What the driver was last told.
    running: bool,
    /// Bumped on every start and stop the driver is told.
    generation: u64,
    /// The status last sent as a `keepalive` event.
    reported: Option<KeepAliveStatus>,
}

/// Keep-alive's state; see the module docs.
pub struct KeepAliveSlot {
    policy: Mutex<Policy>,
    permission: watch::Sender<Permission>,
    driver: OnceLock<Arc<dyn LocationDriver>>,
    /// Where status changes go (`keepalive` events; set by `AppState`).
    events: OnceLock<Arc<dyn EventSink>>,
}

impl Default for KeepAliveSlot {
    fn default() -> Self {
        Self {
            policy: Mutex::default(),
            permission: watch::Sender::new(Permission::NotDetermined),
            driver: OnceLock::new(),
            events: OnceLock::new(),
        }
    }
}

impl KeepAliveSlot {
    /// Sets the driver (once, at app setup on iOS; tests use a fake).
    pub fn install(&self, driver: Arc<dyn LocationDriver>) {
        if self.driver.set(driver).is_err() {
            tracing::warn!("keep-alive driver installed twice; keeping the first");
        }
    }

    /// [`install`](Self::install) for a platform with no permission to ask
    /// (Android: the notification permission does not block a foreground
    /// service, so the answer is always granted).
    pub fn install_authorized(&self, driver: Arc<dyn LocationDriver>) {
        self.install(driver);
        self.set_permission(Permission::Granted);
    }

    /// Sets where status changes are sent (once, by `AppState`).
    pub(crate) fn set_events(&self, events: Arc<dyn EventSink>) {
        let _ = self.events.set(events);
    }

    fn authorized(&self) -> bool {
        *self.permission.borrow() == Permission::Granted
    }

    pub fn status(&self) -> KeepAliveStatus {
        let policy = lock(&self.policy);
        KeepAliveStatus {
            enabled: policy.enabled,
            authorized: self.authorized(),
            running: policy.running,
        }
    }

    /// The permission changed (the driver's delegate). Anything but
    /// granted turns keep-alive off: a denial, a permission withdrawn in iOS
    /// Settings, or "Allow Once" ending (back to not asked). Turning it on
    /// again asks again; nothing here asks by itself.
    pub fn set_permission(&self, permission: Permission) {
        let mut policy = lock(&self.policy);
        self.permission.send_replace(permission);
        if permission != Permission::Granted {
            policy.enabled = false;
        }
        self.apply(&mut policy);
    }

    /// Turns keep-alive on or off. On asks for the permission first if it
    /// was never asked, and waits for the answer; without it keep-alive
    /// stays off (the answer says `enabled: false`). Without a driver
    /// (desktop) on is refused with `unsupported`.
    pub async fn set_enabled(&self, enabled: bool) -> Result<KeepAliveStatus, CmdError> {
        if enabled {
            let Some(driver) = self.driver.get() else {
                return Err(CmdError::new(
                    "unsupported",
                    "background keep-alive is only available on iOS and Android",
                ));
            };
            let mut answer = self.permission.subscribe();
            if *answer.borrow_and_update() == Permission::NotDetermined {
                driver.request_permission();
            }
            // The sender lives in `self`, so the channel cannot close here.
            let permission = *answer
                .wait_for(|p| *p != Permission::NotDetermined)
                .await
                .map_err(|_| CmdError::internal("keep-alive permission channel closed"))?;
            let mut policy = lock(&self.policy);
            policy.enabled = permission == Permission::Granted;
            self.apply(&mut policy);
        } else {
            let mut policy = lock(&self.policy);
            policy.enabled = false;
            self.apply(&mut policy);
        }
        Ok(self.status())
    }

    /// Sets the live count to `count()`, read under the slot's lock so two
    /// updates cannot land out of order. `count` must not take this slot.
    pub fn set_live_with(&self, count: impl FnOnce() -> usize) {
        let mut policy = lock(&self.policy);
        let (before, was_running) = (policy.live, policy.running);
        policy.live = count();
        self.apply(&mut policy);
        if was_running
            && policy.running
            && before != policy.live
            && let Some(driver) = self.driver.get()
        {
            driver.live_changed(policy.live, policy.generation);
        }
    }

    /// Tells the driver when [`should_run`]'s answer changes, and the UI
    /// (a `keepalive` event) when the status does. Sent under the lock, so
    /// events arrive in order.
    fn apply(&self, policy: &mut Policy) {
        let authorized = self.authorized();
        let want = should_run(policy.enabled, policy.live, authorized);
        if want != policy.running
            && let Some(driver) = self.driver.get()
        {
            policy.running = want;
            policy.generation += 1;
            tracing::info!(running = want, "background keep-alive");
            driver.set_updating(want, policy.generation);
        }
        let status = KeepAliveStatus {
            enabled: policy.enabled,
            authorized,
            running: policy.running,
        };
        if policy.reported == Some(status) {
            return;
        }
        policy.reported = Some(status);
        if let Some(sink) = self.events.get() {
            let sent = serde_json::to_value(status)
                .map_err(|e| e.to_string())
                .and_then(|payload| sink.emit(events::KEEPALIVE, payload));
            if let Err(e) = sent {
                tracing::debug!(error = %e, "keep-alive status not delivered");
            }
        }
    }
}

impl AppState {
    /// Keep-alive (the iOS setup installs its driver).
    pub fn keepalive(&self) -> Arc<KeepAliveSlot> {
        self.0.keepalive.clone()
    }

    /// Open sessions plus running port forwards: what keep-alive keeps.
    pub(crate) fn live_count(&self) -> usize {
        let sessions = self.session_count();
        let started: Vec<_> = lock(&self.0.forward_hosts).keys().copied().collect();
        let forwards = started
            .into_iter()
            .filter(|rule| {
                self.0
                    .forwards
                    .status(*rule)
                    .is_some_and(|s| s.is_running())
            })
            .count();
        sessions + forwards
    }

    /// Recounts the live work for keep-alive. Call after a session is
    /// registered or removed and after a forward's status changes, with no
    /// registry or forward lock held.
    pub(crate) fn refresh_keepalive(&self) {
        self.0.keepalive.set_live_with(|| self.live_count());
    }
}

/// Turns keep-alive on or off (on: after asking for location permission if
/// needed). Answers the resulting state.
#[tauri::command]
pub async fn keepalive_set(
    state: State<'_, AppState>,
    enabled: bool,
) -> Result<KeepAliveStatus, CmdError> {
    state.keepalive().set_enabled(enabled).await
}

#[tauri::command]
pub async fn keepalive_status(state: State<'_, AppState>) -> Result<KeepAliveStatus, CmdError> {
    Ok(state.keepalive().status())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_run_only_when_on_permitted_and_something_is_open() {
        // (enabled, live, authorized) -> run
        let table = [
            (false, 0, false, false),
            (false, 0, true, false),
            (false, 1, false, false),
            (false, 1, true, false),
            (false, 3, true, false),
            (true, 0, false, false),
            (true, 0, true, false),
            (true, 1, false, false),
            (true, 2, false, false),
            (true, 1, true, true),
            (true, 2, true, true),
        ];
        for (enabled, live, authorized, run) in table {
            assert_eq!(
                should_run(enabled, live, authorized),
                run,
                "enabled={enabled} live={live} authorized={authorized}"
            );
        }
    }

    /// Records what it is told; answers the permission request with
    /// `answer` (`None`: leaves the question open).
    #[derive(Default)]
    struct FakeDriver {
        calls: Mutex<Vec<&'static str>>,
        lives: Mutex<Vec<usize>>,
        generations: Mutex<Vec<(&'static str, u64)>>,
        answer: Mutex<Option<(Arc<KeepAliveSlot>, Permission)>>,
    }

    impl LocationDriver for FakeDriver {
        fn request_permission(&self) {
            lock(&self.calls).push("ask");
            if let Some((slot, p)) = lock(&self.answer).take() {
                // Answered later on another thread, like the delegate.
                std::thread::spawn(move || slot.set_permission(p));
            }
        }
        fn set_updating(&self, on: bool, generation: u64) {
            let call = if on { "start" } else { "stop" };
            lock(&self.calls).push(call);
            lock(&self.generations).push((call, generation));
        }
        fn live_changed(&self, live: usize, generation: u64) {
            lock(&self.calls).push("live");
            lock(&self.lives).push(live);
            lock(&self.generations).push(("live", generation));
        }
    }

    fn slot_with(answer: Option<Permission>) -> (Arc<KeepAliveSlot>, Arc<FakeDriver>) {
        let slot = Arc::new(KeepAliveSlot::default());
        let driver = Arc::new(FakeDriver::default());
        if let Some(p) = answer {
            *lock(&driver.answer) = Some((slot.clone(), p));
        }
        slot.install(driver.clone());
        (slot, driver)
    }

    fn calls(d: &FakeDriver) -> Vec<&'static str> {
        lock(&d.calls).clone()
    }

    #[tokio::test]
    async fn on_asks_once_then_runs_only_while_something_is_open() {
        let (slot, driver) = slot_with(Some(Permission::Granted));
        let status = slot.set_enabled(true).await.unwrap();
        assert_eq!(
            status,
            KeepAliveStatus {
                enabled: true,
                authorized: true,
                running: false
            }
        );
        assert_eq!(calls(&driver), ["ask"]);

        slot.set_live_with(|| 1);
        assert!(slot.status().running);
        slot.set_live_with(|| 2);
        slot.set_live_with(|| 0);
        assert!(!slot.status().running);
        assert_eq!(calls(&driver), ["ask", "start", "live", "stop"]);

        // Already permitted: no second question; off stops at once.
        slot.set_enabled(false).await.unwrap();
        slot.set_live_with(|| 1);
        assert_eq!(calls(&driver), ["ask", "start", "live", "stop"]);
        slot.set_enabled(true).await.unwrap();
        assert_eq!(calls(&driver), ["ask", "start", "live", "stop", "start"]);
        let status = slot.set_enabled(false).await.unwrap();
        assert!(!status.enabled && status.authorized && !status.running);
        assert_eq!(calls(&driver), ["ask", "start", "live", "stop", "start", "stop"]);
    }

    #[tokio::test]
    async fn denial_leaves_it_off() {
        let (slot, driver) = slot_with(Some(Permission::Denied));
        slot.set_live_with(|| 1);
        let status = slot.set_enabled(true).await.unwrap();
        assert_eq!(
            status,
            KeepAliveStatus {
                enabled: false,
                authorized: false,
                running: false
            }
        );
        // Denied already: not asked again, still off.
        let status = slot.set_enabled(true).await.unwrap();
        assert!(!status.enabled);
        assert_eq!(calls(&driver), ["ask"]);
    }

    /// "Allow Once" ends: iOS puts the permission back to not asked. That
    /// turns keep-alive off like a denial (the box must not stay checked
    /// while nothing runs).
    #[tokio::test]
    async fn allow_once_lapsing_turns_it_off() {
        let (slot, driver) = slot_with(None);
        slot.set_permission(Permission::Granted);
        slot.set_live_with(|| 1);
        slot.set_enabled(true).await.unwrap();
        assert!(slot.status().running);
        slot.set_permission(Permission::NotDetermined);
        assert_eq!(
            slot.status(),
            KeepAliveStatus {
                enabled: false,
                authorized: false,
                running: false
            }
        );
        assert_eq!(calls(&driver), ["start", "stop"]);
    }

    /// Records the `keepalive` events.
    #[derive(Default)]
    struct Events(Mutex<Vec<(String, serde_json::Value)>>);

    impl EventSink for Events {
        fn emit(&self, event: &str, payload: serde_json::Value) -> Result<(), String> {
            lock(&self.0).push((event.to_owned(), payload));
            Ok(())
        }
    }

    /// Every change of the status goes to the UI once, so Settings and the
    /// stored choice follow a permission withdrawn in iOS Settings.
    #[tokio::test]
    async fn each_status_change_is_sent_once() {
        let (slot, _driver) = slot_with(None);
        let events = Arc::new(Events::default());
        slot.set_events(events.clone());
        slot.set_permission(Permission::Granted);
        slot.set_enabled(true).await.unwrap();
        slot.set_live_with(|| 1);
        slot.set_live_with(|| 2);
        slot.set_permission(Permission::Denied);
        let sent: Vec<_> = lock(&events.0).clone();
        assert!(sent.iter().all(|(name, _)| name == "keepalive"), "{sent:?}");
        let payloads: Vec<_> = sent.into_iter().map(|(_, p)| p).collect();
        assert_eq!(
            payloads,
            [
                serde_json::json!({"enabled": false, "authorized": true, "running": false}),
                serde_json::json!({"enabled": true, "authorized": true, "running": false}),
                serde_json::json!({"enabled": true, "authorized": true, "running": true}),
                serde_json::json!({"enabled": false, "authorized": false, "running": false}),
            ]
        );
    }

    #[tokio::test]
    async fn permission_withdrawn_later_stops_and_turns_it_off() {
        let (slot, driver) = slot_with(None);
        slot.set_permission(Permission::Granted);
        slot.set_live_with(|| 1);
        slot.set_enabled(true).await.unwrap();
        assert!(slot.status().running);
        slot.set_permission(Permission::Denied);
        assert_eq!(
            slot.status(),
            KeepAliveStatus {
                enabled: false,
                authorized: false,
                running: false
            }
        );
        // Given again in iOS Settings: stays off until turned on.
        slot.set_permission(Permission::Granted);
        assert!(!slot.status().running);
        assert_eq!(calls(&driver), ["start", "stop"]);
    }

    #[tokio::test]
    async fn without_a_driver_on_is_unsupported() {
        let slot = KeepAliveSlot::default();
        slot.set_live_with(|| 1);
        assert_eq!(
            slot.set_enabled(true).await.unwrap_err().code,
            "unsupported"
        );
        assert_eq!(
            slot.set_enabled(false).await.unwrap(),
            KeepAliveStatus {
                enabled: false,
                authorized: false,
                running: false
            }
        );
    }

    /// Android: the notification permission does not block a foreground
    /// service, so the driver is installed as already permitted and turning
    /// keep-alive on never asks.
    #[tokio::test]
    async fn android_driver_is_always_authorized() {
        let slot = KeepAliveSlot::default();
        let driver = Arc::new(FakeDriver::default());
        slot.install_authorized(driver.clone());
        assert!(slot.status().authorized);
        let status = slot.set_enabled(true).await.unwrap();
        assert!(status.enabled && status.authorized);
        assert!(calls(&driver).is_empty(), "never asks: {:?}", calls(&driver));
    }

    /// A change of the count while running reaches the driver (the
    /// notification's number); starting and stopping are `set_updating`.
    #[tokio::test]
    async fn live_changed_reports_counts() {
        let (slot, driver) = slot_with(None);
        slot.set_permission(Permission::Granted);
        // Off: counts are not the driver's business.
        slot.set_live_with(|| 3);
        slot.set_live_with(|| 0);
        assert!(calls(&driver).is_empty());
        slot.set_enabled(true).await.unwrap();
        slot.set_live_with(|| 1);
        slot.set_live_with(|| 1);
        slot.set_live_with(|| 2);
        slot.set_live_with(|| 0);
        assert_eq!(calls(&driver), ["start", "live", "stop"]);
        assert_eq!(*lock(&driver.lives), [2]);
    }

    /// Every start and stop gets a newer generation and a count carries its
    /// start's, so Android can drop a count or start that lands after a
    /// later stop (the driver's calls are bounded, so they can land late).
    #[tokio::test]
    async fn generations_increase_and_counts_carry_their_start() {
        let (slot, driver) = slot_with(None);
        slot.set_permission(Permission::Granted);
        slot.set_enabled(true).await.unwrap();
        slot.set_live_with(|| 1);
        slot.set_live_with(|| 2);
        slot.set_live_with(|| 0);
        slot.set_live_with(|| 1);
        slot.set_enabled(false).await.unwrap();
        assert_eq!(
            *lock(&driver.generations),
            [("start", 1), ("live", 1), ("stop", 2), ("start", 3), ("stop", 4)]
        );
    }
}
