//! Auto-lock: locks the vault once Tether has been away from the front for
//! the time chosen in Settings → Security → Auto-lock (per device; the
//! webview keeps the choice and sends it with `set_auto_lock` at start-up
//! and on change). No limit (Never) locks only on quit, as before.
//!
//! Tether is **in front only while its window is both focused and visible**
//! (on iOS: the scene active and the page shown). Two signals feed it, each
//! kept on its own: the window's focus events (here, on the main thread, in
//! order; tao maps the iOS scene becoming or resigning active to them) and
//! the webview's `visibilitychange` (`app_front`). The first to go false
//! starts the absence; it ends only once both are true again. So a window
//! shown again but not focused is still away, and time in front never
//! counts (a long `htop` in front never locks).
//!
//! The vault locks, with [`LockReason::Idle`], through the same path as Lock
//! vault (sessions keep running under the unlock screen):
//! * on any report past the limit: coming back, or a report that leaves
//!   Tether still away (shown but unfocused). The UI holds its panes
//!   inactive until `app_front(true)` answers whether the vault is locked,
//!   so no reconnect can start before the lock is in place;
//! * from a [`CHECK_EVERY`] timer, for an app that stays away.
//!
//! An unlock ends the absence (someone is here): in front it is over; still
//! away (unlocked while a Touch ID prompt had the focus, say), a new one
//! starts then, so the next lock comes a full limit later.
//!
//! Elapsed time is the larger of the monotonic clock and the wall clock:
//! the monotonic clock stops while macOS or iOS sleeps, and a wall clock set
//! back must not undo time away. Either can only bring the lock sooner.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

use tauri::State;

use crate::state::{AppState, CmdError, lock};
use crate::vault_cmds::LockReason;

/// How often the timer checks an app that is away.
pub const CHECK_EVERY: Duration = Duration::from_secs(30);

/// A point in time on both clocks.
#[derive(Debug, Clone, Copy)]
pub struct Moment {
    pub mono: Instant,
    pub wall: SystemTime,
}

impl Moment {
    pub fn now() -> Self {
        Self {
            mono: Instant::now(),
            wall: SystemTime::now(),
        }
    }

    /// Time from `earlier` to `self`: the larger of the two clocks' answers.
    fn since(self, earlier: Moment) -> Duration {
        let mono = self.mono.saturating_duration_since(earlier.mono);
        let wall = self
            .wall
            .duration_since(earlier.wall)
            .unwrap_or(Duration::ZERO);
        mono.max(wall)
    }
}

/// Where Tether is, for the auto-lock.
#[derive(Debug, Clone, Copy, Default)]
enum Presence {
    #[default]
    Front,
    /// Away since then.
    Away(Moment),
    /// Still away, and this absence has already locked the vault.
    AwayLocked,
}

/// The auto-lock's clock: the window's focus and visibility, when Tether
/// left the front, and the limit. Pure; all time comes in as arguments.
#[derive(Debug)]
pub struct AutoLock {
    limit: Option<Duration>,
    focused: bool,
    visible: bool,
    presence: Presence,
}

impl Default for AutoLock {
    /// In front (the window starts focused and shown), limit Never.
    fn default() -> Self {
        Self {
            limit: None,
            focused: true,
            visible: true,
            presence: Presence::Front,
        }
    }
}

impl AutoLock {
    /// `None`: never lock.
    pub fn set_limit(&mut self, limit: Option<Duration>) {
        self.limit = limit;
    }

    /// The window gained or lost focus (iOS: the scene became or resigned
    /// active). True = lock now.
    pub fn set_focused(&mut self, focused: bool, now: Moment) -> bool {
        self.focused = focused;
        self.update(now)
    }

    /// The page was shown or hidden. True = lock now.
    pub fn set_visible(&mut self, visible: bool, now: Moment) -> bool {
        self.visible = visible;
        self.update(now)
    }

    /// In front only with both signals true. Still away and past the limit
    /// (shown again but not focused): lock now, once for this absence.
    fn update(&mut self, now: Moment) -> bool {
        if self.focused && self.visible {
            return self.came_front(now);
        }
        self.left_front(now);
        if self.due(now) {
            self.locked_while_away();
            return true;
        }
        false
    }

    /// Tether left the front. A second report while away (unfocused, then
    /// hidden) keeps the first time.
    pub fn left_front(&mut self, now: Moment) {
        if let Presence::Front = self.presence {
            self.presence = Presence::Away(now);
        }
    }

    /// Tether is back in front: true = it was away for the limit or longer
    /// and the vault must lock now.
    pub fn came_front(&mut self, now: Moment) -> bool {
        let lock = self.due(now);
        self.presence = Presence::Front;
        lock
    }

    /// Still away, for the limit or longer, and not locked yet this absence.
    pub fn due(&self, now: Moment) -> bool {
        match (self.presence, self.limit) {
            (Presence::Away(since), Some(limit)) => now.since(since) >= limit,
            _ => false,
        }
    }

    /// The vault locked while away: nothing more to lock until the next absence.
    pub fn locked_while_away(&mut self) {
        if let Presence::Away(_) = self.presence {
            self.presence = Presence::AwayLocked;
        }
    }

    /// The vault was unlocked: the absence is over. Still away by the
    /// signals, a new absence starts now.
    pub fn unlocked(&mut self, now: Moment) {
        self.presence = if self.focused && self.visible {
            Presence::Front
        } else {
            Presence::Away(now)
        };
    }
}

/// The auto-lock in [`AppState`], shared by the window events, the commands
/// and the timer.
///
/// The clock is a plain mutex, updated synchronously where a move is seen
/// (the window event handler runs on the main thread, in order), so a move
/// reported before a suspend is never applied after the return. A lock it
/// decides on is *owed* until one runs; locks run under `locking`, so whoever
/// asks next (the webview's `app_front`) waits for it and sees the vault
/// locked.
#[derive(Default)]
pub struct AutoLockSlot {
    clock: Mutex<AutoLock>,
    owed: AtomicBool,
    locking: tokio::sync::Mutex<()>,
}

impl AutoLockSlot {
    /// `None`: Never. Zero minutes is refused.
    pub fn set_minutes(&self, minutes: Option<u32>) -> Result<(), CmdError> {
        let limit = match minutes {
            Some(0) => return Err(CmdError::invalid("auto-lock needs at least one minute")),
            Some(m) => Some(Duration::from_secs(u64::from(m) * 60)),
            None => None,
        };
        lock(&self.clock).set_limit(limit);
        Ok(())
    }

    /// Records `apply`'s verdict; true: a lock is owed (run it with
    /// [`AutoLockSlot::settle`]).
    fn note(&self, apply: impl FnOnce(&mut AutoLock) -> bool) -> bool {
        if apply(&mut lock(&self.clock)) {
            self.owed.store(true, Ordering::SeqCst);
        }
        self.owed.load(Ordering::SeqCst)
    }

    /// The window's focus changed at `now`. True: a lock is owed.
    pub fn note_focus(&self, focused: bool, now: Moment) -> bool {
        self.note(|c| c.set_focused(focused, now))
    }

    /// The page was shown or hidden at `now`. True: a lock is owed.
    pub fn note_visible(&self, visible: bool, now: Moment) -> bool {
        self.note(|c| c.set_visible(visible, now))
    }

    /// The timer's check. True: a lock is owed.
    pub fn note_due(&self, now: Moment) -> bool {
        self.note(|c| {
            let due = c.due(now);
            if due {
                c.locked_while_away();
            }
            due
        })
    }

    /// The vault was unlocked: the absence is over, and a lock owed for it
    /// is dropped (the person it was for is here).
    pub fn unlocked(&self, now: Moment) {
        lock(&self.clock).unlocked(now);
        self.owed.store(false, Ordering::SeqCst);
    }

    /// Runs an owed lock, if any (after one in progress elsewhere). A lock
    /// that fails stays owed, for the next check.
    pub async fn settle(&self, state: &AppState) {
        let _running = self.locking.lock().await;
        if !self.owed.swap(false, Ordering::SeqCst) {
            return;
        }
        if let Err(e) = state.lock_vault(LockReason::Idle).await {
            self.owed.store(true, Ordering::SeqCst);
            tracing::warn!(error = %e, "auto-lock could not lock the vault");
        }
    }

    /// The webview's report (`app_front`): records the page shown or
    /// hidden, runs any owed lock, and answers whether the vault is locked
    /// now. Unknown (the vault cannot be read) answers locked.
    pub async fn front(&self, state: &AppState, visible: bool, now: Moment) -> bool {
        self.note_visible(visible, now);
        self.settle(state).await;
        state
            .with_vault(|v| Ok(!v.is_unlocked()))
            .await
            .unwrap_or(true)
    }
}

/// A window focus change, from the event loop: recorded at once, any owed
/// lock run in the background.
pub fn on_focus(app: &tauri::AppHandle, focused: bool) {
    use tauri::Manager;
    let now = Moment::now();
    let Some(state) = app.try_state::<AppState>().map(|s| s.inner().clone()) else {
        return;
    };
    if focused {
        state.came_to_front();
    }
    let slot = state.autolock();
    if slot.note_focus(focused, now) {
        tauri::async_runtime::spawn(async move { slot.settle(&state).await });
    }
}

/// Starts the [`CHECK_EVERY`] timer for an app that stays away.
pub fn spawn_timer(state: AppState) {
    tauri::async_runtime::spawn(async move {
        let slot = state.autolock();
        let mut every = tokio::time::interval(CHECK_EVERY);
        every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            every.tick().await;
            if slot.note_due(Moment::now()) {
                slot.settle(&state).await;
            }
        }
    });
}

#[tauri::command]
pub async fn set_auto_lock(
    state: State<'_, AppState>,
    minutes: Option<u32>,
) -> Result<(), CmdError> {
    state.autolock().set_minutes(minutes)
}

/// The page went to the background (`front: false`) or was shown again.
/// Answers whether the vault is locked; when shown, after any lock it called
/// for.
#[tauri::command]
pub async fn app_front(state: State<'_, AppState>, front: bool) -> Result<bool, CmdError> {
    if front {
        state.came_to_front();
    }
    Ok(state.autolock().front(&state, front, Moment::now()).await)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: Duration = Duration::from_secs(60);

    /// A clock the tests move by hand; both clocks advance together unless a test says otherwise.
    struct Clock(Moment);

    impl Clock {
        fn new() -> Self {
            Self(Moment::now())
        }
        fn at(&self) -> Moment {
            self.0
        }
        fn advance(&mut self, d: Duration) -> Moment {
            self.0 = Moment {
                mono: self.0.mono + d,
                wall: self.0.wall + d,
            };
            self.0
        }
    }

    fn with_limit(minutes: u32) -> AutoLock {
        let mut a = AutoLock::default();
        a.set_limit(Some(minutes * MIN));
        a
    }

    #[test]
    fn never_limit_never_locks() {
        let mut clock = Clock::new();
        let mut a = AutoLock::default();
        a.left_front(clock.at());
        let later = clock.advance(24 * 60 * MIN);
        assert!(!a.due(later));
        assert!(!a.came_front(later));
    }

    #[test]
    fn in_front_never_locks() {
        let mut clock = Clock::new();
        let mut a = with_limit(5);
        // A long `htop` in front: hours pass, nobody leaves.
        let later = clock.advance(10 * 60 * MIN);
        assert!(!a.due(later));
        assert!(!a.came_front(later));
    }

    #[test]
    fn background_past_limit_locks_on_return() {
        let mut clock = Clock::new();
        let mut a = with_limit(5);
        a.left_front(clock.at());
        assert!(a.came_front(clock.advance(6 * MIN)));
        // Back in front: the absence is over, nothing more to lock.
        assert!(!a.due(clock.advance(10 * MIN)));
        assert!(!a.came_front(clock.at()));
    }

    #[test]
    fn exactly_at_the_limit_locks() {
        let mut clock = Clock::new();
        let mut a = with_limit(5);
        a.left_front(clock.at());
        assert!(a.came_front(clock.advance(5 * MIN)));
    }

    #[test]
    fn background_short_of_limit_does_not_lock() {
        let mut clock = Clock::new();
        let mut a = with_limit(5);
        a.left_front(clock.at());
        assert!(!a.due(clock.advance(4 * MIN)));
        assert!(!a.came_front(clock.at()));
        // Time away does not add up across absences.
        a.left_front(clock.advance(MIN));
        assert!(!a.came_front(clock.advance(4 * MIN)));
    }

    #[test]
    fn timer_locks_when_due_in_background() {
        let mut clock = Clock::new();
        let mut a = with_limit(5);
        a.left_front(clock.at());
        assert!(!a.due(clock.advance(4 * MIN)));
        assert!(a.due(clock.advance(MIN)));
        a.locked_while_away();
        // Locked once for this absence: no second lock while still away, nor on return.
        assert!(!a.due(clock.advance(30 * MIN)));
        assert!(!a.came_front(clock.at()));
        // The next absence counts again.
        a.left_front(clock.at());
        assert!(a.due(clock.advance(5 * MIN)));
    }

    #[test]
    fn leaving_twice_keeps_the_first_time() {
        // Unfocused, then hidden: the absence started when focus went.
        let mut clock = Clock::new();
        let mut a = with_limit(5);
        a.left_front(clock.at());
        a.left_front(clock.advance(3 * MIN));
        assert!(a.came_front(clock.advance(2 * MIN)));
    }

    #[test]
    fn coming_back_twice_does_not_lock_twice() {
        // Focus and visibility both report the return.
        let mut clock = Clock::new();
        let mut a = with_limit(5);
        a.left_front(clock.at());
        assert!(a.came_front(clock.advance(6 * MIN)));
        assert!(!a.came_front(clock.at()));
    }

    #[test]
    fn a_limit_set_while_away_applies_on_return() {
        let mut clock = Clock::new();
        let mut a = AutoLock::default();
        a.left_front(clock.at());
        a.set_limit(Some(5 * MIN));
        assert!(a.came_front(clock.advance(6 * MIN)));
    }

    // -- focus and visibility, each kept on its own --------------------------

    #[test]
    fn shown_again_but_unfocused_is_still_away() {
        // Focus goes, another window covers Tether, then moves off it: Tether
        // is visible again but nobody has come back to it.
        let mut clock = Clock::new();
        let mut a = with_limit(5);
        assert!(!a.set_focused(false, clock.at()));
        assert!(!a.set_visible(false, clock.advance(MIN)));
        assert!(!a.set_visible(true, clock.advance(MIN)));
        assert!(!a.due(clock.advance(2 * MIN)), "4 minutes away");
        // The absence started with the focus loss and goes on.
        assert!(a.due(clock.advance(MIN)));
        // Coming back now locks.
        assert!(a.set_focused(true, clock.advance(MIN)));
        assert!(!a.due(clock.advance(60 * MIN)), "in front again");
    }

    #[test]
    fn a_report_past_the_limit_while_still_away_locks_at_once() {
        // Shown again (but unfocused) after the limit: lock now, not at focus.
        let mut clock = Clock::new();
        let mut a = with_limit(5);
        a.set_focused(false, clock.at());
        a.set_visible(false, clock.advance(MIN));
        assert!(a.set_visible(true, clock.advance(6 * MIN)));
        // Once per absence: the focus coming back afterwards does not lock again.
        assert!(!a.set_focused(true, clock.advance(MIN)));
    }

    #[test]
    fn focus_and_visibility_reporting_one_return_lock_once() {
        // iOS: the scene resigns active and the page hides; both come back.
        for focus_first in [true, false] {
            let mut clock = Clock::new();
            let mut a = with_limit(30);
            a.set_focused(false, clock.at());
            a.set_visible(false, clock.at());
            let back = clock.advance(31 * MIN);
            let (first, second) = if focus_first {
                (a.set_focused(true, back), a.set_visible(true, back))
            } else {
                (a.set_visible(true, back), a.set_focused(true, back))
            };
            assert!(first, "focus first: {focus_first}");
            assert!(!second, "focus first: {focus_first}");
        }
    }

    #[test]
    fn in_front_needs_both_signals() {
        let mut clock = Clock::new();
        let mut a = with_limit(5);
        a.set_visible(false, clock.at());
        // Focus without the page shown is not in front.
        assert!(!a.set_focused(true, clock.advance(MIN)));
        assert!(a.due(clock.advance(5 * MIN)));
    }

    #[test]
    fn an_unlock_ends_the_absence() {
        let mut clock = Clock::new();
        let mut a = with_limit(5);
        a.set_focused(false, clock.at());
        assert!(a.due(clock.advance(5 * MIN)));
        a.locked_while_away();
        // Unlocked while still unfocused (a Touch ID prompt has the focus):
        // a fresh absence starts, and locks a full limit later.
        a.unlocked(clock.advance(MIN));
        assert!(!a.due(clock.advance(4 * MIN)));
        assert!(a.due(clock.advance(MIN)));
        // Unlocked in front: nothing is being timed.
        let mut b = with_limit(5);
        b.left_front(clock.at());
        b.unlocked(clock.advance(MIN));
        assert!(!b.due(clock.advance(60 * MIN)));
    }

    #[test]
    fn sleep_counts_through_the_wall_clock() {
        // macOS and iOS: the monotonic clock stops while the machine sleeps.
        let clock = Clock::new();
        let mut a = with_limit(30);
        a.left_front(clock.at());
        let woke = Moment {
            mono: clock.at().mono + Duration::from_secs(2),
            wall: clock.at().wall + 8 * 60 * MIN,
        };
        assert!(a.due(woke));
        assert!(a.came_front(woke));
    }

    #[test]
    fn a_wall_clock_set_back_does_not_undo_time_away() {
        let clock = Clock::new();
        let mut a = with_limit(5);
        a.left_front(clock.at());
        let back = Moment {
            mono: clock.at().mono + 6 * MIN,
            wall: clock.at().wall - 60 * MIN,
        };
        assert!(a.came_front(back));
    }
}
