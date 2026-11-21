//! Drives the app's command logic (`AppState`, the same code the `#[tauri::command]`
//! wrappers call) against the docker sshd fixture, without a window.
//!
//! `tauri::ipc::Channel::new` builds a channel from a closure, so the real
//! `ChannelSink` is exercised here, including raw binary frame delivery.
//!
//! Run with: `TETHER_SSH_FIXTURE=1 cargo test -p tether-app` (after `tests/fixtures/up.sh`).

use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use core_api::vault::{MemoryKeyStore, SharedVault, VaultService};
use core_api::{Prompt, PromptBridge, PromptResponse};
use core_model::KnownHost;
use core_ssh::ConnectStep;
use secrecy::SecretString;
use tauri::ipc::{Channel, InvokeResponseBody};
use tempfile::TempDir;
use tether_app_lib::commands::ConnectParamsDto;
use tether_app_lib::{AppState, EventSink, SessionClosed, VaultSlot};

/// An `AppState` over a fresh vault, with a prompt bridge that records every
/// prompt and answers it with `answer` (standing in for the user).
struct Harness {
    state: AppState,
    vault: SharedVault,
    prompts: Arc<Mutex<Vec<Prompt>>>,
    _dir: TempDir,
}

fn harness(unlocked: bool, answer: fn() -> PromptResponse) -> Harness {
    harness_answering(unlocked, Arc::new(move |_| answer()))
}

/// [`harness`] whose stand-in user answers each prompt by its kind.
fn harness_answering(
    unlocked: bool,
    answer: Arc<dyn Fn(&Prompt) -> PromptResponse + Send + Sync>,
) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let mut v = VaultService::open_for_tests(
        &dir.path().join("vault.sqlite"),
        Box::new(MemoryKeyStore::default()),
    )
    .unwrap();
    v.create(SecretString::from("master")).unwrap();
    if !unlocked {
        v.lock();
    }
    let vault: SharedVault = Arc::new(Mutex::new(v));

    let prompts: Arc<Mutex<Vec<Prompt>>> = Arc::default();
    let me: Arc<Mutex<Weak<PromptBridge>>> = Arc::new(Mutex::new(Weak::new()));
    let bridge = Arc::new(PromptBridge::new({
        let prompts = prompts.clone();
        let me = me.clone();
        Arc::new(move |id: String, prompt: Prompt| {
            let response = answer(&prompt);
            prompts.lock().unwrap().push(prompt);
            if let Some(b) = me.lock().unwrap().upgrade() {
                b.respond(&id, response);
            }
            Ok(())
        })
    }));
    *me.lock().unwrap() = Arc::downgrade(&bridge);

    Harness {
        state: AppState::new(
            VaultSlot::Ready(vault.clone()),
            bridge,
            Arc::new(NoEvents),
            Arc::new(core_ssh::RusshTransport),
        ),
        vault,
        prompts,
        _dir: dir,
    }
}

/// Events are not under test here.
struct NoEvents;

impl EventSink for NoEvents {
    fn emit(&self, _event: &str, _payload: serde_json::Value) -> Result<(), String> {
        Ok(())
    }
}

fn no_op_channels() -> (Channel<InvokeResponseBody>, Channel<SessionClosed>) {
    (
        Channel::new(|_| Ok(())),
        Channel::<SessionClosed>::new(|_| Ok(())),
    )
}

fn no_progress() -> Channel<ConnectStep> {
    Channel::new(|_| Ok(()))
}

/// A progress channel and the steps it received, as their wire names.
fn recording_progress() -> (Channel<ConnectStep>, Arc<Mutex<Vec<String>>>) {
    let steps: Arc<Mutex<Vec<String>>> = Arc::default();
    let channel = {
        let steps = steps.clone();
        Channel::new(move |body| {
            let InvokeResponseBody::Json(json) = body else {
                panic!("a connect step must be JSON");
            };
            steps
                .lock()
                .unwrap()
                .push(serde_json::from_str(&json).unwrap());
            Ok(())
        })
    };
    (channel, steps)
}

/// True when the docker sshd fixture is enabled; otherwise prints one skip
/// line for the calling test.
fn fixture_enabled() -> bool {
    let on = std::env::var("TETHER_SSH_FIXTURE").as_deref() == Ok("1");
    if !on {
        eprintln!("skipping: set TETHER_SSH_FIXTURE=1 (and run tests/fixtures/up.sh)");
    }
    on
}

fn fixture_params() -> ConnectParamsDto {
    serde_json::from_value(serde_json::json!({
        "host": "127.0.0.1",
        "port": 2222,
        "username": "tether",
        "password": "tether-test",
        "cols": 80,
        "rows": 24,
    }))
    .unwrap()
}

#[derive(Default)]
struct Captured {
    /// Raw frame payloads, in arrival order.
    frames: Vec<Vec<u8>>,
    /// Any frame that arrived as JSON instead of raw bytes (must stay empty).
    json_frames: usize,
    /// `onClosed` payloads (`{reason, exitStatus}`).
    closed: Vec<serde_json::Value>,
}

impl Captured {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.frames.concat()).into_owned()
    }
}

async fn wait_for(cap: &Mutex<Captured>, what: &str, pred: impl Fn(&Captured) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if pred(&cap.lock().unwrap()) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}; got {:?}",
            cap.lock().unwrap().text()
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Output and closed channels that record into `cap`.
fn capture_channels(
    cap: &Arc<Mutex<Captured>>,
) -> (Channel<InvokeResponseBody>, Channel<SessionClosed>) {
    let on_output = {
        let cap = cap.clone();
        Channel::new(move |body| {
            let mut c = cap.lock().unwrap();
            match body {
                InvokeResponseBody::Raw(bytes) => c.frames.push(bytes),
                InvokeResponseBody::Json(_) => c.json_frames += 1,
            }
            Ok(())
        })
    };
    let on_closed = {
        let cap = cap.clone();
        Channel::<SessionClosed>::new(move |body| {
            let InvokeResponseBody::Json(json) = body else {
                panic!("closed notification must be JSON");
            };
            cap.lock()
                .unwrap()
                .closed
                .push(serde_json::from_str(&json).unwrap());
            Ok(())
        })
    };
    (on_output, on_closed)
}

#[tokio::test]
async fn shell_roundtrip_through_command_layer() {
    if !fixture_enabled() {
        return;
    }

    // The user trusts the fixture's key on first connect and saves it.
    let h = harness(true, || PromptResponse::Accept { save: true });
    let state = &h.state;
    let cap = Arc::new(Mutex::new(Captured::default()));
    let (on_output, on_closed) = capture_channels(&cap);

    let opened = state
        .open_shell(
            "main",
            fixture_params(),
            on_output,
            on_closed,
            no_progress(),
        )
        .await
        .expect("open_shell");
    // The vault verifier asked about the unknown key and saved it.
    {
        let prompts = h.prompts.lock().unwrap();
        assert_eq!(prompts.len(), 1, "prompts: {prompts:?}");
        let Prompt::HostKeyUnknown { host, port, key } = &prompts[0] else {
            panic!("expected HostKeyUnknown, got {:?}", prompts[0]);
        };
        assert_eq!((host.as_str(), *port), ("127.0.0.1", 2222));
        assert!(key.sha256_fingerprint.starts_with("SHA256:"), "{key:?}");
        let known = h.vault.lock().unwrap().list::<KnownHost>().unwrap();
        assert_eq!(known.len(), 1);
        assert_eq!(
            (known[0].host_pattern.as_str(), known[0].port),
            ("127.0.0.1", 2222)
        );
    }
    let id = opened.session_id;

    state
        .write_session("main", &id, b"echo hi-app-$((20+22))\n".to_vec())
        .await
        .expect("write_session");
    // Computed marker: only the shell's output contains `hi-app-42`, not the echoed command line.
    wait_for(&cap, "echo output", |c| c.text().contains("hi-app-42")).await;

    // Acking and resizing must not error on a live session.
    let delivered: usize = cap.lock().unwrap().frames.iter().map(Vec::len).sum();
    state.ack_session("main", &id, delivered).unwrap();
    state
        .resize_session("main", &id, 120, 40)
        .await
        .expect("resize_session");

    state.close_session("main", &id).await.unwrap();
    wait_for(&cap, "closed notification", |c| !c.closed.is_empty()).await;

    {
        let c = cap.lock().unwrap();
        assert_eq!(
            c.json_frames, 0,
            "output frames must be raw binary, not JSON"
        );
        assert_eq!(c.closed.len(), 1, "closed fires exactly once");
        // Closed by us: a normal end, no exit status.
        assert_eq!(
            c.closed[0],
            serde_json::json!({"reason": null, "exitStatus": null})
        );
    }

    // Now known: the next connection is not prompted.
    let (out, closed) = no_op_channels();
    let again = state
        .open_shell("main", fixture_params(), out, closed, no_progress())
        .await
        .expect("second open_shell");
    assert_eq!(h.prompts.lock().unwrap().len(), 1);
    state
        .close_session("main", &again.session_id)
        .await
        .unwrap();
}

#[tokio::test]
async fn open_shell_reports_auth_failure_as_error_string() {
    if !fixture_enabled() {
        return;
    }
    let h = harness(true, || PromptResponse::Accept { save: false });
    let params: ConnectParamsDto = serde_json::from_value(serde_json::json!({
        "host": "127.0.0.1", "port": 2222, "username": "tether",
        "password": "wrong-password", "cols": 80, "rows": 24,
    }))
    .unwrap();
    let (out, closed) = no_op_channels();
    let err = h
        .state
        .open_shell("main", params, out, closed, no_progress())
        .await
        .unwrap_err();
    assert_eq!(err.code, "authFailed");
    assert!(err.message.contains("authentication failed"), "got: {err}");
}

/// No Phase 0 bypass: with the vault locked the app cannot check the host
/// key, so it refuses without asking (even if the user would accept), with an
/// error the UI can tell apart from a user rejection.
#[tokio::test]
async fn open_shell_with_locked_vault_rejects_host_key() {
    if !fixture_enabled() {
        return;
    }
    let h = harness(false, || PromptResponse::Accept { save: true });
    let (out, closed) = no_op_channels();
    let err = h
        .state
        .open_shell("main", fixture_params(), out, closed, no_progress())
        .await
        .unwrap_err();
    assert_eq!(err.code, "hostKeyUnverifiable");
    assert_eq!(err.message, "vault locked: host keys cannot be verified");
    assert!(h.prompts.lock().unwrap().is_empty());
}

/// The user rejecting an unknown key ends the attempt.
#[tokio::test]
async fn open_shell_unknown_host_key_prompt_rejected() {
    if !fixture_enabled() {
        return;
    }
    let h = harness(true, || PromptResponse::Reject);
    let (out, closed) = no_op_channels();
    let err = h
        .state
        .open_shell("main", fixture_params(), out, closed, no_progress())
        .await
        .unwrap_err();
    assert_eq!(err.code, "hostKeyRejected", "got: {err}");
    assert_eq!(h.prompts.lock().unwrap().len(), 1);
    assert!(
        h.vault
            .lock()
            .unwrap()
            .list::<KnownHost>()
            .unwrap()
            .is_empty()
    );
}

// ---------------------------------------------------------------------------
// Vault hosts: one shared connection per host

/// `RusshTransport` that counts connections and keeps each one's close
/// signal, so a test can see reuse and release.
#[derive(Default)]
struct CountingTransport {
    connects: std::sync::atomic::AtomicUsize,
    signals: Mutex<Vec<core_ssh::ClosedSignal>>,
}

#[async_trait::async_trait]
impl core_ssh::SshTransport for CountingTransport {
    async fn open_shell(
        &self,
        p: core_ssh::ConnectParams,
        v: Arc<dyn core_ssh::HostKeyVerifier>,
    ) -> Result<core_ssh::ShellHandle, core_ssh::SshError> {
        core_ssh::RusshTransport.open_shell(p, v).await
    }

    async fn connect(
        &self,
        p: &core_ssh::ConnectParams,
        v: Arc<dyn core_ssh::HostKeyVerifier>,
        prompter: Arc<dyn core_ssh::AuthPrompter>,
    ) -> Result<core_ssh::SshConnection, core_ssh::SshError> {
        let conn = core_ssh::RusshTransport.connect(p, v, prompter).await?;
        self.connects
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.signals.lock().unwrap().push(conn.close_signal());
        Ok(conn)
    }
}

/// Records keep-alive's start/stop calls.
#[derive(Default)]
struct RecordingDriver(Mutex<Vec<bool>>);

impl tether_app_lib::keepalive::LocationDriver for RecordingDriver {
    fn request_permission(&self) {}
    fn set_updating(&self, on: bool, _generation: u64) {
        self.0.lock().unwrap().push(on);
    }
}

#[tokio::test]
async fn host_shell_sftp_and_forward_share_one_connection() {
    if !fixture_enabled() {
        return;
    }
    use std::sync::atomic::Ordering;

    let base = harness(true, || PromptResponse::Accept { save: true });
    let transport = Arc::new(CountingTransport::default());
    let bridge = Arc::new(PromptBridge::new(Arc::new(|_, _| {
        Err("no prompt expected once the key is known".to_string())
    })));
    // Trust the fixture's key first (the base harness answers prompts).
    let (out, closed) = no_op_channels();
    let first = base
        .state
        .open_shell("main", fixture_params(), out, closed, no_progress())
        .await
        .expect("trust the fixture key");
    base.state
        .close_session("main", &first.session_id)
        .await
        .unwrap();
    let state = AppState::new(
        VaultSlot::Ready(base.vault.clone()),
        bridge,
        Arc::new(NoEvents),
        transport.clone(),
    );
    // Keep-alive on: it must count the forward as well as the shells.
    let driver = Arc::new(RecordingDriver::default());
    let keepalive = state.keepalive();
    keepalive.install(driver.clone());
    keepalive.set_permission(tether_app_lib::keepalive::Permission::Granted);
    keepalive.set_enabled(true).await.unwrap();

    let identity = state
        .upsert_entity(
            "identity",
            serde_json::json!({"label": "fixture", "username": "tether", "password": "tether-test"}),
        )
        .await
        .unwrap();
    let host = state
        .upsert_entity(
            "host",
            serde_json::json!({"label": "fixture 🐳", "address": "127.0.0.1",
                "settings": {"port": 2222, "identityId": identity["id"]}}),
        )
        .await
        .unwrap();
    let host_id: uuid::Uuid = host["id"].as_str().unwrap().parse().unwrap();
    let target = || {
        serde_json::from_value::<tether_app_lib::commands::SessionTarget>(
            serde_json::json!({"kind": "host", "hostId": host_id}),
        )
        .unwrap()
    };

    // Two shells, SFTP and a forward: one connection.
    let cap = Arc::new(Mutex::new(Captured::default()));
    let on_output = {
        let cap = cap.clone();
        Channel::new(move |body| {
            if let InvokeResponseBody::Raw(bytes) = body {
                cap.lock().unwrap().frames.push(bytes);
            }
            Ok(())
        })
    };
    let (_, on_closed) = no_op_channels();
    let s1 = state
        .open_session(
            "main",
            target(),
            80,
            24,
            on_output,
            on_closed,
            no_progress(),
        )
        .await
        .expect("shell on vault host");
    let (out, closed) = no_op_channels();
    let s2 = state
        .open_session("main", target(), 80, 24, out, closed, no_progress())
        .await
        .expect("second shell");
    // The connection is live, but SFTP still needs the vault unlocked.
    state.vault_lock().await.unwrap();
    assert_eq!(state.sftp_open(host_id).await.unwrap_err().code, "locked");
    state
        .vault_unlock(SecretString::from("master"))
        .await
        .unwrap();
    state.sftp_open(host_id).await.expect("sftp_open");
    let listed = state
        .sftp_list(host_id, "/".into())
        .await
        .expect("sftp_list");
    assert!(
        listed.iter().any(|e| e.name == "tmp" && e.is_dir),
        "{listed:?}"
    );
    let rule = state
        .upsert_entity(
            "forward_rule",
            serde_json::json!({"hostId": host_id, "label": "ssh", "kind": "local",
                "bindAddr": "127.0.0.1", "bindPort": 0, "destHost": "127.0.0.1",
                "destPort": 22}),
        )
        .await
        .unwrap();
    let rule_id: uuid::Uuid = rule["id"].as_str().unwrap().parse().unwrap();
    let status = state.forward_start(rule_id).await.expect("forward_start");
    assert!(
        matches!(status, core_api::ForwardStatus::Active { .. }),
        "{status:?}"
    );
    assert_eq!(transport.connects.load(Ordering::SeqCst), 1);

    state
        .write_session("main", &s1.session_id, b"echo hi-host-$((40+2))\n".to_vec())
        .await
        .unwrap();
    wait_for(&cap, "shell output", |c| c.text().contains("hi-host-42")).await;
    let work = state.active_work("main");
    assert_eq!(work.sessions.len(), 2);
    assert!(work.sessions.iter().all(|s| s.title == "fixture 🐳"));
    // Running forwards count as live work too (M1).
    assert_eq!(
        work.forwards
            .iter()
            .map(|f| (f.rule_id, f.host_id))
            .collect::<Vec<_>>(),
        vec![(rule_id, host_id)]
    );

    // Everything let go: the connection is released and disconnects.
    assert!(keepalive.status().running);
    state.close_all_sessions("main").await;
    state.sftp_close(host_id).await;
    // The forward alone keeps it running; stopping it stops keep-alive.
    assert!(keepalive.status().running);
    state.forward_stop(rule_id).await;
    let deadline = Instant::now() + Duration::from_secs(5);
    while keepalive.status().running {
        assert!(Instant::now() < deadline, "keep-alive still running");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(*driver.0.lock().unwrap(), vec![true, false]);
    let signal = transport.signals.lock().unwrap()[0].clone();
    tokio::time::timeout(Duration::from_secs(5), signal.wait())
        .await
        .expect("idle connection released and closed");

    // The next use connects afresh.
    let (out, closed) = no_op_channels();
    let s3 = state
        .open_session("main", target(), 80, 24, out, closed, no_progress())
        .await
        .expect("shell after release");
    assert_eq!(transport.connects.load(Ordering::SeqCst), 2);
    state.close_session("main", &s3.session_id).await.unwrap();
    let _ = s2;
}

/// I2: deleting a forward rule, a host (its rules go with it) or a group
/// recursively stops the affected running forwards: their listeners close,
/// they leave `forward_statuses`, and an idle host connection is released.
/// Rules of other hosts keep running.
#[tokio::test]
async fn deleting_host_group_or_rule_stops_its_forwards() {
    if !fixture_enabled() {
        return;
    }
    let base = harness(true, || PromptResponse::Accept { save: true });
    // Trust the fixture's key first.
    let (out, closed) = no_op_channels();
    let first = base
        .state
        .open_shell("main", fixture_params(), out, closed, no_progress())
        .await
        .expect("trust the fixture key");
    base.state
        .close_session("main", &first.session_id)
        .await
        .unwrap();
    let transport = Arc::new(CountingTransport::default());
    let state = AppState::new(
        VaultSlot::Ready(base.vault.clone()),
        Arc::new(PromptBridge::new(Arc::new(|_, _| {
            Err("no prompt expected once the key is known".to_string())
        }))),
        Arc::new(NoEvents),
        transport.clone(),
    );
    let upsert = |kind: &'static str, json: serde_json::Value| {
        let state = state.clone();
        async move {
            let v = state.upsert_entity(kind, json).await.unwrap();
            v["id"].as_str().unwrap().parse::<uuid::Uuid>().unwrap()
        }
    };
    let identity = upsert(
        "identity",
        serde_json::json!({"label": "f", "username": "tether", "password": "tether-test"}),
    )
    .await;
    let group = upsert("group", serde_json::json!({"name": "doomed"})).await;
    let mk_host = |label: &str, group: Option<uuid::Uuid>| {
        serde_json::json!({"label": label, "address": "127.0.0.1", "groupId": group,
            "settings": {"port": 2222, "identityId": identity}})
    };
    let in_group = upsert("host", mk_host("in-group", Some(group))).await;
    let alone = upsert("host", mk_host("alone", None)).await;
    let keeper = upsert("host", mk_host("keeper", None)).await;
    let rule = |host: uuid::Uuid, label: &str| {
        serde_json::json!({"hostId": host, "label": label, "kind": "local",
            "bindAddr": "127.0.0.1", "bindPort": 0, "destHost": "127.0.0.1", "destPort": 22})
    };
    let g1 = upsert("forward_rule", rule(in_group, "g1")).await;
    let g2 = upsert("forward_rule", rule(in_group, "g2")).await;
    let a1 = upsert("forward_rule", rule(alone, "a1")).await;
    let k1 = upsert("forward_rule", rule(keeper, "k1")).await;

    // Started in this order: connections 0 (in-group), 1 (alone), 2 (keeper).
    let mut bound = std::collections::HashMap::new();
    for r in [g1, g2, a1, k1] {
        match state.forward_start(r).await.expect("forward_start") {
            core_api::ForwardStatus::Active { bound: addr } => {
                bound.insert(r, addr);
            }
            other => panic!("rule {r}: {other:?}"),
        }
    }
    assert_eq!(
        transport.connects.load(std::sync::atomic::Ordering::SeqCst),
        3
    );
    let signals = transport.signals.lock().unwrap().clone();
    let listening = |r: uuid::Uuid| {
        let addr = bound[&r];
        async move { tokio::net::TcpStream::connect(addr).await.is_ok() }
    };
    let running = |state: &AppState| {
        let mut ids: Vec<uuid::Uuid> = state
            .forward_statuses()
            .into_iter()
            .filter(|e| e.status.is_running())
            .map(|e| e.rule_id)
            .collect();
        ids.sort();
        ids
    };
    let sorted = |mut v: Vec<uuid::Uuid>| {
        v.sort();
        v
    };
    for r in [g1, g2, a1, k1] {
        assert!(listening(r).await, "rule {r} not listening");
    }

    // A rule.
    state
        .delete_entity("forward_rule", g1)
        .await
        .expect("delete rule");
    assert!(!listening(g1).await, "deleted rule still listening");
    assert_eq!(running(&state), sorted(vec![g2, a1, k1]));

    // A host: its rule stops and its now idle connection closes.
    state
        .delete_entity("host", alone)
        .await
        .expect("delete host");
    assert!(!listening(a1).await, "deleted host's rule still listening");
    assert_eq!(running(&state), sorted(vec![g2, k1]));
    tokio::time::timeout(Duration::from_secs(5), signals[1].clone().wait())
        .await
        .expect("deleted host's connection released");

    // A group, recursively.
    state
        .delete_group(
            group,
            tether_app_lib::vault_cmds::ChildPolicyDto::DeleteRecursive,
        )
        .await
        .expect("delete group");
    assert!(!listening(g2).await, "deleted group's rule still listening");
    assert_eq!(running(&state), vec![k1]);
    tokio::time::timeout(Duration::from_secs(5), signals[0].clone().wait())
        .await
        .expect("deleted group's host connection released");

    // The other host's forward is untouched.
    assert!(listening(k1).await);
    assert!(!signals[2].is_closed());
    state.forward_stop(k1).await;
}

/// Connects, then fills the connection's session limit (sshd `MaxSessions`
/// = 10) with SFTP channels it holds, so the next channel open fails with a
/// non-`closed` error.
#[derive(Default)]
struct SaturatingTransport {
    held: Mutex<Vec<core_ssh::SftpChannel>>,
    signals: Mutex<Vec<core_ssh::ClosedSignal>>,
}

#[async_trait::async_trait]
impl core_ssh::SshTransport for SaturatingTransport {
    async fn open_shell(
        &self,
        p: core_ssh::ConnectParams,
        v: Arc<dyn core_ssh::HostKeyVerifier>,
    ) -> Result<core_ssh::ShellHandle, core_ssh::SshError> {
        core_ssh::RusshTransport.open_shell(p, v).await
    }

    async fn connect(
        &self,
        p: &core_ssh::ConnectParams,
        v: Arc<dyn core_ssh::HostKeyVerifier>,
        prompter: Arc<dyn core_ssh::AuthPrompter>,
    ) -> Result<core_ssh::SshConnection, core_ssh::SshError> {
        let conn = core_ssh::RusshTransport.connect(p, v, prompter).await?;
        for _ in 0..10 {
            let channel = conn.open_sftp().await?;
            self.held.lock().unwrap().push(channel);
        }
        self.signals.lock().unwrap().push(conn.close_signal());
        Ok(conn)
    }
}

/// A connection whose first channel fails (here: the server refuses more
/// sessions) is not kept pooled: once nothing else holds it, it closes.
#[tokio::test]
async fn failed_first_channel_releases_the_pooled_connection() {
    if !fixture_enabled() {
        return;
    }
    let base = harness(true, || PromptResponse::Accept { save: true });
    // Trust the fixture key first.
    let (out, closed) = no_op_channels();
    let first = base
        .state
        .open_shell("main", fixture_params(), out, closed, no_progress())
        .await
        .expect("trust the fixture key");
    base.state
        .close_session("main", &first.session_id)
        .await
        .unwrap();

    let transport = Arc::new(SaturatingTransport::default());
    let state = AppState::new(
        VaultSlot::Ready(base.vault.clone()),
        Arc::new(PromptBridge::new(Arc::new(|_, _| {
            Err("no prompt expected".to_string())
        }))),
        Arc::new(NoEvents),
        transport.clone(),
    );
    let identity = state
        .upsert_entity(
            "identity",
            serde_json::json!({"label": "f", "username": "tether", "password": "tether-test"}),
        )
        .await
        .unwrap();
    let host = state
        .upsert_entity(
            "host",
            serde_json::json!({"label": "full", "address": "127.0.0.1",
                "settings": {"port": 2222, "identityId": identity["id"]}}),
        )
        .await
        .unwrap();
    let target = serde_json::from_value::<tether_app_lib::commands::SessionTarget>(
        serde_json::json!({"kind": "host", "hostId": host["id"]}),
    )
    .unwrap();
    let (out, closed) = no_op_channels();
    let err = state
        .open_session("main", target, 80, 24, out, closed, no_progress())
        .await
        .unwrap_err();
    assert_eq!(err.code, "channel", "{err}");
    assert!(state.active_work("main").sessions.is_empty());

    // Only the transport's channels hold the connection now; drop them and
    // it must close (it would stay open if the pool still held it).
    let signal = transport.signals.lock().unwrap()[0].clone();
    transport.held.lock().unwrap().clear();
    tokio::time::timeout(Duration::from_secs(5), signal.wait())
        .await
        .expect("pooled connection released after the failed channel");
}

/// Ruling R31: an imported `~/.ssh/config` host without `User` (so without
/// an identity) connects as the local login name with keyboard-interactive,
/// which asks for the password through the bridge. The fixture only knows
/// user `tether`, so with any other login name the prompted password is
/// refused by the server (`authFailed`), never `noIdentity` before trying.
#[tokio::test]
async fn imported_host_without_user_uses_login_name_and_prompts() {
    if !fixture_enabled() {
        return;
    }
    let h = harness_answering(
        true,
        Arc::new(|p: &Prompt| match p {
            Prompt::HostKeyUnknown { .. } => PromptResponse::Accept { save: true },
            Prompt::Password { .. } => PromptResponse::Secret(SecretString::from("tether-test")),
            Prompt::KeyboardInteractive { prompts, .. } => PromptResponse::Answers(
                prompts
                    .iter()
                    .map(|_| SecretString::from("tether-test"))
                    .collect(),
            ),
            _ => PromptResponse::Reject,
        }),
    );
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("config");
    std::fs::write(&cfg, "Host no-user\n  HostName 127.0.0.1\n  Port 2222\n").unwrap();
    let report = h.state.import_ssh_config(cfg).await.expect("import");
    assert_eq!((report.hosts_added, report.identities_added), (1, 0));
    let hosts = h.state.list_entities("host").await.unwrap();
    let host = hosts
        .iter()
        .find(|h| h["label"] == "no-user")
        .expect("imported host");
    let host_id: uuid::Uuid = host["id"].as_str().unwrap().parse().unwrap();
    let login = tether_app_lib::vault_cmds::local_login_name().expect("USER set");
    assert_eq!(
        h.state
            .connect_params(host_id, 80, 24)
            .await
            .unwrap()
            .username,
        login
    );

    let target = serde_json::from_value::<tether_app_lib::commands::SessionTarget>(
        serde_json::json!({"kind": "host", "hostId": host_id}),
    )
    .unwrap();
    let (out, closed) = no_op_channels();
    let result = h
        .state
        .open_session("main", target, 80, 24, out, closed, no_progress())
        .await;
    let prompts = h.prompts.lock().unwrap().clone();
    assert!(
        matches!(prompts.first(), Some(Prompt::HostKeyUnknown { .. })),
        "{prompts:?}"
    );
    let asked = prompts.iter().any(|p| match p {
        Prompt::Password { user, .. } => *user == login,
        Prompt::KeyboardInteractive { .. } => true,
        _ => false,
    });
    assert!(asked, "no password prompt: {prompts:?}");
    if login == "tether" {
        result.expect("shell as the local login name");
        h.state.close_all_sessions("main").await;
    } else {
        assert_eq!(result.unwrap_err().code, "authFailed");
    }
}

/// The remote shell exiting is a normal end (`reason: null`) that carries
/// the exit status the server reported.
#[tokio::test]
async fn remote_exit_reports_exit_status() {
    if !fixture_enabled() {
        return;
    }
    let h = harness(true, || PromptResponse::Accept { save: true });
    let cap = Arc::new(Mutex::new(Captured::default()));
    let (on_output, on_closed) = capture_channels(&cap);
    let opened = h
        .state
        .open_shell(
            "main",
            fixture_params(),
            on_output,
            on_closed,
            no_progress(),
        )
        .await
        .expect("open_shell");
    h.state
        .write_session("main", &opened.session_id, b"exit 7\n".to_vec())
        .await
        .expect("write_session");
    wait_for(&cap, "closed notification", |c| !c.closed.is_empty()).await;
    let c = cap.lock().unwrap();
    assert_eq!(
        c.closed,
        vec![serde_json::json!({"reason": null, "exitStatus": 7})]
    );
}

/// Phase 1 acceptance (automated part of "connect via an imported
/// ssh_config host"): an `~/.ssh/config`-style file naming the fixture and
/// its key file is imported, and a shell on the imported host authenticates
/// with the imported key (no password anywhere). The unknown host key is
/// prompted for once.
#[tokio::test]
async fn imported_ssh_config_host_connects_with_its_key() {
    if !fixture_enabled() {
        return;
    }
    let h = harness(true, || PromptResponse::Accept { save: true });
    let key = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../tests/fixtures/keys/id_ed25519_test")
        .canonicalize()
        .expect("fixture key");
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("config");
    std::fs::write(
        &cfg,
        format!(
            "Host tether-fixture\n  HostName 127.0.0.1\n  Port 2222\n  User tether\n  IdentityFile {}\n",
            key.display()
        ),
    )
    .unwrap();
    let report = h.state.import_ssh_config(cfg).await.expect("import");
    assert_eq!(
        (
            report.hosts_added,
            report.identities_added,
            report.keys_added
        ),
        (1, 1, 1)
    );
    assert!(report.key_failures.is_empty(), "{:?}", report.key_failures);
    let hosts = h.state.list_entities("host").await.unwrap();
    let host = hosts
        .iter()
        .find(|h| h["label"] == "tether-fixture")
        .expect("imported host");
    let identities = h.state.list_entities("identity").await.unwrap();
    assert!(
        identities
            .iter()
            .all(|i| i["hasPassword"] == false && !i["keyId"].is_null()),
        "{identities:?}"
    );
    let target = serde_json::from_value::<tether_app_lib::commands::SessionTarget>(
        serde_json::json!({"kind": "host", "hostId": host["id"]}),
    )
    .unwrap();
    let cap = Arc::new(Mutex::new(Captured::default()));
    let (on_output, on_closed) = capture_channels(&cap);
    let opened = h
        .state
        .open_session("main", target, 80, 24, on_output, on_closed, no_progress())
        .await
        .expect("shell on the imported host");
    h.state
        .write_session(
            "main",
            &opened.session_id,
            b"echo imported-$((6*7))\n".to_vec(),
        )
        .await
        .unwrap();
    wait_for(&cap, "shell output", |c| c.text().contains("imported-42")).await;
    let prompts = h.prompts.lock().unwrap().clone();
    assert!(
        matches!(prompts.as_slice(), [Prompt::HostKeyUnknown { .. }]),
        "{prompts:?}"
    );
    h.state.close_all_sessions("main").await;
}

/// A password typed at the prompt with "save" checked is stored on the
/// host's identity only once the login succeeded; the next connection then
/// asks nothing.
#[tokio::test]
async fn saved_only_after_success() {
    if !fixture_enabled() {
        return;
    }
    use std::sync::atomic::{AtomicUsize, Ordering};
    static PASSWORD_PROMPTS: AtomicUsize = AtomicUsize::new(0);
    let h = harness_answering(
        true,
        Arc::new(|p: &Prompt| match p {
            Prompt::HostKeyUnknown { .. } => PromptResponse::Accept { save: true },
            // First a wrong password, then the right one; both "save".
            Prompt::Password { .. } => {
                let n = PASSWORD_PROMPTS.fetch_add(1, Ordering::SeqCst);
                let pw = if n == 0 {
                    "wrong-password"
                } else {
                    "tether-test"
                };
                PromptResponse::SaveSecret(SecretString::from(pw))
            }
            _ => PromptResponse::Reject,
        }),
    );
    let identity = h
        .state
        .upsert_entity(
            "identity",
            serde_json::json!({"label": "fixture", "username": "tether"}),
        )
        .await
        .unwrap();
    let identity_id: uuid::Uuid = identity["id"].as_str().unwrap().parse().unwrap();
    let host = h
        .state
        .upsert_entity(
            "host",
            serde_json::json!({"label": "fixture", "address": "127.0.0.1",
                "settings": {"port": 2222, "identityId": identity["id"]}}),
        )
        .await
        .unwrap();
    let host_id: uuid::Uuid = host["id"].as_str().unwrap().parse().unwrap();
    let target = || {
        serde_json::from_value::<tether_app_lib::commands::SessionTarget>(
            serde_json::json!({"kind": "host", "hostId": host_id}),
        )
        .unwrap()
    };
    let stored_password = || {
        let v = h.vault.lock().unwrap();
        v.get::<core_model::Identity>(identity_id)
            .unwrap()
            .unwrap()
            .password
            .map(|p| secrecy::ExposeSecret::expose_secret(&p).to_owned())
    };

    let (out, closed) = no_op_channels();
    let err = h
        .state
        .open_session("main", target(), 80, 24, out, closed, no_progress())
        .await
        .unwrap_err();
    assert_eq!(err.code, "authFailed");
    assert_eq!(stored_password(), None, "a refused password is not saved");
    let offered = h.prompts.lock().unwrap().iter().any(|p| {
        matches!(p, Prompt::Password { save_to: Some(t), .. }
            if t.identity_label == "fixture" && !t.is_new)
    });
    assert!(offered, "{:?}", h.prompts.lock().unwrap());

    let (out, closed) = no_op_channels();
    let opened = h
        .state
        .open_session("main", target(), 80, 24, out, closed, no_progress())
        .await
        .expect("right password");
    assert_eq!(stored_password().as_deref(), Some("tether-test"));
    h.state
        .close_session("main", &opened.session_id)
        .await
        .unwrap();

    // A fresh app on the same vault (so a new connection) asks nothing.
    let fresh = AppState::new(
        VaultSlot::Ready(h.vault.clone()),
        Arc::new(PromptBridge::new(Arc::new(|_, _| {
            Err("no prompt expected with a saved password".to_string())
        }))),
        Arc::new(NoEvents),
        Arc::new(core_ssh::RusshTransport),
    );
    let (out, closed) = no_op_channels();
    let again = fresh
        .open_session("main", target(), 80, 24, out, closed, no_progress())
        .await
        .expect("saved password");
    fresh
        .close_session("main", &again.session_id)
        .await
        .unwrap();
}

/// A vault host for the fixture (password saved on its identity).
async fn fixture_host(state: &AppState) -> tether_app_lib::commands::SessionTarget {
    let identity = state
        .upsert_entity(
            "identity",
            serde_json::json!({"label": "fixture", "username": "tether", "password": "tether-test"}),
        )
        .await
        .unwrap();
    let host = state
        .upsert_entity(
            "host",
            serde_json::json!({"label": "fixture", "address": "127.0.0.1",
                "settings": {"port": 2222, "identityId": identity["id"]}}),
        )
        .await
        .unwrap();
    host_target(&host["id"])
}

fn host_target(host_id: &serde_json::Value) -> tether_app_lib::commands::SessionTarget {
    serde_json::from_value(serde_json::json!({"kind": "host", "hostId": host_id})).unwrap()
}

/// A new connection reports every connect step, then the shell.
#[tokio::test]
async fn open_session_sends_steps_for_a_new_connection() {
    if !fixture_enabled() {
        return;
    }
    let h = harness(true, || PromptResponse::Accept { save: true });
    let target = fixture_host(&h.state).await;
    let (out, closed) = no_op_channels();
    let (progress, steps) = recording_progress();
    let opened = h
        .state
        .open_session("main", target, 80, 24, out, closed, progress)
        .await
        .expect("open_session");
    assert_eq!(
        *steps.lock().unwrap(),
        ["tcp", "hostKey", "auth", "shell"],
        "steps of a new connection"
    );
    h.state
        .close_session("main", &opened.session_id)
        .await
        .unwrap();
}

/// A second shell on a live connection connects nothing: only the shell step.
#[tokio::test]
async fn open_session_on_pooled_connection_sends_only_shell() {
    if !fixture_enabled() {
        return;
    }
    let h = harness(true, || PromptResponse::Accept { save: true });
    let first_target = fixture_host(&h.state).await;
    let host_id = match &first_target {
        tether_app_lib::commands::SessionTarget::Host { host_id, .. } => serde_json::json!(host_id),
        tether_app_lib::commands::SessionTarget::Local { .. } => unreachable!(),
    };
    let (out, closed) = no_op_channels();
    h.state
        .open_session("main", first_target, 80, 24, out, closed, no_progress())
        .await
        .expect("first shell");
    let (out, closed) = no_op_channels();
    let (progress, steps) = recording_progress();
    h.state
        .open_session("main", host_target(&host_id), 80, 24, out, closed, progress)
        .await
        .expect("second shell");
    assert_eq!(*steps.lock().unwrap(), ["shell"], "pooled connection");
    h.state.close_all_sessions("main").await;
}

/// Writes `text` to a session as keystrokes.
async fn type_into(state: &AppState, id: &str, text: &str) {
    state
        .write_session("main", id, text.as_bytes().to_vec())
        .await
        .expect("write");
}

/// `session_tmux` names the tmux session a host tab is in (`None` for a
/// plain tab), and a tab opened with `reattachTmux` lands in that session
/// and is tracked again.
#[tokio::test]
async fn session_tmux_names_each_tabs_session_and_reattaches() {
    if !fixture_enabled() {
        return;
    }
    let h = harness(true, || PromptResponse::Accept { save: true });
    let target = fixture_host(&h.state).await;
    let host_id = match &target {
        tether_app_lib::commands::SessionTarget::Host { host_id, .. } => serde_json::json!(host_id),
        tether_app_lib::commands::SessionTarget::Local { .. } => unreachable!(),
    };
    let in_tmux = Arc::new(Mutex::new(Captured::default()));
    let (out, closed) = capture_channels(&in_tmux);
    let t1 = h
        .state
        .open_session("main", target, 80, 24, out, closed, no_progress())
        .await
        .expect("tab 1")
        .session_id;
    let plain = Arc::new(Mutex::new(Captured::default()));
    let (out, closed) = capture_channels(&plain);
    let t2 = h
        .state
        .open_session(
            "main",
            host_target(&host_id),
            80,
            24,
            out,
            closed,
            no_progress(),
        )
        .await
        .expect("tab 2")
        .session_id;

    type_into(&h.state, &t1, "tmux new -s tapp1\r").await;
    // tmux's status line names the session once the client is up.
    wait_for(&in_tmux, "the tmux status line", |c| {
        c.text().contains("[tapp1]")
    })
    .await;
    assert_eq!(
        h.state.session_tmux("main", &t1).await.unwrap().as_deref(),
        Some("tapp1")
    );
    assert_eq!(h.state.session_tmux("main", &t2).await.unwrap(), None);
    let other = h.state.session_tmux("other", &t1).await.unwrap_err();
    assert_eq!(other.code, "notOwner");

    // A new tab told to reattach lands in the session, with its own tab id.
    let back = Arc::new(Mutex::new(Captured::default()));
    let (out, closed) = capture_channels(&back);
    let reattach: tether_app_lib::commands::SessionTarget = serde_json::from_value(
        serde_json::json!({"kind": "host", "hostId": host_id, "reattachTmux": "tapp1"}),
    )
    .unwrap();
    let t3 = h
        .state
        .open_session("main", reattach, 80, 24, out, closed, no_progress())
        .await
        .expect("tab 3")
        .session_id;
    type_into(&h.state, &t3, "tmux display -p 'in=#S'\r").await;
    wait_for(&back, "the reattached session", |c| {
        c.text().contains("in=tapp1")
    })
    .await;
    assert_eq!(
        h.state.session_tmux("main", &t3).await.unwrap().as_deref(),
        Some("tapp1")
    );

    type_into(&h.state, &t2, "tmux kill-session -t tapp1\r").await;
    wait_for(&back, "tab 3 to leave the killed session", |c| {
        c.text().contains("[exited]")
    })
    .await;
    h.state.close_all_sessions("main").await;
}
