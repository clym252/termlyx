//! Deterministic multi-client convergence simulation (spec "Testing").
//!
//! Each seed runs 3 to 5 real `VaultService`s, each with its own sync
//! [`Engine`], against one in-process TLS server reached through a TCP proxy
//! (its address survives server restarts, which move the server to a new
//! port). One seeded `ChaCha8Rng` picks every action, and the harness ticks
//! the engines one step at a time: no driver task, no WebSocket, changes
//! travel by pull. The actions:
//!
//! * local edits through the same `VaultService` calls the app makes (every
//!   delete and every host/group save followed by `tidy_own_logins`): hosts,
//!   own logins with passwords, forward rules, groups, group moves, Keychain
//!   identities with passwords, known hosts;
//! * races: two devices that have not synced change the same thing (a host
//!   with its own login and forward rules deleted on one and edited on the
//!   other, two groups moved under each other, one identity's password
//!   changed on both, the same known-host key trusted on both, a forward
//!   rule deleted on one while its host is edited on the other, and a host
//!   delete whose stashed forward rule is edited elsewhere and then deleted
//!   on the deleting device before its host delete loses, and a host delete
//!   that loses to an edit pulled together with a newer version of its
//!   stashed rule or own login);
//! * full syncs, partial syncs (a few ticks, leaving a cycle half done),
//!   partitions (a client is not ticked for k steps but keeps editing),
//!   lock and unlock (a locked client still pulls into its queue), client
//!   restarts from the vault file (mid-cycle; it comes back locked) and
//!   server restarts.
//!
//! Then everyone heals (online, unlocked) and syncs round-robin until a whole
//! round is quiet: nobody started it with unpushed or queued rows and nobody's
//! records changed. Not quiet within [`MAX_ROUNDS`] rounds is a convergence
//! failure. Then: (a) every client decrypts the same records and conflicts;
//! (b) no group cycle; (c) no lost version (below); (d) no dirty or queued
//! rows, every engine Idle. Also checked along the way: records never change
//! while a vault is locked (or across a restart), the only records sync
//! writes by itself are the ones the merge rules allow (a group moved to the
//! top level by cycle repair, an own login adopted into the Keychain, a
//! forward rule or own login that rule 6 keeps, re-stamped, instead of
//! applying its tombstone), a dependent revived from the cascade stash never
//! turns a newer remote version into a conflict in the same apply (check (c)
//! would count that version as kept), every own login a host or group links still
//! exists, and every forward rule a host lists still exists (unless the user
//! deleted it after its last version: a stale list entry then reads as
//! missing).
//!
//! ## No lost versions
//!
//! *Write events*: every record a harness action changes (the device's
//! decrypted records before and after the action, cascades included), and
//! every record a sync tick changes that is left unpushed afterwards — a write
//! by sync itself, since a pulled row that is taken is clean. Each event has
//! the writing device's content of the record just before (`pre`) and after
//! (`post`, "deleted" for a deletion).
//!
//! *Versions*: every live `post`, and every deletion the user asked for (the
//! record a delete action names; not its cascades — a host's forward rules,
//! own logins removed by `tidy_own_logins`, a known host's duplicates — which
//! merge rule 6 deliberately keeps alive for an owner an edit restored).
//! Content is compared by value, passwords included, so two writes of the
//! same content are one version (merge rule 4 treats them alike too: "two
//! devices made the same change").
//!
//! After the final sync every version V of record R must be one of:
//! 1. current: R on every client is V (absent, for a deletion);
//! 2. kept: a `Conflict` about R holds V as its losing side. A live version
//!    is matched by value; a deletion by its own tombstone, through the
//!    conflict's id (`conflict_id(R, tombstone HLC)`), or, when its tombstone
//!    was replaced on its device before being pushed (delete vs delete
//!    merges concurrent deletions into one), through the tombstone of a
//!    concurrent deletion of R: one written after the version V deleted and
//!    before V's tombstone was replaced (the harness reads the row's HLC and
//!    dirty flag from the file after every tick);
//! 3. superseded: another write event W of R, on any device, whose writer
//!    held exactly V just before writing (it saw V and changed it), where W
//!    itself passes (transitively). A cascaded deletion W passes like a
//!    deletion (1 or 2), or when its cause stands: the owner as finally
//!    merged no longer links R (a forward rule's host, an own login's
//!    owner), or the known host whose duplicate R was is deleted;
//! 4. repaired: a write by sync of R whose result is V with the one field
//!    that repair clears set to null (`parent_id` of a group cycle repair,
//!    `owner` of an own-login adoption).
//!
//! A failing seed prints its number and the tail of the action log; rerun it
//! alone with `TETHER_SIM_SEED=<n>`. `TETHER_SIM_SEEDS=<count>` runs seeds
//! `0..count` (default 50), `TETHER_SIM_STEPS` changes the actions per seed,
//! `TETHER_SIM_TRACE=1` also logs every record change each tick makes.
//!
//! Coverage: over 20 or more seeds the run must have produced conflicts,
//! deletion conflicts, cycle repairs, own-login adoptions, rule-6 keeps (a
//! dependent re-stamped against a tombstone: same content, newly unpushed),
//! cascade revivals (a sync write from absent to live), duplicate known
//! hosts, races, locks, and client and server restarts.
//!
//! Determinism: every choice comes from the seed, and so do the ids of the
//! records the harness creates (own-login ids come from the vault, but picks
//! go by creation order, never by id). Seeds run in parallel, each on its own
//! thread and runtime, each internally sequential. The harness keeps the wall
//! clock moving between writes (each action at least 1 ms after the previous
//! write or tick) so that HLC order is action order. The HLC reads the
//! system clock (there is no injection point), so all clients share one
//! clock: no skew is simulated.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::ops::AddAssign;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use common::{PW, USER, net, pw};
use core_api::vault::{
    ChildPolicy, ConflictSide, Entity, MemoryKeyStore, PasswordChange, SharedVault, VaultError,
    VaultService,
};
use core_model::{
    ConnSettings, ForwardKind, ForwardRule, Group, Hlc, Host, Id, Identity, Kind, KnownHost,
    KnownHostSource,
};
use core_store::StoreError;
use core_sync::flows::{self, RecoveryChoice};
use core_sync::{Engine, MemoryTokenStore, SyncStatus, TickOutcome, parse_join_code};
use rand_chacha::ChaCha8Rng;
use rand_chacha::rand_core::{RngCore, SeedableRng};
use secrecy::{ExposeSecret, SecretString};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tether_server::testing::{TestServer, spawn_tls};

/// Seeds in a normal run.
const DEFAULT_SEEDS: usize = 50;
/// Actions per seed.
const DEFAULT_STEPS: usize = 150;
/// Seeds run at once (each on its own thread and runtime).
const PARALLEL: usize = 8;
/// Round-robin sync rounds the final heal may take.
const MAX_ROUNDS: usize = 12;
/// Ticks one sync cycle may take.
const MAX_TICKS: usize = 300;
/// One seed may take this long.
const SEED_TIMEOUT: Duration = Duration::from_secs(180);
/// Log lines printed for a failing seed.
const LOG_TAIL: usize = 120;
/// Distinct known-host keys, so that devices trust the same key on their own.
const KNOWN_KEYS: u64 = 3;

#[test]
fn simulation_converges() {
    let env = |k: &str| std::env::var(k).ok().and_then(|v| v.parse::<usize>().ok());
    let seeds: Vec<u64> = match env("TETHER_SIM_SEED") {
        Some(seed) => vec![seed as u64],
        None => (0..env("TETHER_SIM_SEEDS").unwrap_or(DEFAULT_SEEDS) as u64).collect(),
    };
    let steps = env("TETHER_SIM_STEPS").unwrap_or(DEFAULT_STEPS);
    let started = Instant::now();
    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<SeedResult>> = Mutex::new(Vec::new());
    std::thread::scope(|s| {
        for _ in 0..PARALLEL.min(seeds.len()) {
            s.spawn(|| {
                loop {
                    let Some(&seed) = seeds.get(next.fetch_add(1, Ordering::SeqCst)) else {
                        return;
                    };
                    let log = Arc::new(Mutex::new(Vec::new()));
                    let result = run_seed(seed, steps, log.clone());
                    let log = std::mem::take(&mut *log.lock().unwrap_or_else(|p| p.into_inner()));
                    let log = if result.is_ok() { Vec::new() } else { log };
                    results.lock().unwrap().push((seed, result, log));
                }
            });
        }
    });

    let mut results = results.into_inner().unwrap();
    results.sort_by_key(|r| r.0);
    let mut total = Stats::default();
    let mut failures = Vec::new();
    for (seed, result, log) in results {
        match result {
            Ok(stats) => total += stats,
            Err(msg) => {
                let tail = &log[log.len().saturating_sub(LOG_TAIL)..];
                failures.push(format!(
                    "seed {seed} FAILED: {msg}\n  reproduce: TETHER_SIM_SEED={seed} cargo test -p core-sync --test simulation\n  last {} actions:\n    {}",
                    tail.len(),
                    tail.join("\n    ")
                ));
            }
        }
    }
    eprintln!(
        "simulation: {} seed(s), {steps} steps each, {:.1?}: {total:?}",
        seeds.len(),
        started.elapsed()
    );
    if !failures.is_empty() {
        for f in &failures {
            eprintln!("{f}\n");
        }
        panic!(
            "{} of {} seed(s) failed (details above): {:?}",
            failures.len(),
            seeds.len(),
            failures
                .iter()
                .map(|f| f.split_whitespace().nth(1).unwrap_or("?"))
                .collect::<Vec<_>>()
        );
    }
    // The run must actually exercise the tricky paths, or it proves little.
    if seeds.len() >= 20 {
        for (what, n) in [
            ("conflicts", total.conflicts),
            ("edit-beats-delete conflicts", total.deletion_conflicts),
            ("group cycle repairs", total.repairs),
            ("duplicate known hosts", total.known_dupes),
            ("client restarts", total.client_restarts),
            ("server restarts", total.server_restarts),
            ("locks", total.locks),
            ("races", total.races),
            ("adoptions", total.adoptions),
            ("rule-6 keeps", total.kept_dependents),
            ("cascade revivals", total.revives),
        ] {
            assert!(n > 0, "no seed produced any {what}: {total:?}");
        }
    }
}

/// A seed, how it ended, and its action log (kept only if it failed).
type SeedResult = (u64, Result<Stats, String>, Vec<String>);

/// Runs one seed on its own runtime (dropping it stops that seed's server).
fn run_seed(seed: u64, steps: usize, log: Arc<Mutex<Vec<String>>>) -> Result<Stats, String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
        rt.block_on(async {
            match tokio::time::timeout(SEED_TIMEOUT, run(seed, steps, log)).await {
                Ok(result) => result,
                Err(_) => Err(format!("timed out after {SEED_TIMEOUT:?}")),
            }
        })
    }));
    drop(rt);
    match outcome {
        Ok(result) => result,
        Err(panic) => Err(format!(
            "panicked: {}",
            panic
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| panic.downcast_ref::<&str>().map(|s| (*s).to_owned()))
                .unwrap_or_default()
        )),
    }
}

async fn run(seed: u64, steps: usize, log: Arc<Mutex<Vec<String>>>) -> Result<Stats, String> {
    let mut sim = Sim::setup(seed, log).await?;
    for step in 0..steps {
        sim.step = step;
        sim.act().await?;
    }
    sim.step = steps;
    sim.heal().await?;
    sim.check()?;
    Ok(sim.stats)
}

macro_rules! ensure {
    ($cond:expr, $($arg:tt)*) => {
        if !$cond {
            return Err(format!($($arg)*));
        }
    };
}

/// The record kinds the simulation writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum K {
    Group,
    Host,
    Identity,
    KnownHost,
    Rule,
}

impl K {
    fn of(kind: Kind) -> Option<K> {
        Some(match kind {
            Kind::Group => K::Group,
            Kind::Host => K::Host,
            Kind::Identity => K::Identity,
            Kind::KnownHost => K::KnownHost,
            Kind::ForwardRule => K::Rule,
            _ => return None,
        })
    }

    /// The field sync's own writes clear: cycle repair moves a group to the
    /// top level, adoption turns an own login into a Keychain identity.
    fn repair_field(self) -> Option<&'static str> {
        match self {
            K::Group => Some("parent_id"),
            K::Identity => Some("owner"),
            _ => None,
        }
    }
}

/// A record's content in comparable form: its JSON, passwords included.
fn canon<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value).expect("serialize").to_string()
}

fn canon_value(k: K, value: Value) -> Result<String, String> {
    fn via<T: Serialize + DeserializeOwned>(value: Value) -> Result<String, String> {
        serde_json::from_value::<T>(value)
            .map(|e| canon(&e))
            .map_err(|e| format!("a conflict's version does not decode: {e}"))
    }
    match k {
        K::Group => via::<Group>(value),
        K::Host => via::<Host>(value),
        K::Identity => via::<Identity>(value),
        K::KnownHost => via::<KnownHost>(value),
        K::Rule => via::<ForwardRule>(value),
    }
}

/// The id of the conflict about losing version `hlc` of `record`: the
/// derivation of `core-api`'s `conflict_id` (golden-tested there).
fn conflict_id(record: Id, hlc: Hlc) -> Id {
    let mut h = Sha256::new();
    h.update(b"tether/conflict-id/v1");
    h.update([0u8]);
    h.update(record.as_bytes());
    h.update(hlc.to_string().as_bytes());
    let digest = h.finalize();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    uuid::Builder::from_custom_bytes(bytes).into_uuid()
}

fn parse(content: &str) -> Value {
    serde_json::from_str(content).expect("canonical JSON")
}

/// `content` with `field` set to null.
fn cleared(k: K, content: &str, field: &str) -> Result<String, String> {
    let mut v = parse(content);
    v[field] = Value::Null;
    canon_value(k, v)
}

fn id_at(v: &Value, ptr: &str) -> Option<Id> {
    v.pointer(ptr)?.as_str()?.parse().ok()
}

fn read(v: &VaultService, k: K, id: Id) -> Result<Option<String>, String> {
    fn one<T: Entity>(v: &VaultService, id: Id) -> Result<Option<String>, VaultError> {
        Ok(v.get::<T>(id)?.map(|e| canon(&e)))
    }
    match k {
        K::Group => one::<Group>(v, id),
        K::Host => one::<Host>(v, id),
        K::Identity => one::<Identity>(v, id),
        K::KnownHost => one::<KnownHost>(v, id),
        K::Rule => one::<ForwardRule>(v, id),
    }
    .map_err(|e| format!("reading {k:?} {id}: {e}"))
}

/// A client's live records: id → content.
type Snap = BTreeMap<Id, String>;

/// Ids whose content differs between `a` and `b`.
fn changed(a: &Snap, b: &Snap) -> Vec<Id> {
    let ids: BTreeSet<Id> = a.keys().chain(b.keys()).copied().collect();
    ids.into_iter()
        .filter(|id| a.get(id) != b.get(id))
        .collect()
}

fn short(content: Option<&String>) -> String {
    match content {
        None => "<deleted>".into(),
        Some(c) if c.len() > 300 => format!("{}…", &c[..300]),
        Some(c) => c.clone(),
    }
}

/// A conflict record as compared across clients: the record it is about and
/// its losing version (`None`: a deletion).
#[derive(Debug, Clone, PartialEq, Eq)]
struct ConflictRec {
    record: Id,
    other: Option<String>,
}

/// One write event (see the module docs).
#[derive(Debug)]
struct Write {
    device: usize,
    step: usize,
    id: Id,
    kind: K,
    pre: Option<String>,
    post: Option<String>,
    /// Written by a sync tick, not by an action.
    by_sync: bool,
    /// A deletion the action asked for (not a cascade).
    user_delete: bool,
    /// For a deletion by an action (asked for or cascaded): the tombstone's
    /// HLC (a conflict about exactly this deletion has the id
    /// `conflict_id(id, hlc)`).
    tombstone: Option<Hlc>,
    /// For a cascaded deletion (not asked for): the record it depended on
    /// (a forward rule's host, an own login's owner, a known host's deleted
    /// twin).
    cascade_of: Option<Id>,
    /// A user deletion whose tombstone was replaced on its device before it
    /// was pushed (by another device's tombstone: delete vs delete merges
    /// them, or by an edit): the number of write events at that point.
    replaced_at: Option<usize>,
}

#[derive(Debug, Default, Clone, Copy)]
struct Stats {
    seeds: usize,
    clients: usize,
    ops: usize,
    refused: usize,
    races: usize,
    syncs: usize,
    ticks: usize,
    partitions: usize,
    locks: usize,
    client_restarts: usize,
    server_restarts: usize,
    backoffs: usize,
    versions: usize,
    conflicts: usize,
    deletion_conflicts: usize,
    repairs: usize,
    adoptions: usize,
    kept_dependents: usize,
    revives: usize,
    known_dupes: usize,
    rounds: usize,
}

impl AddAssign for Stats {
    fn add_assign(&mut self, o: Stats) {
        self.seeds += o.seeds;
        self.clients += o.clients;
        self.ops += o.ops;
        self.refused += o.refused;
        self.races += o.races;
        self.syncs += o.syncs;
        self.ticks += o.ticks;
        self.partitions += o.partitions;
        self.locks += o.locks;
        self.client_restarts += o.client_restarts;
        self.server_restarts += o.server_restarts;
        self.backoffs += o.backoffs;
        self.versions += o.versions;
        self.conflicts += o.conflicts;
        self.deletion_conflicts += o.deletion_conflicts;
        self.repairs += o.repairs;
        self.adoptions += o.adoptions;
        self.kept_dependents += o.kept_dependents;
        self.revives += o.revives;
        self.known_dupes += o.known_dupes;
        self.rounds += o.rounds;
    }
}

/// A user action with its targets chosen. Passwords are test values.
#[derive(Debug, Clone)]
enum Op {
    CreateHost {
        id: Id,
        group: Option<Id>,
        label: String,
    },
    EditHost {
        id: Id,
        label: String,
        group: Option<Option<Id>>,
    },
    DeleteHost {
        id: Id,
    },
    SetOwnLogin {
        owner: Id,
        username: String,
        password: String,
    },
    LinkIdentity {
        host: Id,
        identity: Id,
    },
    AddRule {
        id: Id,
        host: Id,
        port: u16,
    },
    EditRule {
        id: Id,
        port: u16,
    },
    DeleteRule {
        id: Id,
    },
    CreateGroup {
        id: Id,
        parent: Option<Id>,
        name: String,
    },
    RenameGroup {
        id: Id,
        name: String,
    },
    MoveGroup {
        id: Id,
        parent: Option<Id>,
    },
    DeleteGroup {
        id: Id,
    },
    CreateIdentity {
        id: Id,
        username: String,
        password: String,
    },
    EditIdentity {
        id: Id,
        password: String,
    },
    DeleteIdentity {
        id: Id,
    },
    AddKnownHost {
        id: Id,
        key: u64,
    },
    DeleteKnownHost {
        id: Id,
    },
}

/// What an action wrote besides plain edits.
#[derive(Default)]
struct Effect {
    created: Vec<(Id, K)>,
    deleted: Vec<Id>,
    /// `(login, owner)` from `set_own_login`.
    own_login: Option<(Id, Id)>,
}

/// Refusals the app shows the user; anything else fails the run.
fn refused(e: &VaultError) -> bool {
    matches!(
        e,
        VaultError::MissingRef(_)
            | VaultError::Referenced(_)
            | VaultError::Cycle
            | VaultError::ForeignOwnLogin(_)
            | VaultError::Store(StoreError::NotFound { .. })
    )
}

fn gone(id: Id) -> VaultError {
    VaultError::Store(StoreError::NotFound { id })
}

/// Runs `op` the way the app's commands do: deletes, host and group saves
/// and own-login changes are followed by `tidy_own_logins`.
fn perform(v: &mut VaultService, op: &Op) -> Result<Effect, VaultError> {
    let mut fx = Effect::default();
    let mut tidy = true;
    match op.clone() {
        Op::CreateHost { id, group, label } => {
            v.upsert(Host {
                id,
                group_id: group,
                label,
                address: "10.0.0.1".into(),
                tag_ids: vec![],
                settings: ConnSettings::default(),
                forward_rule_ids: vec![],
                extra: Default::default(),
            })?;
            fx.created.push((id, K::Host));
        }
        Op::EditHost { id, label, group } => {
            let mut h = v.get::<Host>(id)?.ok_or_else(|| gone(id))?;
            h.label = label;
            h.address = format!("10.0.{}.1", h.label.len());
            if let Some(g) = group {
                h.group_id = g;
            }
            v.upsert(h)?;
        }
        Op::DeleteHost { id } => {
            v.delete::<Host>(id)?;
            fx.deleted.push(id);
        }
        Op::SetOwnLogin {
            owner,
            username,
            password,
        } => {
            let login = v.set_own_login(
                owner,
                username,
                PasswordChange::Set(SecretString::from(password)),
            )?;
            fx.created.push((login.id, K::Identity));
            fx.own_login = Some((login.id, owner));
        }
        Op::LinkIdentity { host, identity } => {
            let mut h = v.get::<Host>(host)?.ok_or_else(|| gone(host))?;
            h.settings.identity_id = Some(identity);
            v.upsert(h)?;
        }
        Op::AddRule { id, host, port } => {
            v.upsert(ForwardRule {
                id,
                host_id: host,
                label: format!("fwd {port}"),
                kind: ForwardKind::Local,
                bind_addr: "127.0.0.1".into(),
                bind_port: port,
                dest_host: Some("db.internal".into()),
                dest_port: Some(5432),
                autostart: false,
                extra: Default::default(),
            })?;
            fx.created.push((id, K::Rule));
            tidy = false;
        }
        Op::EditRule { id, port } => {
            let mut r = v.get::<ForwardRule>(id)?.ok_or_else(|| gone(id))?;
            r.bind_port = port;
            r.label = format!("fwd {port}");
            v.upsert(r)?;
            tidy = false;
        }
        Op::DeleteRule { id } => {
            v.delete::<ForwardRule>(id)?;
            fx.deleted.push(id);
        }
        Op::CreateGroup { id, parent, name } => {
            v.upsert(Group {
                id,
                parent_id: parent,
                name,
                settings: ConnSettings::default(),
                extra: Default::default(),
            })?;
            fx.created.push((id, K::Group));
        }
        Op::RenameGroup { id, name } => {
            let mut g = v.get::<Group>(id)?.ok_or_else(|| gone(id))?;
            g.name = name;
            v.upsert(g)?;
        }
        Op::MoveGroup { id, parent } => {
            let mut g = v.get::<Group>(id)?.ok_or_else(|| gone(id))?;
            g.parent_id = parent;
            v.upsert(g)?;
        }
        Op::DeleteGroup { id } => {
            v.delete_group(id, ChildPolicy::MoveToParent)?;
            fx.deleted.push(id);
        }
        Op::CreateIdentity {
            id,
            username,
            password,
        } => {
            v.upsert(Identity {
                id,
                label: format!("{username} login"),
                username,
                password: Some(SecretString::from(password)),
                key_id: None,
                owner: None,
                extra: Default::default(),
            })?;
            fx.created.push((id, K::Identity));
            tidy = false;
        }
        Op::EditIdentity { id, password } => {
            let mut i = v.get::<Identity>(id)?.ok_or_else(|| gone(id))?;
            i.password = Some(SecretString::from(password));
            v.upsert(i)?;
            tidy = false;
        }
        Op::DeleteIdentity { id } => {
            v.delete::<Identity>(id)?;
            fx.deleted.push(id);
        }
        Op::AddKnownHost { id, key } => {
            v.upsert(KnownHost {
                id,
                host_pattern: format!("server{key}.example"),
                port: 22,
                algorithm: "ssh-ed25519".into(),
                public_key: format!("AAAAC3NzaC1lZDI1NTE5AAAAIKey{key}"),
                added_at: 1_700_000_000,
                source: KnownHostSource::Prompt,
                extra: Default::default(),
            })?;
            fx.created.push((id, K::KnownHost));
            tidy = false;
        }
        Op::DeleteKnownHost { id } => {
            v.delete::<KnownHost>(id)?;
            fx.deleted.push(id);
        }
    }
    if tidy {
        v.tidy_own_logins()?;
    }
    Ok(fx)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn open(path: &Path) -> Result<SharedVault, String> {
    let v = VaultService::open_for_tests(path, Box::new(MemoryKeyStore::default()))
        .map_err(|e| format!("opening {}: {e}", path.display()))?;
    Ok(Arc::new(Mutex::new(v)))
}

struct Client {
    path: PathBuf,
    vault: SharedVault,
    tokens: Arc<MemoryTokenStore>,
    engine: Option<Engine>,
    /// Not ticked before this step (partitioned).
    offline_until: usize,
    /// Its live records as last seen (kept current while unlocked; records
    /// cannot change while locked).
    cache: Snap,
    /// Its deletions not yet pushed: record → (write index, tombstone
    /// HLC). Watched after every tick until acknowledged, overtaken by an
    /// edit, or absorbed by another device's tombstone (delete vs delete).
    pending: BTreeMap<Id, (usize, Hlc)>,
    /// A read-only connection to its file, for what no API shows (a row's
    /// HLC and dirty flag).
    db: rusqlite::Connection,
}

impl Client {
    fn v(&self) -> MutexGuard<'_, VaultService> {
        self.vault.lock().unwrap()
    }

    fn unlocked(&self) -> bool {
        self.v().is_unlocked()
    }

    fn engine(&mut self) -> &mut Engine {
        self.engine.as_mut().expect("engine running")
    }
}

struct Sim {
    rng: ChaCha8Rng,
    step: usize,
    srv: Option<TestServer>,
    pin: [u8; 32],
    proxy: net::Proxy,
    _dir: tempfile::TempDir,
    clients: Vec<Client>,
    /// Every record id the simulation created, with its kind.
    kinds: BTreeMap<Id, K>,
    /// The same ids in creation order (picks are by position: deterministic).
    order: Vec<Id>,
    /// Own login → the host or group it was created for.
    own_login_of: BTreeMap<Id, Id>,
    writes: Vec<Write>,
    log: Arc<Mutex<Vec<String>>>,
    stats: Stats,
    last_write_ms: u64,
    counter: u64,
    /// When a client saw a record vanish through sync (it applied someone's
    /// deletion): (client, record) → write-event counts at those moments.
    saw_deleted: BTreeMap<(usize, Id), Vec<usize>>,
    /// `TETHER_SIM_TRACE=1`: log every record change a tick makes, pulled
    /// ones included (for debugging a seed).
    trace: bool,
}

impl Sim {
    async fn setup(seed: u64, log: Arc<Mutex<Vec<String>>>) -> Result<Sim, String> {
        let mut rng = ChaCha8Rng::seed_from_u64(seed);
        let n = 3 + (rng.next_u64() % 3) as usize;
        let srv = spawn_tls().await;
        let proxy = net::proxy(srv.addr()).await;
        let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let mut sim = Sim {
            rng,
            step: 0,
            pin: srv.pin,
            srv: Some(srv),
            proxy,
            _dir: dir,
            clients: Vec::new(),
            kinds: BTreeMap::new(),
            order: Vec::new(),
            own_login_of: BTreeMap::new(),
            writes: Vec::new(),
            log,
            stats: Stats {
                seeds: 1,
                clients: n,
                ..Stats::default()
            },
            last_write_ms: 0,
            counter: 0,
            saw_deleted: BTreeMap::new(),
            trace: std::env::var_os("TETHER_SIM_TRACE").is_some(),
        };
        sim.note(format!("seed {seed}: {n} clients"));

        // The first device creates the vault and turns sync on.
        let path = sim._dir.path().join("c0.db");
        let vault = open(&path)?;
        let recovery = vault
            .lock()
            .unwrap()
            .create(pw(PW))
            .map_err(|e| e.to_string())?;
        let tokens = Arc::new(MemoryTokenStore::new());
        let srv = sim.srv.as_ref().expect("server");
        let mut code = parse_join_code(&srv.make_invite().await).map_err(|e| e.to_string())?;
        code.url = sim.proxy.url.clone();
        flows::enable_with_invite(
            &vault,
            tokens.clone(),
            &code,
            USER,
            pw(PW),
            RecoveryChoice::Existing(SecretString::from(recovery)),
        )
        .await
        .map_err(|e| format!("enable: {e}"))?;
        sim.add_client(path, vault, tokens)?;
        sim.sync(0).await?;

        // The others join with add-device codes from the first.
        for k in 1..n {
            let link = sim.clients[0]
                .engine()
                .client()
                .create_link()
                .await
                .map_err(|e| format!("link code: {e}"))?;
            let mut code = parse_join_code(&link.code).map_err(|e| e.to_string())?;
            code.url = sim.proxy.url.clone();
            let path = sim._dir.path().join(format!("c{k}.db"));
            let vault = open(&path)?;
            let tokens = Arc::new(MemoryTokenStore::new());
            flows::join_with_code(&vault, tokens.clone(), &code, USER, pw(PW), false)
                .await
                .map_err(|e| format!("join: {e}"))?;
            sim.add_client(path, vault, tokens)?;
            sim.sync(k).await?;
        }
        Ok(sim)
    }

    fn add_client(
        &mut self,
        path: PathBuf,
        vault: SharedVault,
        tokens: Arc<MemoryTokenStore>,
    ) -> Result<(), String> {
        let engine = Engine::new(vault.clone(), tokens.clone())
            .map_err(|e| format!("engine: {e}"))?
            .ok_or("sync is off")?;
        let db = rusqlite::Connection::open_with_flags(
            &path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .map_err(|e| format!("reading {}: {e}", path.display()))?;
        self.clients.push(Client {
            db,
            path,
            vault,
            tokens,
            engine: Some(engine),
            offline_until: 0,
            cache: Snap::new(),
            pending: BTreeMap::new(),
        });
        Ok(())
    }

    // ---- randomness (all of it comes from the seed) ----

    fn below(&mut self, n: usize) -> usize {
        (self.rng.next_u64() % n as u64) as usize
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.rng.next_u64() % 100 < percent
    }

    fn pick<T: Copy>(&mut self, xs: &[T]) -> Option<T> {
        if xs.is_empty() {
            None
        } else {
            Some(xs[self.below(xs.len())])
        }
    }

    fn rid(&mut self) -> Id {
        let mut b = [0u8; 16];
        self.rng.fill_bytes(&mut b);
        uuid::Builder::from_random_bytes(b).into_uuid()
    }

    fn fresh(&mut self, prefix: &str) -> String {
        self.counter += 1;
        format!("{prefix}-{}", self.counter)
    }

    fn port(&mut self) -> u16 {
        1024 + self.below(30_000) as u16
    }

    fn note(&self, line: String) {
        self.log
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(format!("[{:>3}] {line}", self.step));
    }

    // ---- what a client has ----

    fn live(&self, i: usize, k: K) -> Vec<Id> {
        self.order
            .iter()
            .copied()
            .filter(|id| self.kinds[id] == k && self.clients[i].cache.contains_key(id))
            .collect()
    }

    fn json(&self, i: usize, id: Id) -> Value {
        self.clients[i]
            .cache
            .get(&id)
            .map_or(Value::Null, |c| parse(c))
    }

    /// Keychain identities (not an own login).
    fn keychain(&self, i: usize) -> Vec<Id> {
        self.live(i, K::Identity)
            .into_iter()
            .filter(|id| self.json(i, *id)["owner"].is_null())
            .collect()
    }

    fn rules_of(&self, i: usize, host: Id) -> Vec<Id> {
        self.live(i, K::Rule)
            .into_iter()
            .filter(|id| id_at(&self.json(i, *id), "/host_id") == Some(host))
            .collect()
    }

    /// Hosts with an own login and at least one forward rule.
    fn rich_hosts(&self, i: usize) -> Vec<Id> {
        self.live(i, K::Host)
            .into_iter()
            .filter(|h| {
                let login = id_at(&self.json(i, *h), "/settings/identity_id");
                login.is_some_and(|l| self.own_login_of.get(&l) == Some(h))
                    && !self.rules_of(i, *h).is_empty()
            })
            .collect()
    }

    fn unlocked_clients(&self) -> Vec<usize> {
        (0..self.clients.len())
            .filter(|&i| self.clients[i].unlocked())
            .collect()
    }

    fn online_clients(&self) -> Vec<usize> {
        (0..self.clients.len())
            .filter(|&i| self.clients[i].offline_until <= self.step)
            .collect()
    }

    fn status(&self, i: usize) -> SyncStatus {
        self.clients[i]
            .engine
            .as_ref()
            .expect("engine running")
            .status()
            .borrow()
            .clone()
    }

    fn snapshot(&self, i: usize) -> Result<Snap, String> {
        let v = self.clients[i].v();
        let mut snap = Snap::new();
        for (&id, &k) in &self.kinds {
            if let Some(content) = read(&v, k, id)? {
                snap.insert(id, content);
            }
        }
        Ok(snap)
    }

    fn conflicts(&self, i: usize) -> Result<BTreeMap<Id, ConflictRec>, String> {
        let v = self.clients[i].v();
        let mut out = BTreeMap::new();
        for view in v.list_conflicts().map_err(|e| format!("conflicts: {e}"))? {
            let k = K::of(view.kind).ok_or("a conflict about a kind never written")?;
            let other = match view.other {
                None => None,
                Some(mut value) => {
                    if k == K::Identity && value.get("password").is_some_and(|p| !p.is_null()) {
                        let secret = v
                            .reveal_conflict_field(view.id, ConflictSide::Other, "password")
                            .map_err(|e| format!("reveal: {e}"))?;
                        value["password"] = Value::String(secret.expose_secret().to_owned());
                    }
                    Some(canon_value(k, value)?)
                }
            };
            out.insert(
                view.id,
                ConflictRec {
                    record: view.record_id,
                    other,
                },
            );
        }
        Ok(out)
    }

    // ---- driving engines ----

    async fn tick(&mut self, i: usize) -> Result<TickOutcome, String> {
        let dirty_before = if self.clients[i].unlocked() {
            Some(self.dirty_ids(i)?)
        } else {
            None
        };
        let outcome = self.clients[i].engine().tick().await;
        self.stats.ticks += 1;
        if let Some(before) = dirty_before {
            self.after_tick(i, &before)?;
        }
        self.watch_tombstones(i)?;
        // Whatever the tick wrote, later actions sort after it.
        self.last_write_ms = self.last_write_ms.max(now_ms());
        if outcome == TickOutcome::Stopped {
            return Err(format!("client {i}: engine stopped: {:?}", self.status(i)));
        }
        Ok(outcome)
    }

    /// Records sync's own writes in the tick that just ran (see the module
    /// docs) and refreshes the cache.
    fn after_tick(&mut self, i: usize, dirty_before: &BTreeSet<Id>) -> Result<(), String> {
        if !self.clients[i].unlocked() {
            return Ok(());
        }
        let now = self.snapshot(i)?;
        let dirty = self.dirty_ids(i)?;
        // Merge rule 6 keeping a dependent against a tombstone: same
        // content, re-stamped, newly unpushed.
        for id in dirty.difference(dirty_before) {
            if matches!(self.kinds.get(id), Some(K::Rule | K::Identity))
                && now.contains_key(id)
                && now.get(id) == self.clients[i].cache.get(id)
            {
                self.stats.kept_dependents += 1;
                self.note(format!("c{i} sync kept {:?} {id}", self.kinds[id]));
            }
        }
        if now == self.clients[i].cache {
            return Ok(());
        }
        let before = std::mem::replace(&mut self.clients[i].cache, now);
        let now = &self.clients[i].cache;
        let mut writes = Vec::new();
        let mut seen_gone = Vec::new();
        for id in changed(&before, now) {
            if self.trace {
                self.note(format!(
                    "c{i} tick changed {:?} {id} (dirty after: {}): {} -> {}",
                    self.kinds[&id],
                    dirty.contains(&id),
                    short(before.get(&id)),
                    short(now.get(&id))
                ));
            }
            if !dirty.contains(&id) {
                if now.get(&id).is_none() {
                    seen_gone.push(id);
                }
                continue; // a pulled row, taken
            }
            let kind = self.kinds[&id];
            let post = now.get(&id).cloned();
            // Repair and adoption clear one field; merge rule 6 keeps (and
            // re-stamps) a forward rule or own login whose owner still links
            // it instead of applying its tombstone: a version that existed.
            let repaired = match (kind.repair_field(), &post) {
                (Some(field), Some(c)) => parse(c)[field].is_null(),
                _ => false,
            };
            let kept = matches!(kind, K::Rule | K::Identity)
                && post.is_some()
                && self.writes.iter().any(|w| w.id == id && w.post == post);
            let allowed = repaired || kept;
            ensure!(
                allowed,
                "client {i}: sync itself wrote {kind:?} {id}, which no merge rule does:\n  before {}\n  after  {}",
                short(before.get(&id)),
                short(post.as_ref())
            );
            writes.push(Write {
                device: i,
                step: self.step,
                id,
                kind,
                pre: before.get(&id).cloned(),
                post,
                by_sync: true,
                user_delete: false,
                tombstone: None,
                cascade_of: None,
                replaced_at: None,
            });
        }
        for id in seen_gone {
            let at = self.writes.len();
            self.saw_deleted.entry((i, id)).or_default().push(at);
        }
        let mut revived = Vec::new();
        for w in writes {
            let adopted = w.kind == K::Identity
                && w.post
                    .as_deref()
                    .is_some_and(|c| parse(c)["owner"].is_null());
            match w.kind {
                K::Group => self.stats.repairs += 1,
                _ if adopted => self.stats.adoptions += 1,
                _ => {
                    self.stats.revives += 1;
                    revived.push(w.id);
                }
            }
            self.note(format!("c{i} sync wrote {:?} {}", w.kind, w.id));
            self.writes.push(w);
        }
        // A version sync brought back (a cascade revive, or rule 6 keeping a
        // dependent against a tombstone) must not bury a newer remote version
        // as a conflict in the same apply (R32): check (c) would accept it,
        // the newer version being "kept".
        if !revived.is_empty() {
            for (cid, c) in self.conflicts(i)? {
                ensure!(
                    !(revived.contains(&c.record)
                        && c.other.is_some()
                        && dirty.contains(&cid)
                        && !dirty_before.contains(&cid)),
                    "client {i}: sync brought back {} (a cascade revive, or rule 6 keeping it against a tombstone) and turned a newer version into a conflict:\n    {}",
                    c.record,
                    short(c.other.as_ref())
                );
            }
        }
        Ok(())
    }

    /// Ids of client `i`'s unpushed records.
    fn dirty_ids(&self, i: usize) -> Result<BTreeSet<Id>, String> {
        let db = &self.clients[i].db;
        let mut stmt = db
            .prepare_cached("SELECT id FROM records WHERE dirty = 1")
            .map_err(|e| e.to_string())?;
        let ids = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?
            .filter_map(|id| id.ok()?.parse().ok())
            .collect();
        Ok(ids)
    }

    /// Follows client `i`'s unpushed user deletions (see `Client::pending`).
    fn watch_tombstones(&mut self, i: usize) -> Result<(), String> {
        if self.clients[i].pending.is_empty() {
            return Ok(());
        }
        let pending = std::mem::take(&mut self.clients[i].pending);
        for (id, (idx, hlc)) in pending {
            let (now, dirty): (String, bool) = self.clients[i]
                .db
                .query_row(
                    "SELECT hlc, dirty FROM records WHERE id = ?1",
                    [id.to_string()],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .map_err(|e| format!("reading record {id}: {e}"))?;
            let now: Hlc = now.parse().map_err(|_| "bad hlc in the store")?;
            if now == hlc {
                if dirty {
                    self.clients[i].pending.insert(id, (idx, hlc)); // not pushed yet
                }
            } else {
                // Overtaken before it was pushed: by an edit (which keeps it
                // as a conflict) or by another device's tombstone.
                self.writes[idx].replaced_at = Some(self.writes.len());
            }
        }
        Ok(())
    }

    async fn cycle(&mut self, i: usize) -> Result<TickOutcome, String> {
        for _ in 0..MAX_TICKS {
            match self.tick(i).await? {
                TickOutcome::Progress => continue,
                other => return Ok(other),
            }
        }
        Err(format!(
            "client {i}: no end to a sync cycle after {MAX_TICKS} ticks"
        ))
    }

    /// "Sync now" until the cycle ends; a cycle that backs off (the server
    /// just restarted, pooled connections are dead) is retried twice.
    async fn sync(&mut self, i: usize) -> Result<TickOutcome, String> {
        self.stats.syncs += 1;
        let mut outcome = TickOutcome::Idle;
        for _ in 0..3 {
            self.clients[i].engine().request_sync();
            outcome = self.cycle(i).await?;
            if !matches!(outcome, TickOutcome::Backoff(_)) {
                break;
            }
            self.stats.backoffs += 1;
            self.note(format!("c{i} backoff: {:?}", self.status(i)));
        }
        if let SyncStatus::Error { kind, message } = self.status(i) {
            self.note(format!("c{i} sync ended with {kind:?}: {message}"));
        }
        Ok(outcome)
    }

    // ---- actions ----

    /// Keeps HLC order equal to action order (see the module docs).
    async fn space(&mut self) {
        while now_ms() <= self.last_write_ms {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        self.last_write_ms = now_ms();
    }

    async fn apply(&mut self, i: usize, op: Op) -> Result<(), String> {
        self.space().await;
        let result = {
            let mut v = self.clients[i].v();
            perform(&mut v, &op)
        };
        let was_refused = matches!(&result, Err(e) if refused(e));
        let fx = match result {
            Ok(fx) => {
                self.stats.ops += 1;
                self.note(format!("c{i} {op:?}"));
                fx
            }
            Err(e) if was_refused => {
                self.stats.refused += 1;
                self.note(format!("c{i} {op:?}: refused ({e})"));
                Effect::default()
            }
            Err(e) => return Err(format!("client {i}: {op:?} failed: {e}")),
        };
        for (id, k) in fx.created {
            if self.kinds.insert(id, k).is_none() {
                self.order.push(id);
            }
        }
        if let Some((login, owner)) = fx.own_login {
            self.own_login_of.insert(login, owner);
        }
        let post = self.snapshot(i)?;
        let pre = std::mem::replace(&mut self.clients[i].cache, post);
        let post = &self.clients[i].cache;
        let diff = changed(&pre, post);
        ensure!(
            !was_refused || diff.is_empty(),
            "client {i}: a refused {op:?} still changed {diff:?}"
        );
        // The tombstones this action wrote (unpushed, so in the outbox).
        let gone: Vec<Id> = diff
            .iter()
            .copied()
            .filter(|id| !post.contains_key(id))
            .collect();
        let tombstones: BTreeMap<Id, Hlc> = if gone.is_empty() {
            BTreeMap::new()
        } else {
            self.clients[i]
                .v()
                .outbox(1_000_000)
                .map_err(|e| format!("outbox: {e}"))?
                .into_iter()
                .filter(|r| r.deleted && gone.contains(&r.id))
                .map(|r| (r.id, r.hlc))
                .collect()
        };
        let mut watch = Vec::new();
        for id in diff {
            let kind = self.kinds[&id];
            let before = pre.get(&id).cloned();
            let after = post.get(&id).cloned();
            let user_delete = after.is_none() && fx.deleted.contains(&id);
            let cascade_of = match (&after, &before, kind) {
                (None, Some(c), K::Rule) if !user_delete => id_at(&parse(c), "/host_id"),
                (None, Some(c), K::Identity) if !user_delete => id_at(&parse(c), "/owner"),
                (None, Some(_), K::KnownHost) if !user_delete => fx
                    .deleted
                    .iter()
                    .copied()
                    .find(|d| self.kinds.get(d) == Some(&K::KnownHost)),
                _ => None,
            };
            self.writes.push(Write {
                device: i,
                step: self.step,
                id,
                kind,
                pre: before,
                post: after,
                by_sync: false,
                user_delete,
                tombstone: tombstones.get(&id).copied(),
                cascade_of,
                replaced_at: None,
            });
            if let Some(hlc) = tombstones.get(&id) {
                watch.push((id, (self.writes.len() - 1, *hlc)));
            }
        }
        self.clients[i].pending.extend(watch);
        Ok(())
    }

    /// A random user action on client `i`, or `None` if it has nothing to
    /// act on.
    fn random_op(&mut self, i: usize) -> Option<Op> {
        let hosts = self.live(i, K::Host);
        let groups = self.live(i, K::Group);
        let rules = self.live(i, K::Rule);
        let keychain = self.keychain(i);
        let known: Vec<Id> = self.clients[i]
            .v()
            .list::<KnownHost>()
            .map(|l| l.into_iter().map(|k| k.id).collect())
            .unwrap_or_default();
        Some(match self.below(20) {
            0..=2 => Op::CreateHost {
                id: self.rid(),
                group: if self.chance(50) {
                    self.pick(&groups)
                } else {
                    None
                },
                label: self.fresh("host"),
            },
            3..=4 => {
                let id = self.pick(&hosts)?;
                let group = if self.chance(25) {
                    Some(if self.chance(30) {
                        None
                    } else {
                        self.pick(&groups)
                    })
                } else {
                    None
                };
                Op::EditHost {
                    id,
                    label: self.fresh("host"),
                    group,
                }
            }
            5 => Op::DeleteHost {
                id: self.pick(&hosts)?,
            },
            6..=7 => {
                let owner = if self.chance(80) {
                    self.pick(&hosts)?
                } else {
                    self.pick(&groups)?
                };
                Op::SetOwnLogin {
                    owner,
                    username: self.fresh("user"),
                    password: self.fresh("pw"),
                }
            }
            8 => Op::LinkIdentity {
                host: self.pick(&hosts)?,
                identity: self.pick(&keychain)?,
            },
            9 => Op::AddRule {
                id: self.rid(),
                host: self.pick(&hosts)?,
                port: self.port(),
            },
            10 => Op::EditRule {
                id: self.pick(&rules)?,
                port: self.port(),
            },
            11 => Op::DeleteRule {
                id: self.pick(&rules)?,
            },
            12 => Op::CreateGroup {
                id: self.rid(),
                parent: if self.chance(50) {
                    self.pick(&groups)
                } else {
                    None
                },
                name: self.fresh("group"),
            },
            13 => Op::RenameGroup {
                id: self.pick(&groups)?,
                name: self.fresh("group"),
            },
            14 => {
                let id = self.pick(&groups)?;
                let others: Vec<Id> = groups.iter().copied().filter(|g| *g != id).collect();
                Op::MoveGroup {
                    id,
                    parent: if self.chance(25) {
                        None
                    } else {
                        Some(self.pick(&others)?)
                    },
                }
            }
            15 => Op::DeleteGroup {
                id: self.pick(&groups)?,
            },
            16 => Op::CreateIdentity {
                id: self.rid(),
                username: self.fresh("user"),
                password: self.fresh("pw"),
            },
            17 => Op::EditIdentity {
                id: self.pick(&keychain)?,
                password: self.fresh("pw"),
            },
            18 => {
                if self.chance(50) {
                    Op::DeleteIdentity {
                        id: self.pick(&keychain)?,
                    }
                } else {
                    Op::DeleteKnownHost {
                        id: self.pick(&known)?,
                    }
                }
            }
            _ => Op::AddKnownHost {
                id: self.rid(),
                key: self.rng.next_u64() % KNOWN_KEYS,
            },
        })
    }

    async fn random_action(&mut self, i: usize) -> Result<(), String> {
        let op = match self.random_op(i) {
            Some(op) => op,
            None => Op::CreateGroup {
                id: self.rid(),
                parent: None,
                name: self.fresh("group"),
            },
        };
        self.apply(i, op).await
    }

    /// Two clients that have not synced change the same thing.
    async fn race(&mut self) -> Result<(), String> {
        let up = self.unlocked_clients();
        if up.len() < 2 {
            return match self.pick(&up) {
                Some(i) => self.random_action(i).await,
                None => Ok(()),
            };
        }
        let a = self.pick(&up).expect("two clients");
        let others: Vec<usize> = up.into_iter().filter(|&c| c != a).collect();
        let b = self.pick(&others).expect("two clients");
        let both = |s: &Sim, k: K| -> Vec<Id> {
            let theirs = s.live(b, k);
            s.live(a, k)
                .into_iter()
                .filter(|id| theirs.contains(id))
                .collect()
        };
        let pick = self.below(8);
        if pick >= 6 {
            let rules = both(self, K::Rule);
            if let Some(rule) = self.pick(&rules) {
                return if pick == 6 {
                    self.race_delete_after_pull(a, b, rule).await
                } else {
                    self.race_owner_and_dependent_edit(a, b, rule).await
                };
            }
            return self.random_action(a).await;
        }
        let pair = match pick {
            0 | 1 => {
                // A host (preferably with its own login and forward rules)
                // deleted on A while B edits it.
                let rich: Vec<Id> = self
                    .rich_hosts(a)
                    .into_iter()
                    .filter(|h| self.clients[b].cache.contains_key(h))
                    .collect();
                let host = if !rich.is_empty() && self.chance(75) {
                    self.pick(&rich)
                } else {
                    let hosts = both(self, K::Host);
                    self.pick(&hosts)
                };
                host.map(|h| {
                    let edit = match self.below(3) {
                        0 => Op::EditHost {
                            id: h,
                            label: self.fresh("host"),
                            group: None,
                        },
                        1 => Op::SetOwnLogin {
                            owner: h,
                            username: self.fresh("user"),
                            password: self.fresh("pw"),
                        },
                        _ => Op::AddRule {
                            id: self.rid(),
                            host: h,
                            port: self.port(),
                        },
                    };
                    (Op::DeleteHost { id: h }, edit)
                })
            }
            2 => {
                // Two groups moved under each other: a cycle once merged.
                let groups = both(self, K::Group);
                let g1 = self.pick(&groups);
                let rest: Vec<Id> = groups.into_iter().filter(|g| Some(*g) != g1).collect();
                match (g1, self.pick(&rest)) {
                    (Some(g1), Some(g2)) => Some((
                        Op::MoveGroup {
                            id: g1,
                            parent: Some(g2),
                        },
                        Op::MoveGroup {
                            id: g2,
                            parent: Some(g1),
                        },
                    )),
                    _ => None,
                }
            }
            3 => {
                // One password changed on both.
                let ids: Vec<Id> = self
                    .keychain(a)
                    .into_iter()
                    .filter(|id| self.keychain(b).contains(id))
                    .collect();
                match self.pick(&ids) {
                    Some(id) => Some((
                        Op::EditIdentity {
                            id,
                            password: self.fresh("pw"),
                        },
                        Op::EditIdentity {
                            id,
                            password: self.fresh("pw"),
                        },
                    )),
                    None => {
                        let hosts = both(self, K::Host);
                        self.pick(&hosts).map(|h| {
                            (
                                Op::SetOwnLogin {
                                    owner: h,
                                    username: self.fresh("user"),
                                    password: self.fresh("pw"),
                                },
                                Op::SetOwnLogin {
                                    owner: h,
                                    username: self.fresh("user"),
                                    password: self.fresh("pw"),
                                },
                            )
                        })
                    }
                }
            }
            4 => {
                // The same key trusted on both.
                let key = self.rng.next_u64() % KNOWN_KEYS;
                Some((
                    Op::AddKnownHost {
                        id: self.rid(),
                        key,
                    },
                    Op::AddKnownHost {
                        id: self.rid(),
                        key,
                    },
                ))
            }
            _ => {
                // A forward rule deleted on A while B edits its host.
                let rules = both(self, K::Rule);
                self.pick(&rules).and_then(|r| {
                    let host = id_at(&self.json(a, r), "/host_id")?;
                    Some((
                        Op::DeleteRule { id: r },
                        Op::EditHost {
                            id: host,
                            label: self.fresh("host"),
                            group: None,
                        },
                    ))
                })
            }
        };
        let Some((op_a, op_b)) = pair else {
            return self.random_action(a).await;
        };
        self.stats.races += 1;
        self.note(format!("race: c{a} and c{b}"));
        if self.chance(50) {
            self.apply(a, op_a).await?;
            self.apply(b, op_b).await
        } else {
            self.apply(b, op_b).await?;
            self.apply(a, op_a).await
        }
    }

    /// The review's R31 scenario: B deletes the rule's host (the rule is
    /// stashed); A edits the rule and syncs; B pulls and applies (the edit
    /// beats B's cascade) but stops before pushing; B deletes the rule; A
    /// renames the host and syncs; B syncs, and its host delete loses. B's
    /// delete must not be undone by the stash, nor A's edit lost.
    async fn race_delete_after_pull(&mut self, a: usize, b: usize, rule: Id) -> Result<(), String> {
        let online = self.online_clients();
        let Some(host) = id_at(&self.json(a, rule), "/host_id") else {
            return Ok(());
        };
        if !online.contains(&a)
            || !online.contains(&b)
            || !self.clients[b].cache.contains_key(&host)
        {
            return self.random_action(a).await;
        }
        self.stats.races += 1;
        self.note(format!("race (delete after pull): c{a} and c{b}"));
        self.apply(b, Op::DeleteHost { id: host }).await?;
        let port = self.port();
        self.apply(a, Op::EditRule { id: rule, port }).await?;
        self.sync(a).await?;
        // B pulls and applies, but stops before its push.
        self.clients[b].engine().request_sync();
        let before = self.clients[b].cache.clone();
        for _ in 0..MAX_TICKS {
            if self.tick(b).await? != TickOutcome::Progress || self.clients[b].cache != before {
                break;
            }
        }
        self.apply(b, Op::DeleteRule { id: rule }).await?;
        let label = self.fresh("host");
        self.apply(
            a,
            Op::EditHost {
                id: host,
                label,
                group: None,
            },
        )
        .await?;
        self.sync(a).await?;
        self.sync(b).await?;
        Ok(())
    }

    /// The review's R32 ordering: B deletes the rule's host; A edits the
    /// rule (or its host's own-login password) and renames the host, then
    /// syncs; B syncs. B's pull holds the host and the newer dependent: the
    /// host applies first and B's delete loses, and the newer dependent,
    /// still queued, must win over B's stashed copy.
    async fn race_owner_and_dependent_edit(
        &mut self,
        a: usize,
        b: usize,
        rule: Id,
    ) -> Result<(), String> {
        let online = self.online_clients();
        let Some(host) = id_at(&self.json(a, rule), "/host_id") else {
            return Ok(());
        };
        if !online.contains(&a)
            || !online.contains(&b)
            || !self.clients[b].cache.contains_key(&host)
        {
            return self.random_action(a).await;
        }
        self.stats.races += 1;
        self.note(format!("race (owner and dependent edit): c{a} and c{b}"));
        self.apply(b, Op::DeleteHost { id: host }).await?;
        let edit = if self.chance(50) {
            Op::EditRule {
                id: rule,
                port: self.port(),
            }
        } else {
            Op::SetOwnLogin {
                owner: host,
                username: self.fresh("user"),
                password: self.fresh("pw"),
            }
        };
        self.apply(a, edit).await?;
        let label = self.fresh("host");
        self.apply(
            a,
            Op::EditHost {
                id: host,
                label,
                group: None,
            },
        )
        .await?;
        self.sync(a).await?;
        self.sync(b).await?;
        Ok(())
    }

    fn lock(&mut self, i: usize) {
        self.clients[i].v().lock();
        self.stats.locks += 1;
        self.note(format!("c{i} lock"));
    }

    /// Unlocks client `i`; its records must be what they were when it locked.
    fn unlock(&mut self, i: usize) -> Result<(), String> {
        self.clients[i]
            .v()
            .unlock(pw(PW))
            .map_err(|e| format!("client {i}: unlock: {e}"))?;
        self.note(format!("c{i} unlock"));
        let now = self.snapshot(i)?;
        let diff = changed(&self.clients[i].cache, &now);
        ensure!(
            diff.is_empty(),
            "client {i}: records changed while locked or across a restart: {diff:?}"
        );
        Ok(())
    }

    /// Drops the engine and the open vault, reopens the file (locked, as the
    /// app starts) and starts a new engine on it.
    fn restart_client(&mut self, i: usize) -> Result<(), String> {
        self.stats.client_restarts += 1;
        self.note(format!("c{i} restart"));
        let c = &mut self.clients[i];
        c.engine = None;
        let path = c.path.clone();
        c.vault = open(&path)?;
        c.engine = Some(
            Engine::new(c.vault.clone(), c.tokens.clone())
                .map_err(|e| format!("client {i}: engine after restart: {e}"))?
                .ok_or_else(|| format!("client {i}: sync off after restart"))?,
        );
        Ok(())
    }

    async fn restart_server(&mut self) -> Result<(), String> {
        self.stats.server_restarts += 1;
        self.note("server restart".into());
        // Dropping the proxied connections lets the old server shut down.
        self.proxy.set_down(true);
        let srv = self.srv.take().expect("server").restart().await;
        ensure!(srv.pin == self.pin, "server certificate changed on restart");
        self.proxy.retarget(srv.addr());
        self.proxy.set_down(false);
        self.srv = Some(srv);
        Ok(())
    }

    /// One step of the run.
    async fn act(&mut self) -> Result<(), String> {
        let roll = self.below(100);
        match roll {
            0..=44 => {
                let up = self.unlocked_clients();
                if let Some(i) = self.pick(&up) {
                    self.random_action(i).await?;
                }
            }
            45..=54 => self.race().await?,
            55..=72 => {
                let online = self.online_clients();
                if let Some(i) = self.pick(&online) {
                    let outcome = self.sync(i).await?;
                    self.note(format!("c{i} sync: {outcome:?}"));
                }
            }
            73..=80 => {
                // A cycle left half done.
                let online = self.online_clients();
                if let Some(i) = self.pick(&online) {
                    let ticks = 1 + self.below(4);
                    self.note(format!("c{i} {ticks} tick(s)"));
                    self.clients[i].engine().request_sync();
                    for _ in 0..ticks {
                        if self.tick(i).await? != TickOutcome::Progress {
                            break;
                        }
                    }
                }
            }
            81..=84 => {
                let i = self.below(self.clients.len());
                let k = 3 + self.below(25);
                self.clients[i].offline_until = self.step + k;
                self.stats.partitions += 1;
                self.note(format!("c{i} offline for {k} steps"));
            }
            85..=89 => {
                let i = self.below(self.clients.len());
                if self.clients[i].unlocked() {
                    self.lock(i);
                } else {
                    self.unlock(i)?;
                }
            }
            90..=92 => {
                let i = self.below(self.clients.len());
                self.restart_client(i)?;
            }
            93..=94 => self.restart_server().await?,
            _ => {
                for i in self.online_clients() {
                    let outcome = self.sync(i).await?;
                    self.note(format!("c{i} sync: {outcome:?}"));
                }
            }
        }
        Ok(())
    }

    /// Everyone online and unlocked, then round-robin syncs until a round
    /// is quiet.
    async fn heal(&mut self) -> Result<(), String> {
        self.note("heal".into());
        for i in 0..self.clients.len() {
            self.clients[i].offline_until = 0;
            if !self.clients[i].unlocked() {
                self.unlock(i)?;
            }
        }
        for round in 1..=MAX_ROUNDS {
            let mut busy = false;
            for i in 0..self.clients.len() {
                let pending = {
                    let v = self.clients[i].v();
                    let out = v.outbox_len().map_err(|e| e.to_string())?;
                    let queued = v.inbound_len().map_err(|e| e.to_string())?;
                    out + queued
                };
                busy |= pending > 0;
                let before = self.clients[i].cache.clone();
                let outcome = self.sync(i).await?;
                ensure!(
                    outcome == TickOutcome::Idle,
                    "client {i}: final sync ended {outcome:?}, status {:?}",
                    self.status(i)
                );
                busy |= self.clients[i].cache != before;
            }
            self.note(format!("heal round {round}: busy {busy}"));
            if !busy {
                self.stats.rounds += round;
                return Ok(());
            }
        }
        Err(format!(
            "did not go quiet within {MAX_ROUNDS} rounds of syncing every client"
        ))
    }

    /// Every write event of `id`, one per line (for failure reports).
    fn history(&self, id: Id) -> String {
        self.writes
            .iter()
            .filter(|f| f.id == id)
            .map(|f| {
                format!(
                    "step {} c{}{}: {} -> {}",
                    f.step,
                    f.device,
                    if f.by_sync { " (sync)" } else { "" },
                    short(f.pre.as_ref()),
                    short(f.post.as_ref())
                )
            })
            .collect::<Vec<_>>()
            .join("\n      ")
    }

    fn check(&mut self) -> Result<(), String> {
        let n = self.clients.len();
        // (d) nothing left to push or apply, every engine idle.
        for i in 0..n {
            let (out, queued) = {
                let v = self.clients[i].v();
                (
                    v.outbox_len().map_err(|e| e.to_string())?,
                    v.inbound_len().map_err(|e| e.to_string())?,
                )
            };
            ensure!(
                out == 0 && queued == 0,
                "client {i}: {out} unpushed and {queued} queued row(s) after the final sync"
            );
            let status = self.status(i);
            ensure!(
                matches!(status, SyncStatus::Idle { .. }),
                "client {i}: ends with status {status:?}"
            );
        }

        // (a) identical decrypted state.
        let snaps: Vec<Snap> = (0..n).map(|i| self.snapshot(i)).collect::<Result<_, _>>()?;
        let confl: Vec<_> = (0..n)
            .map(|i| self.conflicts(i))
            .collect::<Result<Vec<_>, _>>()?;
        let known: Vec<Vec<Id>> = (0..n)
            .map(|i| {
                self.clients[i]
                    .v()
                    .list::<KnownHost>()
                    .map(|l| l.into_iter().map(|k| k.id).collect())
                    .map_err(|e| e.to_string())
            })
            .collect::<Result<_, _>>()?;
        for i in 1..n {
            let diff = changed(&snaps[0], &snaps[i]);
            ensure!(
                diff.is_empty(),
                "clients 0 and {i} differ in {} record(s), e.g. {:?}:\n  c0 {}\n  c{i} {}",
                diff.len(),
                diff[0],
                short(snaps[0].get(&diff[0])),
                short(snaps[i].get(&diff[0]))
            );
            ensure!(
                confl[0] == confl[i],
                "clients 0 and {i} hold different conflicts: {} vs {}",
                confl[0].len(),
                confl[i].len()
            );
            ensure!(
                known[0] == known[i],
                "clients 0 and {i} list different known hosts"
            );
        }
        let fin = &snaps[0];
        let conflicts = &confl[0];

        // (b) no group cycle.
        let parents: BTreeMap<Id, Option<Id>> = fin
            .iter()
            .filter(|(id, _)| self.kinds[id] == K::Group)
            .map(|(id, c)| (*id, id_at(&parse(c), "/parent_id")))
            .collect();
        for &start in parents.keys() {
            let mut at = Some(start);
            for _ in 0..=parents.len() {
                at = at.and_then(|g| parents.get(&g).copied().flatten());
            }
            ensure!(
                at.is_none(),
                "group cycle through {start} after the final sync"
            );
        }

        // Dependents of live owners.
        for (id, c) in fin {
            let v = parse(c);
            match self.kinds[id] {
                K::Host | K::Group => {
                    // A login the user deleted (once adopted into the
                    // Keychain it can be) while another device linked it
                    // reads as unset, like any reference to a deleted record.
                    if let Some(login) = id_at(&v, "/settings/identity_id")
                        && self.own_login_of.get(&login) == Some(id)
                        && !self
                            .writes
                            .iter()
                            .rev()
                            .find(|w| w.id == login)
                            .is_some_and(|w| w.user_delete)
                    {
                        ensure!(
                            fin.contains_key(&login),
                            "{:?} {id} links its own login {login}, which is gone\n    writes of the login:\n      {}",
                            self.kinds[id],
                            self.history(login)
                        );
                    }
                    if let Some(rules) = v["forward_rule_ids"].as_array() {
                        for r in rules.iter().filter_map(|r| r.as_str()?.parse::<Id>().ok()) {
                            // A rule the user deleted while this device's
                            // copy of the host was itself deleted (so the
                            // host was not updated) stays listed once an
                            // edit restores the host; it reads as missing,
                            // like any reference to a deleted record.
                            let last_live = self
                                .writes
                                .iter()
                                .rposition(|w| w.id == r && w.post.is_some());
                            let user_deleted =
                                self.writes.iter().enumerate().any(|(j, w)| {
                                    w.id == r && w.user_delete && Some(j) > last_live
                                });
                            ensure!(
                                fin.contains_key(&r) || user_deleted,
                                "host {id} lists forward rule {r}, which is gone\n    writes of the rule:\n      {}\n    writes of the host:\n      {}",
                                self.history(r),
                                self.history(*id)
                            );
                        }
                    }
                }
                _ => {}
            }
        }

        // (c) no lost version (see the module docs). Base cases first, then
        // "superseded" to a fixpoint: a superseding write only counts if it
        // passes itself, or is a cascade whose owner's change stands.
        let is_version = |w: &Write| w.post.is_some() || w.user_delete;
        let links = |owner: Id, dep: Id| -> bool {
            fin.get(&owner).is_some_and(|c| {
                let v = parse(c);
                id_at(&v, "/settings/identity_id") == Some(dep)
                    || v["forward_rule_ids"]
                        .as_array()
                        .is_some_and(|ids| ids.iter().any(|x| x.as_str() == Some(&dep.to_string())))
            })
        };
        let stands = |w: &Write| -> bool {
            match (w.kind, w.cascade_of) {
                (K::KnownHost, Some(twin)) => !fin.contains_key(&twin),
                (_, Some(owner)) => !links(owner, w.id),
                _ => false,
            }
        };
        let mut pass: Vec<bool> = Vec::with_capacity(self.writes.len());
        for (idx, w) in self.writes.iter().enumerate() {
            let ok = match &w.post {
                Some(content) => {
                    fin.get(&w.id) == Some(content)
                        || conflicts
                            .values()
                            .any(|c| c.record == w.id && c.other.as_ref() == Some(content))
                        || match w.kind.repair_field() {
                            Some(field) => {
                                let repaired = cleared(w.kind, content, field)?;
                                self.writes.iter().any(|f| {
                                    f.by_sync && f.id == w.id && f.post.as_ref() == Some(&repaired)
                                })
                            }
                            None => false,
                        }
                }
                None => {
                    // Its own deletion kept as a conflict, or, when its
                    // tombstone was replaced on its device before it was
                    // pushed (delete vs delete merges concurrent deletions
                    // into one), that of a concurrent deletion of the same
                    // record: by another device, before the replacement,
                    // and not seen by this device before it deleted.
                    let deleted_kept = |d: &Write| {
                        d.tombstone.is_some_and(|hlc| {
                            conflicts
                                .get(&conflict_id(w.id, hlc))
                                .is_some_and(|c| c.record == w.id && c.other.is_none())
                        })
                    };
                    let seen = self.saw_deleted.get(&(w.device, w.id));
                    let concurrent = |j: usize, d: &Write| {
                        d.device != w.device
                            && w.replaced_at.is_some_and(|until| j < until)
                            && (j > idx
                                || !seen.is_some_and(|at| at.iter().any(|&t| t > j && t <= idx)))
                    };
                    !fin.contains_key(&w.id)
                        || deleted_kept(w)
                        || (!w.user_delete && stands(w))
                        || self.writes.iter().enumerate().any(|(j, d)| {
                            d.post.is_none() && d.id == w.id && concurrent(j, d) && deleted_kept(d)
                        })
                }
            };
            pass.push(ok);
        }
        loop {
            let mut changed_any = false;
            for (idx, w) in self.writes.iter().enumerate() {
                if pass[idx] || !is_version(w) {
                    continue;
                }
                let Some(content) = &w.post else { continue };
                let superseded = self.writes.iter().enumerate().any(|(j, f)| {
                    j != idx && f.id == w.id && f.pre.as_ref() == Some(content) && pass[j]
                });
                if superseded {
                    pass[idx] = true;
                    changed_any = true;
                }
            }
            if !changed_any {
                break;
            }
        }
        let mut lost = Vec::new();
        let mut versions = 0;
        for (idx, w) in self.writes.iter().enumerate() {
            if !is_version(w) {
                continue;
            }
            versions += 1;
            if pass[idx] {
                continue;
            }
            let v = &w.post;
            let about: Vec<String> = conflicts
                .values()
                .filter(|c| c.record == w.id)
                .map(|c| short(c.other.as_ref()))
                .collect();
            let history = self.history(w.id);
            lost.push(format!(
                "{:?} {} written by c{} at step {}{}{}:\n    version {}\n    final   {}\n    conflicts about it: {about:?}\n    writes of it:\n      {}",
                w.kind,
                w.id,
                w.device,
                w.step,
                if w.by_sync { " (by sync)" } else { "" },
                if w.user_delete { " (a deletion)" } else { "" },
                short(v.as_ref()),
                short(fin.get(&w.id)),
                history,
            ));
        }
        self.stats.versions = versions;
        ensure!(
            lost.is_empty(),
            "{} lost version(s):\n  {}",
            lost.len(),
            lost.join("\n  ")
        );

        self.stats.conflicts = conflicts.len();
        self.stats.deletion_conflicts = conflicts.values().filter(|c| c.other.is_none()).count();
        self.stats.known_dupes = self.live(0, K::KnownHost).len() - known[0].len();
        Ok(())
    }
}
