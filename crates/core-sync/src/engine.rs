//! The sync engine: one tokio task per synced vault that pulls the change
//! log, merges it, pushes local edits and keeps the account material current.
//!
//! [`Engine`] is a state machine; [`Engine::tick`] does one bounded unit of
//! work (sign in from the stored credentials, fetch the account, pull a page,
//! apply the queue, push a batch, repair) and says what to do next
//! ([`TickOutcome`]). The driver loop ([`start`]) ticks it, and between
//! cycles waits for a trigger: a WebSocket event, [`SyncHandle::sync_now`],
//! a local change (debounced 500 ms), an unlock, or the 5-minute timer.
//!
//! A cycle is: account (when asked) → pull pages into the inbound queue
//! (works locked) → apply the whole queue once the pull is caught up (the
//! cascade rule judges owners against the whole queue) → push the outbox in
//! batches → repair, only when caught up and nothing is unpushed. The vault
//! lock is taken for each store call and never held across an `.await`, and
//! no inbound row is applied between sending a push and recording its acks
//! (an ack writes `rev`, which must not regress past a newer pulled row).
//!
//! Full resync (epoch changed, `410 gone` on pull or push, a cursor ahead of
//! the server, or a newer kind set): every `rev` is forgotten but the dirty
//! flags stay, the queue is emptied, everything is pulled from `since = 0`
//! and applied, then the live rows the server lacks are marked dirty and
//! pushed as new.
//!
//! A credential store that is only out of reach until the device unlocks
//! ([`SyncError::StorageLocked`]: the iOS keychain while the phone is locked,
//! with the app kept alive in the background) pauses the engine: no cycle,
//! no event socket, so no refresh rotates a token it could not save. Unlike
//! a keychain that does not work at all it never moves the credentials to
//! the vault database. The pause ends on [`SyncHandle::notify_foreground`],
//! an unlock, "Sync now" or the 5-minute timer; a retry still locked pauses
//! again before reaching the server (see [`TokenStore::ready`]).

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use core_api::vault::SharedVault;
use core_model::{Hlc, Id, KIND_SET_VERSION};
use core_proto::{ErrorCode, Event, PushItem, PushReq, PushResult, WireRow};
use core_store::{InboundRow, RecordRow};
use futures_util::StreamExt;
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::client::Client;
use crate::error::SyncError;
use crate::fallback;
use crate::state::{self, SyncConfig, SyncErrorKind, SyncNotice, SyncStatus, keys, with_vault};
use crate::tokens::TokenStore;
use crate::ws;

/// Rows per pulled page.
const PAGE: u32 = 500;
/// Rows per push request (the server's maximum).
const PUSH_ITEMS: usize = 500;
/// Estimated JSON size per push request. The protocol allows 4 MiB, but the
/// server's JSON extractor still applies axum's default 2 MiB limit, so stay
/// well below that (one maximal 1 MiB envelope still fits on its own).
const PUSH_BYTES: usize = 1536 * 1024;
/// The server's envelope limit; a larger record cannot be pushed.
const MAX_ENVELOPE: usize = 1024 * 1024;
/// Local edits are pushed this long after the first one of a burst.
const DEBOUNCE: Duration = Duration::from_millis(500);
/// A sync runs at least this often.
const PERIODIC: Duration = Duration::from_secs(5 * 60);
/// Retry delays after a failed cycle or socket connection (jittered).
const BACKOFF_MIN: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(5 * 60);

/// What a [`Engine::tick`] asks the driver to do next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TickOutcome {
    /// More work is queued: tick again.
    Progress,
    /// Nothing to do until a trigger.
    Idle,
    /// The server is unreachable: retry after this delay (or on a trigger).
    Backoff(Duration),
    /// The engine is finished (device revoked, or a simulated crash).
    Stopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    /// Load the stored credentials.
    Resume,
    Idle,
    Account,
    Pull,
    Apply,
    Push,
    Repair,
}

/// Why the engine stopped trying on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Halt {
    /// Permanent.
    Revoked,
    /// Until "Sync now".
    CertificateChanged,
    /// Until "Sync now" (no session).
    SignIn,
}

/// The sync state machine for one vault. The app runs it through [`start`];
/// tests and simulations can drive it with [`Engine::tick`] (feature
/// `test-util`).
pub struct Engine {
    vault: SharedVault,
    client: Client,
    device: Id,
    step: Step,
    want_account: bool,
    want_sync: bool,
    resumed: bool,
    halt: Option<Halt>,
    backoff: Duration,
    done: usize,
    total: Option<usize>,
    /// Unix ms of the last completed sync.
    last_sync: i64,
    using_fallback: bool,
    /// Paused: the credential store is out of reach until the device is
    /// unlocked ([`SyncError::StorageLocked`]).
    keychain_locked: bool,
    /// Unreadable rows from the server skipped in this cycle.
    skipped: usize,
    /// Local records the server refuses (too large, or isolated by a
    /// refused push); not pushed again until "Sync now" or a restart.
    held: HashSet<Id>,
    /// Items per push; halved while narrowing down a refused request.
    push_cap: usize,
    /// No automatic cycle before this (backoff after a failure).
    not_before: Option<Instant>,
    status: watch::Sender<SyncStatus>,
    notices: broadcast::Sender<SyncNotice>,
    /// Serializes account-material writes with the account flows.
    gate: Arc<tokio::sync::Mutex<()>>,
    /// Callers waiting for the next cycle to finish.
    waiters: Vec<oneshot::Sender<SyncStatus>>,
    /// Callers the running cycle will answer.
    cycle_waiters: Vec<oneshot::Sender<SyncStatus>>,
    crash_before_ack: bool,
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// `d` scaled by a random factor in [0.75, 1.25), so devices that failed
/// together do not retry together.
fn jittered(d: Duration) -> Duration {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
    );
    let frac = (h.finish() % 1000) as f64 / 1000.0;
    d.mul_f64(0.75 + 0.5 * frac)
}

fn bad_row() -> SyncError {
    SyncError::Protocol("the server sent a malformed row".into())
}

/// A pulled (or conflict) row as the inbound queue holds it.
fn inbound(row: WireRow) -> Result<InboundRow, SyncError> {
    Ok(InboundRow {
        seq: row.seq.unwrap_or(0),
        id: row.id.parse().map_err(|_| bad_row())?,
        hlc: row.hlc.parse().map_err(|_| bad_row())?,
        rev: row.rev.ok_or_else(bad_row)?,
        kind: row.kind,
        key_version: row.key_version,
        deleted: row.deleted,
        envelope: row.envelope,
    })
}

fn wire(row: &RecordRow) -> WireRow {
    WireRow {
        id: row.id.to_string(),
        kind: row.kind.as_str().to_owned(),
        key_version: row.key_version,
        hlc: row.hlc.to_string(),
        rev: None,
        seq: None,
        deleted: row.deleted,
        envelope: if row.deleted {
            Vec::new()
        } else {
            row.envelope.clone()
        },
    }
}

/// Rough JSON size of a pushed row: base64 envelope plus field overhead.
fn push_size(row: &RecordRow) -> usize {
    row.envelope.len().div_ceil(3) * 4 + 256
}

/// Pull bookkeeping read at the start of a page.
struct Cursor {
    cursor: i64,
    epoch: Option<String>,
    resync: bool,
    resync_epoch: Option<String>,
    kinds_version: i64,
}

impl Engine {
    /// The engine for `vault`, or `None` if sync is not enabled there.
    /// `tokens` is the primary credential store (the keychain in the app);
    /// a vault that switched to the fallback keeps using it.
    pub fn new(
        vault: SharedVault,
        tokens: Arc<dyn TokenStore>,
    ) -> Result<Option<Engine>, SyncError> {
        let (cfg, store, using_fallback, last_sync) = {
            let v = vault.lock().unwrap_or_else(|p| p.into_inner());
            let Some(cfg) = SyncConfig::load(&v)? else {
                return Ok(None);
            };
            let store = fallback::credential_store(&v, &vault, tokens)?;
            let last = state::get_i64(&v, keys::LAST_SYNC)?.unwrap_or(0);
            (cfg, store, fallback::uses_fallback(&v)?, last)
        };
        let client = Client::new(&cfg.url, cfg.pin)?
            .with_store(store)
            .for_device(cfg.device_id);
        let (status, _) = watch::channel(SyncStatus::Idle { last_sync });
        let (notices, _) = broadcast::channel(16);
        Ok(Some(Engine {
            vault,
            client,
            device: cfg.device_id,
            step: Step::Resume,
            want_account: true,
            want_sync: true,
            resumed: false,
            halt: None,
            backoff: BACKOFF_MIN,
            done: 0,
            total: None,
            last_sync,
            using_fallback,
            keychain_locked: false,
            skipped: 0,
            held: HashSet::new(),
            push_cap: PUSH_ITEMS,
            not_before: None,
            status,
            notices,
            gate: Arc::new(tokio::sync::Mutex::new(())),
            waiters: Vec::new(),
            cycle_waiters: Vec::new(),
            crash_before_ack: false,
        }))
    }

    /// The engine's client (shares its session).
    #[cfg_attr(not(feature = "test-util"), allow(dead_code))]
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Status updates.
    pub fn status(&self) -> watch::Receiver<SyncStatus> {
        self.status.subscribe()
    }

    /// Tells the UI that records changed (at most once per step).
    fn records_changed(&self) {
        let _ = self.notices.send(SyncNotice::RecordsChanged);
    }

    /// Asks for a full cycle (account and records), as "Sync now" does; also
    /// retries after a certificate or sign-in error.
    pub fn request_sync(&mut self) {
        if matches!(self.halt, Some(Halt::CertificateChanged | Halt::SignIn)) {
            self.halt = None;
        }
        self.keychain_locked = false;
        self.want_account = true;
        self.want_sync = true;
        self.not_before = None;
        self.held.clear();
    }

    /// The event socket (re)connected: catch up on records and on the
    /// account (an `account_changed` may have been missed while it was down).
    pub fn on_connected(&mut self) {
        self.want_account = true;
        self.want_sync = true;
    }

    /// The event socket dropped or could not connect: check the server with
    /// a sync cycle.
    fn on_socket_lost(&mut self) {
        self.want_sync = true;
    }

    /// Test-only: the next push is sent but its acks are not recorded, and
    /// the engine stops (a crash right after the server accepted).
    #[cfg(feature = "test-util")]
    pub fn crash_before_next_ack(&mut self) {
        self.crash_before_ack = true;
    }

    /// Feeds a WebSocket event in.
    pub fn on_event(&mut self, event: Event) {
        match event {
            Event::Changed { .. } => self.want_sync = true,
            Event::AccountChanged { .. } => self.want_account = true,
            Event::Revoked => {
                self.halt = Some(Halt::Revoked);
                self.set_status(SyncStatus::error(&SyncError::Revoked));
            }
        }
    }

    /// Ends a pause for a locked credential store and syncs (the app came
    /// to the front, or the periodic retry). Otherwise nothing.
    fn end_keychain_pause(&mut self) {
        if self.keychain_locked {
            tracing::info!("retrying sync after the credential store was locked");
            self.keychain_locked = false;
            self.want_account = true;
            self.want_sync = true;
            self.not_before = None;
        }
    }

    /// Whether the event socket should be open.
    fn wants_socket(&self) -> bool {
        self.resumed && self.halt.is_none() && !self.keychain_locked
    }

    fn set_status(&self, s: SyncStatus) {
        self.status.send_replace(s);
    }

    fn progress(&self) {
        self.set_status(SyncStatus::Syncing {
            done: self.done,
            total: self.total,
        });
    }

    /// One unit of work; see the module docs.
    pub async fn tick(&mut self) -> TickOutcome {
        let result = match self.step {
            Step::Resume => self.resume().await,
            Step::Idle => Ok(self.idle()),
            Step::Account => self.account().await,
            Step::Pull => self.pull().await,
            Step::Apply => self.apply().await,
            Step::Push => self.push().await,
            Step::Repair => self.repair().await,
        };
        match result {
            Ok(outcome) => outcome,
            Err(e) => self.fail(e).await,
        }
    }

    async fn resume(&mut self) -> Result<TickOutcome, SyncError> {
        if !self.client.resume(self.device).await? {
            return Err(SyncError::Unauthorized);
        }
        self.resumed = true;
        self.step = Step::Idle;
        Ok(TickOutcome::Progress)
    }

    fn idle(&mut self) -> TickOutcome {
        match self.halt {
            Some(Halt::Revoked) => {
                self.answer_all();
                return TickOutcome::Stopped;
            }
            Some(_) => {
                self.answer_all();
                return TickOutcome::Idle;
            }
            None => {}
        }
        if self.keychain_locked {
            // Until a wake-up says the device may be unlocked.
            self.answer_all();
            return TickOutcome::Idle;
        }
        if !self.resumed {
            self.step = Step::Resume;
            return TickOutcome::Progress;
        }
        if !(self.want_account || self.want_sync || !self.waiters.is_empty()) {
            return TickOutcome::Idle;
        }
        if let Some(at) = self.not_before {
            let now = Instant::now();
            if at > now && self.waiters.is_empty() {
                // Backing off after a failure; only "Sync now" overrides.
                return TickOutcome::Backoff(at - now);
            }
        }
        // A cycle starts: it answers everyone who asked before now, and
        // serves every wake-up so far; one that arrives while it runs
        // stays set for the next cycle, however this one ends.
        self.cycle_waiters.append(&mut self.waiters);
        let account = std::mem::take(&mut self.want_account);
        self.want_sync = false;
        self.done = 0;
        self.total = None;
        self.skipped = 0;
        self.push_cap = PUSH_ITEMS;
        self.progress();
        self.step = if account { Step::Account } else { Step::Pull };
        TickOutcome::Progress
    }

    async fn account(&mut self) -> Result<TickOutcome, SyncError> {
        let gate = self.gate.clone();
        let _held = gate.lock().await;
        let resp = self.client.account().await?;
        let (was, now) = with_vault(&self.vault, move |v| {
            let was = v.is_unlocked();
            v.install_account_material(&resp.material, resp.account_rev)?;
            Ok((was, v.is_unlocked()))
        })
        .await?;
        if was && !now {
            let _ = self.notices.send(SyncNotice::PasswordChangedElsewhere);
        }
        self.step = Step::Pull;
        Ok(TickOutcome::Progress)
    }

    /// Starts a full resync: revs forgotten (dirty flags kept), queue
    /// emptied, cursor back to 0.
    async fn begin_resync(&mut self) -> Result<TickOutcome, SyncError> {
        tracing::info!("full resync");
        with_vault(&self.vault, |v| {
            Ok(v.begin_resync(
                &[(keys::CURSOR, b"0"), (keys::RESYNC, b"1")],
                &[keys::RESYNC_EPOCH],
            )?)
        })
        .await?;
        self.step = Step::Pull;
        Ok(TickOutcome::Progress)
    }

    async fn pull(&mut self) -> Result<TickOutcome, SyncError> {
        self.want_sync = false;
        let c = with_vault(&self.vault, |v| {
            Ok(Cursor {
                cursor: state::get_i64(v, keys::CURSOR)?.unwrap_or(0),
                epoch: state::get_text(v, keys::EPOCH)?,
                resync: v.sync_get(keys::RESYNC)?.is_some(),
                resync_epoch: state::get_text(v, keys::RESYNC_EPOCH)?,
                kinds_version: state::get_i64(v, keys::KINDS_VERSION)?.unwrap_or(0),
            })
        })
        .await?;
        if !c.resync && c.kinds_version < i64::from(KIND_SET_VERSION) {
            // A newer build: rows of kinds the old one skipped come again.
            return self.begin_resync().await;
        }
        let page = match self.client.changes(c.cursor, PAGE).await {
            Err(SyncError::Gone) => return self.begin_resync().await,
            other => other?,
        };
        let expected = if c.resync { &c.resync_epoch } else { &c.epoch };
        if expected.as_ref().is_some_and(|e| *e != page.epoch) || page.next < c.cursor {
            // Another database (restored from a backup) or a rewound log.
            return self.begin_resync().await;
        }
        let total = page.changes.len();
        let rows: Vec<InboundRow> = page
            .changes
            .into_iter()
            .filter_map(|r| inbound(r).ok())
            .collect();
        let n = rows.len();
        if n < total {
            // A row this build cannot read must not stop sync for good: it
            // is skipped (the cursor moves past it) and reported.
            tracing::warn!(
                count = total - n,
                "skipping unreadable rows from the server"
            );
            self.skipped += total - n;
        }
        let (next, epoch, resync) = (page.next, page.epoch, c.resync);
        let first_epoch = expected.is_none();
        with_vault(&self.vault, move |v| {
            v.queue_inbound(rows)?;
            state::put_i64(v, keys::CURSOR, next)?;
            if first_epoch {
                let k = if resync {
                    keys::RESYNC_EPOCH
                } else {
                    keys::EPOCH
                };
                v.sync_put(k, epoch.as_bytes())?;
            }
            Ok(())
        })
        .await?;
        self.done += n;
        self.progress();
        if !page.more {
            self.step = Step::Apply;
        }
        Ok(TickOutcome::Progress)
    }

    async fn apply(&mut self) -> Result<TickOutcome, SyncError> {
        let result = with_vault(&self.vault, |v| {
            let report = v.apply_inbound()?;
            if v.sync_get(keys::RESYNC)?.is_some() {
                // The pull is complete: rows still without a revision are
                // ones the server lacks.
                let epoch = v.sync_get(keys::RESYNC_EPOCH)?.unwrap_or_default();
                let kinds = KIND_SET_VERSION.to_string();
                v.finish_resync(
                    &[
                        (keys::EPOCH, &epoch),
                        (keys::KINDS_VERSION, kinds.as_bytes()),
                    ],
                    &[keys::RESYNC, keys::RESYNC_EPOCH],
                )?;
            }
            Ok((report.touched(), v.outbox_len()?))
        })
        .await;
        let total = match result {
            Ok((touched, total)) => {
                if touched {
                    self.records_changed();
                }
                total
            }
            Err(e) => {
                // Batches applied before the failure stay applied.
                if !matches!(e, SyncError::Locked) {
                    self.records_changed();
                }
                return Err(e);
            }
        };
        self.done = 0;
        self.total = Some(total);
        self.progress();
        self.step = Step::Push;
        Ok(TickOutcome::Progress)
    }

    async fn push(&mut self) -> Result<TickOutcome, SyncError> {
        // Fetch enough that held-back rows cannot crowd out the rest.
        let limit = PUSH_ITEMS + self.held.len();
        let rows = with_vault(&self.vault, move |v| Ok(v.outbox(limit)?)).await?;
        let mut budget = PUSH_BYTES;
        let mut batch: Vec<RecordRow> = Vec::new();
        for row in rows {
            if row.envelope.len() > MAX_ENVELOPE {
                if self.held.insert(row.id) {
                    tracing::warn!(id = %row.id, "record too large to sync; held back");
                }
                continue;
            }
            if self.held.contains(&row.id) {
                continue;
            }
            let size = push_size(&row);
            if batch.len() >= self.push_cap || (!batch.is_empty() && size > budget) {
                break;
            }
            budget = budget.saturating_sub(size);
            batch.push(row);
        }
        if batch.is_empty() {
            self.step = Step::Repair;
            return Ok(TickOutcome::Progress);
        }
        let req = PushReq {
            items: batch
                .iter()
                .map(|r| PushItem {
                    row: wire(r),
                    base_rev: r.rev,
                })
                .collect(),
        };
        let resp = match self.client.push(&req).await {
            Err(SyncError::Gone) => return self.begin_resync().await,
            Err(SyncError::Server {
                code: ErrorCode::BadRequest | ErrorCode::PayloadTooLarge,
                ..
            }) => {
                // The server refused the whole request because of some item:
                // narrow it down by halving, then hold back the culprit.
                if let [only] = batch.as_slice() {
                    tracing::warn!(id = %only.id, "server refuses record; held back");
                    self.held.insert(only.id);
                    self.push_cap = PUSH_ITEMS;
                } else {
                    self.push_cap = batch.len() / 2;
                }
                return Ok(TickOutcome::Progress);
            }
            other => other?,
        };
        if resp.results.len() != batch.len() {
            return Err(SyncError::Protocol(
                "the server answered a different number of rows".into(),
            ));
        }
        if self.crash_before_ack {
            self.crash_before_ack = false;
            return Ok(TickOutcome::Stopped);
        }
        let n = batch.len();
        let acks: Vec<(Id, Hlc, PushResult)> = batch
            .iter()
            .zip(resp.results)
            .map(|(r, res)| (r.id, r.hlc, res))
            .collect();
        let (unreadable, touched) = with_vault(&self.vault, move |v| {
            let mut unreadable = Vec::new();
            let mut touched = false;
            for (id, hlc, result) in acks {
                match result {
                    PushResult::Ok { rev, .. } => v.ack_pushed(id, hlc, rev)?,
                    PushResult::Conflict { current, .. } => match inbound(current) {
                        Ok(row) => {
                            touched |= v.apply_conflict(id, row)?.touched();
                        }
                        // The server's version cannot be read: keep ours
                        // unpushed and stop retrying it this run.
                        Err(_) => unreadable.push(id),
                    },
                }
            }
            // Owners this push found restored get their cascade back, now
            // that every result is applied (R33).
            touched |= v.revive_restored_owners()? > 0;
            Ok((unreadable, touched))
        })
        .await?;
        if touched {
            self.records_changed();
        }
        self.skipped += unreadable.len();
        self.held.extend(unreadable);
        self.done += n;
        self.progress();
        Ok(TickOutcome::Progress)
    }

    async fn repair(&mut self) -> Result<TickOutcome, SyncError> {
        let moved = with_vault(&self.vault, |v| {
            if v.inbound_len()? > 0 || v.outbox_len()? > 0 {
                return Ok(0);
            }
            Ok(v.repair()?)
        })
        .await?;
        if moved > 0 {
            // Repaired records (a group moved out of a cycle) show.
            self.records_changed();
            self.step = Step::Push;
            return Ok(TickOutcome::Progress);
        }
        let last_sync = now_ms();
        with_vault(&self.vault, move |v| {
            state::put_i64(v, keys::LAST_SYNC, last_sync)
        })
        .await?;
        self.last_sync = last_sync;
        self.backoff = BACKOFF_MIN;
        self.not_before = None;
        Ok(self.finish(self.cycle_status()))
    }

    /// How a completed cycle ends: idle, or a partial sync naming what was
    /// skipped.
    fn cycle_status(&self) -> SyncStatus {
        let mut parts = Vec::new();
        if self.skipped > 0 {
            parts.push(format!(
                "{} record(s) from the server could not be read",
                self.skipped
            ));
        }
        if !self.held.is_empty() {
            parts.push(format!(
                "{} record(s) could not be uploaded (too large or refused by the server)",
                self.held.len()
            ));
        }
        if parts.is_empty() {
            SyncStatus::Idle {
                last_sync: self.last_sync,
            }
        } else {
            SyncStatus::Error {
                kind: SyncErrorKind::Partial,
                message: parts.join("; "),
            }
        }
    }

    /// Ends the cycle with `status` and answers its waiters.
    fn finish(&mut self, status: SyncStatus) -> TickOutcome {
        self.set_status(status.clone());
        for w in self.cycle_waiters.drain(..) {
            let _ = w.send(status.clone());
        }
        self.step = Step::Idle;
        // A request that came in during this cycle is served by the next.
        if self.want_sync || self.want_account || !self.waiters.is_empty() {
            TickOutcome::Progress
        } else {
            TickOutcome::Idle
        }
    }

    /// Answers every waiter (pending or in a cycle) with the current status.
    fn answer_all(&mut self) {
        let status = self.status.borrow().clone();
        for w in self.cycle_waiters.drain(..).chain(self.waiters.drain(..)) {
            let _ = w.send(status.clone());
        }
    }

    /// Ends the cycle on `e`. Wake-ups set now arrived during the cycle
    /// (it cleared them when it started) and are kept: on a locked device
    /// every cycle ends here at apply, and an `account_changed` heard
    /// mid-cycle must still be fetched.
    async fn fail(&mut self, e: SyncError) -> TickOutcome {
        match e {
            // The vault locked mid-cycle: pulled rows stay queued.
            SyncError::Locked => self.finish(SyncStatus::Idle {
                last_sync: self.last_sync,
            }),
            // The device is locked (iOS keychain): pause, never fall back.
            // Rotated credentials a save missed stay in the client's memory
            // and are saved first once the store answers again.
            SyncError::StorageLocked => {
                if !self.keychain_locked {
                    tracing::info!("credential store locked with the device; sync paused");
                }
                self.keychain_locked = true;
                self.finish(SyncStatus::Idle {
                    last_sync: self.last_sync,
                })
            }
            // Only with a session in hand: a keychain that cannot even be
            // read at start must not switch to an empty fallback.
            SyncError::Storage(_) if !self.using_fallback && self.client.has_credentials() => {
                // The keychain refused the rotated credentials: keep them in
                // the vault database instead, then carry on.
                match fallback::switch_to_fallback(&self.client, &self.vault).await {
                    Ok(()) => {
                        self.using_fallback = true;
                        self.want_account = true;
                        self.want_sync = true;
                        self.step = Step::Idle;
                        self.finish_quiet()
                    }
                    Err(e) => self.stop_with(e, None),
                }
            }
            SyncError::Unreachable(reason) => self.retry_later(SyncStatus::Offline { reason }),
            SyncError::Revoked => {
                self.stop_with(SyncError::Revoked, Some(Halt::Revoked));
                TickOutcome::Stopped
            }
            SyncError::CertificateChanged => self.stop_with(
                SyncError::CertificateChanged,
                Some(Halt::CertificateChanged),
            ),
            SyncError::Unauthorized => self.stop_with(SyncError::Unauthorized, Some(Halt::SignIn)),
            other => {
                tracing::warn!(error = %other, "sync failed; retrying later");
                self.retry_later(SyncStatus::error(&other))
            }
        }
    }

    /// Ends the cycle with `status` and schedules a full retry (account
    /// included: a change may have been missed) after a growing, jittered
    /// delay.
    fn retry_later(&mut self, status: SyncStatus) -> TickOutcome {
        self.finish(status);
        self.want_account = true;
        self.want_sync = true;
        let delay = jittered(self.backoff);
        self.backoff = (self.backoff * 2).min(BACKOFF_MAX);
        self.not_before = Some(Instant::now() + delay);
        TickOutcome::Backoff(delay)
    }

    /// Ends the cycle on error `e`, optionally halting automatic retries.
    fn stop_with(&mut self, e: SyncError, halt: Option<Halt>) -> TickOutcome {
        tracing::warn!(error = %e, "sync stopped");
        if halt.is_some() {
            self.halt = halt;
        }
        let out = self.finish(SyncStatus::error(&e));
        if halt.is_some() {
            self.answer_all();
        }
        out
    }

    /// Restarts the cycle without answering anyone (after recovering from
    /// a store failure).
    fn finish_quiet(&mut self) -> TickOutcome {
        self.step = Step::Idle;
        TickOutcome::Progress
    }
}

/// A running engine. Dropping it stops the engine.
pub struct SyncHandle {
    cmds: mpsc::UnboundedSender<Command>,
    status: watch::Receiver<SyncStatus>,
    notices: broadcast::Sender<SyncNotice>,
    client: Client,
    gate: Arc<tokio::sync::Mutex<()>>,
    task: JoinHandle<()>,
}

enum Command {
    SyncNow(Option<oneshot::Sender<SyncStatus>>),
    Unlocked,
    Foreground,
    LocalChange,
    Stop,
}

/// Starts syncing `vault` on the current tokio runtime, or returns `None` if
/// sync is not enabled for it. `tokens` is the primary credential store (the
/// OS keychain in the app). A saved configuration that cannot be used (a
/// damaged `sync_state`, an invalid URL) is logged and also gives `None`.
pub fn start(vault: SharedVault, tokens: Arc<dyn TokenStore>) -> Option<SyncHandle> {
    let engine = match Engine::new(vault, tokens) {
        Ok(Some(engine)) => engine,
        Ok(None) => return None,
        Err(e) => {
            tracing::warn!(error = %e, "sync configuration unusable");
            return None;
        }
    };
    let (tx, rx) = mpsc::unbounded_channel();
    let handle_status = engine.status();
    let notices = engine.notices.clone();
    let client = engine.client.clone();
    let gate = engine.gate.clone();
    let task = tokio::spawn(run(engine, rx));
    Some(SyncHandle {
        cmds: tx,
        status: handle_status,
        notices,
        client,
        gate,
        task,
    })
}

impl SyncHandle {
    /// Status updates (the current value is available at once).
    pub fn status(&self) -> watch::Receiver<SyncStatus> {
        self.status.clone()
    }

    /// One-off notices ([`SyncNotice`]) from now on.
    pub fn notices(&self) -> broadcast::Receiver<SyncNotice> {
        self.notices.subscribe()
    }

    /// Syncs now (account and records); also retries after a certificate or
    /// sign-in error.
    pub fn sync_now(&self) {
        let _ = self.cmds.send(Command::SyncNow(None));
    }

    /// Syncs now and waits for that sync to end; returns how it ended
    /// (idle, offline or an error).
    pub async fn synced(&self) -> SyncStatus {
        let (tx, rx) = oneshot::channel();
        if self.cmds.send(Command::SyncNow(Some(tx))).is_err() {
            return self.status.borrow().clone();
        }
        match rx.await {
            Ok(status) => status,
            Err(_) => self.status.borrow().clone(),
        }
    }

    /// The vault was unlocked: apply what was pulled while locked, push.
    pub fn notify_unlocked(&self) {
        let _ = self.cmds.send(Command::Unlocked);
    }

    /// The app came to the front: ends a pause for a credential store that
    /// was locked with the device (iOS), else does nothing.
    pub fn notify_foreground(&self) {
        let _ = self.cmds.send(Command::Foreground);
    }

    /// The user changed something: push it shortly.
    pub fn notify_local_change(&self) {
        let _ = self.cmds.send(Command::LocalChange);
    }

    /// The engine's client (one per device: it holds the session).
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Whether the engine has stopped (revoked, or [`SyncHandle::stop`]).
    pub fn is_finished(&self) -> bool {
        self.task.is_finished()
    }

    /// Serializes account-material changes between the flows and the engine.
    pub(crate) fn gate(&self) -> &Arc<tokio::sync::Mutex<()>> {
        &self.gate
    }

    /// Stops the engine and waits for it to finish its current step.
    pub async fn stop(mut self) {
        let _ = self.cmds.send(Command::Stop);
        let _ = (&mut self.task).await;
    }
}

impl Drop for SyncHandle {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// What the event socket task reports.
enum Signal {
    Connected,
    Event(Event),
    Failed(SyncError),
    /// An open socket ended.
    Lost,
}

/// Keeps the events WebSocket open, reconnecting with a growing, jittered
/// delay after every failure, and forwards what it hears. Polling the stream
/// continuously also answers the server's pings while the engine is busy.
/// Ends only when the device is revoked (or the engine drops the receiver);
/// the driver aborts it while sync is halted.
async fn socket_task(client: Client, tx: mpsc::UnboundedSender<Signal>) {
    let mut delay = BACKOFF_MIN;
    loop {
        match ws::open_stream(&client).await {
            Ok(stream) => {
                delay = BACKOFF_MIN;
                if tx.send(Signal::Connected).is_err() {
                    return;
                }
                let mut stream = std::pin::pin!(stream);
                while let Some(item) = stream.next().await {
                    let signal = match item {
                        Ok(event) => Signal::Event(event),
                        Err(e) => Signal::Failed(e),
                    };
                    if tx.send(signal).is_err() {
                        return;
                    }
                }
                // The server closed the socket (it stopped, or restarts).
                if tx.send(Signal::Lost).is_err() {
                    return;
                }
            }
            Err(SyncError::Revoked) => {
                let _ = tx.send(Signal::Failed(SyncError::Revoked));
                return;
            }
            Err(e) => {
                tracing::debug!(error = %e, "event socket unavailable");
                if tx.send(Signal::Failed(e)).is_err() {
                    return;
                }
            }
        }
        tokio::time::sleep(jittered(delay)).await;
        delay = (delay * 2).min(BACKOFF_MAX);
    }
}

async fn run(mut engine: Engine, mut cmds: mpsc::UnboundedReceiver<Command>) {
    let (sig_tx, mut sig_rx) = mpsc::unbounded_channel();
    let mut socket: Option<JoinHandle<()>> = None;
    let mut local_due: Option<Instant> = None;

    'run: loop {
        // Inputs that arrived while working.
        while let Ok(cmd) = cmds.try_recv() {
            if !handle_command(&mut engine, cmd, &mut local_due) {
                break 'run;
            }
        }
        while let Ok(sig) = sig_rx.try_recv() {
            handle_signal(&mut engine, sig);
        }
        match (&socket, engine.wants_socket()) {
            (None, true) => {
                socket = Some(tokio::spawn(socket_task(
                    engine.client.clone(),
                    sig_tx.clone(),
                )));
            }
            (Some(task), false) => {
                task.abort();
                socket = None;
            }
            (Some(task), true) if task.is_finished() => socket = None,
            _ => {}
        }

        let wake = match engine.tick().await {
            TickOutcome::Progress => continue,
            TickOutcome::Stopped => break,
            TickOutcome::Idle => Instant::now() + PERIODIC,
            TickOutcome::Backoff(d) => Instant::now() + d,
        };

        // Wait for a trigger.
        let deadline = local_due.map_or(wake, |due| due.min(wake));
        tokio::select! {
            cmd = cmds.recv() => match cmd {
                Some(cmd) => {
                    if !handle_command(&mut engine, cmd, &mut local_due) {
                        break;
                    }
                }
                None => break,
            },
            Some(sig) = sig_rx.recv() => handle_signal(&mut engine, sig),
            () = tokio::time::sleep_until(deadline) => {
                if local_due.is_some_and(|due| due <= Instant::now()) {
                    local_due = None;
                } else {
                    // The periodic sync also checks the account, in case
                    // an `account_changed` event was missed; it also retries
                    // after a pause for a locked device (the phone may be
                    // unlocked with Tether still in the background).
                    engine.want_account = true;
                    engine.end_keychain_pause();
                }
                engine.want_sync = true;
            }
        }
    }
    if let Some(task) = socket {
        task.abort();
    }
}

/// Applies a command; `false` means stop.
fn handle_command(engine: &mut Engine, cmd: Command, local_due: &mut Option<Instant>) -> bool {
    match cmd {
        Command::SyncNow(waiter) => {
            engine.request_sync();
            if let Some(w) = waiter {
                engine.waiters.push(w);
            }
        }
        Command::Unlocked => {
            engine.keychain_locked = false;
            engine.want_sync = true;
            engine.not_before = None;
        }
        Command::Foreground => engine.end_keychain_pause(),
        Command::LocalChange => {
            local_due.get_or_insert_with(|| Instant::now() + DEBOUNCE);
        }
        Command::Stop => return false,
    }
    true
}

fn handle_signal(engine: &mut Engine, sig: Signal) {
    match sig {
        Signal::Connected => engine.on_connected(),
        Signal::Event(event) => engine.on_event(event),
        Signal::Failed(SyncError::Revoked) => engine.on_event(Event::Revoked),
        // The socket's refresh found the device locked: pause (this also
        // closes the socket).
        Signal::Failed(SyncError::StorageLocked) => engine.keychain_locked = true,
        // Let the next request confirm and halt.
        Signal::Failed(SyncError::CertificateChanged | SyncError::Unauthorized) => {
            engine.want_sync = true;
        }
        // Anything else: the socket task backs off and the next connection
        // triggers the catch-up. Meanwhile a request finds out whether the
        // server is gone, so the status turns Offline now rather than at the
        // next edit or poll (a backoff already running still holds it).
        Signal::Failed(_) | Signal::Lost => engine.on_socket_lost(),
    }
}
