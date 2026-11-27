//! The Phase 1 command layer, driven through `AppState` (the code every
//! `#[tauri::command]` wrapper calls) without a running Tauri app.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use core_api::vault::{FakeGate, GateKind, MemoryKeyStore, SharedVault, VaultService};
use core_api::{
    ForwardEvent, ForwardStatus, OutputSink, Prompt, PromptBridge, ShellTarget, TransferEvent,
};
use core_ssh::{
    AuthMethod, AuthPrompter, ConnectParams, ConnectProgress, ConnectStep, ControlMsg, HostKeyInfo,
    HostKeyVerifier, NoPrompter, ShellControl, ShellHandle, SshConnection, SshError, SshTransport,
};
use core_sync::MemoryTokenStore;
use secrecy::{ExposeSecret, SecretString};
use serde_json::{Value, json};
use tauri::http::{HeaderMap, HeaderValue};
use tauri::ipc::{Channel, InvokeBody, InvokeResponseBody};
use tempfile::TempDir;
use tether_app_lib::autolock::Moment;
use tether_app_lib::commands::{
    ActiveSession, ActiveTransfer, ActiveWork, OpenedSession, SESSION_ID_HEADER, SessionMeta,
    write_args, write_args_for,
};
use tether_app_lib::forward_cmds::ForwardStatusEntry;
use tether_app_lib::keepalive::{KeepAliveStatus, LocationDriver, Permission};
use tether_app_lib::prompt_cmds::{PromptEvent, PromptWithdrawn};
use tether_app_lib::sftp_cmds::{
    EnqueuedTransfer, LocalEntryDto, RemoteEntryDto, TransferEventPayload,
};
use tether_app_lib::state::{AppState, CmdError, EventSink, VaultSlot};
use tether_app_lib::sync_cmds::{
    CodeInfoDto, ConflictDto, ConflictSideDto, DeviceDto, EnableFailure, EnableOutcome,
    JoinOutcome, LinkCodeDto, RecoverOutcome, RecoveryInput, ResolutionDto, SyncAccountDto,
    SyncErrorKindDto, SyncStateDto, SyncStatusDto,
};
use tether_app_lib::vault_cmds::{
    ChildPolicyDto, ConnSettingsDto, EffectiveSettingsDto, GroupDto, HostDto, IdentityDto,
    ImportReportDto, KeyAlgoDto, KeyExport, KeyFailureDto, KnownHostDto, KnownHostsImportReport,
    LockReason, ResolvedDto, SkippedEntryDto, SnippetDto, SourceDto, SshKeyDto, TagDto,
    VaultCreated, VaultLocked, VaultState, VaultStatus,
};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Harness

/// Records every event the state emits.
#[derive(Default)]
struct Recorder(Mutex<Vec<(String, Value)>>);

impl EventSink for Recorder {
    fn emit(&self, event: &str, payload: Value) -> Result<(), String> {
        self.0.lock().unwrap().push((event.to_owned(), payload));
        Ok(())
    }
}

impl Recorder {
    fn names(&self) -> Vec<String> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .map(|(n, _)| n.clone())
            .collect()
    }
}

/// A transport whose shells are in-memory pipes; it keeps every shell's
/// control receiver so a shell stays open until the session closes it.
#[derive(Default)]
struct FakeTransport {
    shells: Mutex<
        Vec<(
            tokio::sync::mpsc::Sender<Bytes>,
            tokio::sync::mpsc::Receiver<ControlMsg>,
        )>,
    >,
    opened: AtomicUsize,
    /// Reported by `connect_with_progress`, which then fails.
    connect_steps: Vec<ConnectStep>,
}

#[async_trait]
impl SshTransport for FakeTransport {
    async fn open_shell(
        &self,
        _p: ConnectParams,
        _v: Arc<dyn HostKeyVerifier>,
    ) -> Result<ShellHandle, SshError> {
        let (out_tx, out_rx) = tokio::sync::mpsc::channel(4);
        let (ctl_tx, ctl_rx) = tokio::sync::mpsc::channel(4);
        self.shells.lock().unwrap().push((out_tx, ctl_rx));
        self.opened.fetch_add(1, Ordering::SeqCst);
        Ok(ShellHandle::from_parts(
            out_rx,
            ShellControl::from_sender(ctl_tx),
        ))
    }

    /// Reports `connect_steps`, then refuses (a fake cannot make an
    /// `SshConnection`; the success path runs against the sshd fixture).
    async fn connect_with_progress(
        &self,
        _p: &ConnectParams,
        _v: Arc<dyn HostKeyVerifier>,
        _prompter: Arc<dyn AuthPrompter>,
        progress: Arc<dyn ConnectProgress>,
    ) -> Result<SshConnection, SshError> {
        for step in &self.connect_steps {
            progress.step(*step);
        }
        Err(SshError::Channel("fake refused".into()))
    }
}

/// A sink that records what it is told.
#[derive(Default)]
struct TestSink {
    frames: Mutex<Vec<Vec<u8>>>,
    closed: Mutex<Vec<Option<String>>>,
}

impl OutputSink for TestSink {
    fn frame(&self, bytes: Vec<u8>) {
        self.frames.lock().unwrap().push(bytes);
    }
    fn closed(&self, reason: Option<String>, _exit_status: Option<u32>) {
        self.closed.lock().unwrap().push(reason);
    }
}

struct Harness {
    state: AppState,
    vault: SharedVault,
    events: Arc<Recorder>,
    transport: Arc<FakeTransport>,
    _dir: TempDir,
}

/// An `AppState` over a fresh (not yet created) cheap-KDF vault and a fake
/// transport.
fn harness() -> Harness {
    harness_with(FakeTransport::default())
}

fn harness_with(transport: FakeTransport) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let vault = VaultService::open_for_tests(
        &dir.path().join("vault.sqlite"),
        Box::new(MemoryKeyStore::default()),
    )
    .unwrap();
    let vault: SharedVault = Arc::new(Mutex::new(vault));
    let events = Arc::new(Recorder::default());
    let transport = Arc::new(transport);
    let prompts = Arc::new(PromptBridge::new(Arc::new(|_, _| Ok(()))));
    let tokens = Arc::new(MemoryTokenStore::new());
    let state = AppState::with_token_store(
        VaultSlot::Ready(vault.clone()),
        prompts,
        events.clone(),
        transport.clone(),
        // In memory: tests never touch the real keychain.
        tokens,
    );
    Harness {
        state,
        vault,
        events,
        transport,
        _dir: dir,
    }
}

fn secret(s: &str) -> SecretString {
    SecretString::from(s)
}

fn fake_ssh_target() -> ShellTarget {
    ShellTarget::Ssh {
        params: ConnectParams {
            host: "fake.test".into(),
            port: 22,
            username: "u".into(),
            auth: AuthMethod::KeyboardInteractive,
            term: "xterm-256color".into(),
            cols: 80,
            rows: 24,
            connect_timeout: Duration::from_secs(5),
            keepalive: None,
            env: Vec::new(),
        },
        prompter: Arc::new(NoPrompter),
    }
}

fn meta(title: &str) -> SessionMeta {
    SessionMeta {
        title: title.into(),
        host_id: None,
        tab: None,
    }
}

async fn open_fake(h: &Harness, owner: &str, title: &str) -> (String, Arc<TestSink>) {
    let sink = Arc::new(TestSink::default());
    let id = h
        .state
        .open_target(owner, meta(title), fake_ssh_target(), {
            let sink = sink.clone();
            move |_reaper| sink as Arc<dyn OutputSink>
        })
        .await
        .expect("open fake session");
    (id, sink)
}

async fn eventually(what: &str, f: impl Fn() -> bool) {
    for _ in 0..200 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {what}");
}

/// A password made at run time, for tests of the re-authentication: a
/// literal would trip CodeQL's hard-coded credential rule.
fn fresh_password() -> String {
    core_model::Id::now_v7().to_string()
}

/// [`created`] with a fresh master password, returned.
async fn created_fresh(h: &Harness) -> String {
    let master = fresh_password();
    h.state.vault_create(secret(&master)).await.unwrap();
    master
}

async fn created(h: &Harness) -> VaultCreated {
    h.state.vault_create(secret("master-pw")).await.unwrap()
}

// ---------------------------------------------------------------------------
// DTO field names vs apps/tauri/src/api/bindings.json

const BINDINGS: &str = include_str!("../../src/api/bindings.json");

fn bindings() -> BTreeMap<String, BTreeSet<String>> {
    serde_json::from_str::<BTreeMap<String, Vec<String>>>(BINDINGS)
        .expect("bindings.json parses")
        .into_iter()
        .map(|(k, v)| (k, v.into_iter().collect()))
        .collect()
}

fn keys(v: &Value) -> BTreeSet<String> {
    v.as_object()
        .unwrap_or_else(|| panic!("not an object: {v}"))
        .keys()
        .cloned()
        .collect()
}

fn uid() -> Uuid {
    Uuid::now_v7()
}

fn settings_all_set() -> ConnSettingsDto {
    ConnSettingsDto {
        port: Some(22),
        identity_id: Some(uid()),
        keepalive_secs: Some(30),
        term_type: Some("xterm".into()),
        charset: Some("UTF-8".into()),
        color_scheme: Some("Tether Dark".into()),
        font_family: Some("Menlo".into()),
        font_size: Some(13),
        env: Some(vec![("A".into(), "b".into())]),
    }
}

fn resolved<T>(value: T) -> ResolvedDto<T> {
    ResolvedDto {
        value,
        source: SourceDto::Host,
    }
}

fn save_target() -> core_api::SaveTarget {
    core_api::SaveTarget {
        identity_label: "alice".into(),
        is_new: false,
    }
}

fn host_key() -> HostKeyInfo {
    HostKeyInfo {
        algorithm: "ssh-ed25519".into(),
        sha256_fingerprint: "SHA256:x".into(),
        openssh: "ssh-ed25519 AAAA".into(),
    }
}

/// Every output DTO, serialized with all optional fields set, under its
/// bindings.json name.
fn output_samples() -> Vec<(&'static str, Value)> {
    let to = |v: &dyn erased::Ser| v.json();
    let job = || core_api::JobId::from("j1".to_owned());
    vec![
        (
            "VaultStatus",
            to(&VaultStatus {
                state: VaultState::Unavailable,
                quick_unlock_enabled: false,
                quick_unlock_kind: GateKind::TouchId,
                error: Some("e".into()),
            }),
        ),
        (
            "VaultCreated",
            to(&VaultCreated {
                recovery_key: "r".into(),
            }),
        ),
        ("ConnSettings", to(&settings_all_set())),
        (
            "Group",
            to(&GroupDto {
                id: uid(),
                parent_id: Some(uid()),
                name: "g".into(),
                settings: settings_all_set(),
            }),
        ),
        (
            "Host",
            to(&HostDto {
                id: uid(),
                group_id: Some(uid()),
                label: "h".into(),
                address: "a".into(),
                tag_ids: vec![uid()],
                settings: settings_all_set(),
                forward_rule_ids: vec![uid()],
            }),
        ),
        (
            "Tag",
            to(&TagDto {
                id: uid(),
                name: "t".into(),
                color: "#fff".into(),
            }),
        ),
        (
            "Snippet",
            to(&SnippetDto {
                id: uid(),
                label: "s".into(),
                text: "df -h".into(),
                run: true,
            }),
        ),
        (
            "Identity",
            to(&IdentityDto {
                id: uid(),
                label: "i".into(),
                username: "u".into(),
                has_password: true,
                key_id: Some(uid()),
                owner: Some(uid()),
            }),
        ),
        (
            "SshKey",
            to(&SshKeyDto {
                id: uid(),
                label: "k".into(),
                algorithm: "ssh-ed25519".into(),
                public_openssh: "ssh-ed25519 AAAA".into(),
                fingerprint: "SHA256:x".into(),
                has_passphrase: false,
                certificate: Some("c".into()),
            }),
        ),
        (
            "KnownHost",
            to(&KnownHostDto {
                id: uid(),
                host_pattern: "h".into(),
                port: 22,
                algorithm: "ssh-ed25519".into(),
                public_key: "AAAA".into(),
                added_at: 1,
                source: "import".into(),
                fingerprint: "SHA256:x".into(),
            }),
        ),
        (
            "ForwardRule",
            to(&tether_app_lib::vault_cmds::ForwardRuleDto {
                id: uid(),
                host_id: uid(),
                label: "f".into(),
                kind: "local".into(),
                bind_addr: "127.0.0.1".into(),
                bind_port: 8080,
                dest_host: Some("d".into()),
                dest_port: Some(80),
                autostart: false,
            }),
        ),
        (
            "EffectiveSettings",
            to(&EffectiveSettingsDto {
                port: resolved(22),
                identity_id: resolved(Some(uid())),
                keepalive_secs: resolved(Some(30)),
                term_type: resolved(Some("xterm".into())),
                charset: resolved(Some("UTF-8".into())),
                color_scheme: resolved(None),
                font_family: resolved(None),
                font_size: resolved(None),
                env: resolved(None),
            }),
        ),
        ("Resolved", to(&resolved(1))),
        ("Source.default", to(&SourceDto::Default)),
        ("Source.group", to(&SourceDto::Group { group_id: uid() })),
        ("Source.host", to(&SourceDto::Host)),
        (
            "ImportReport",
            to(&ImportReportDto {
                hosts_added: 1,
                identities_added: 1,
                keys_added: 1,
                key_failures: vec![],
                skipped: vec![],
            }),
        ),
        (
            "KeyFailure",
            to(&KeyFailureDto {
                path: "/k".into(),
                needs_passphrase: true,
                message: "m".into(),
            }),
        ),
        (
            "SkippedEntry",
            to(&SkippedEntryDto {
                name: "n".into(),
                reason: "r".into(),
            }),
        ),
        (
            "KnownHostsImportReport",
            to(&KnownHostsImportReport {
                added: 1,
                already_known: 2,
            }),
        ),
        (
            "KeyExport",
            to(&KeyExport {
                private_openssh: "PEM".into(),
            }),
        ),
        (
            "OpenedSession",
            to(&OpenedSession {
                session_id: "s".into(),
            }),
        ),
        (
            "SessionClosed",
            to(&tether_app_lib::SessionClosed {
                reason: Some("r".into()),
                exit_status: Some(1),
            }),
        ),
        (
            "ActiveWork",
            to(&ActiveWork {
                sessions: vec![],
                transfers: vec![],
                forwards: vec![],
            }),
        ),
        (
            "LocalShell",
            to(&tether_app_lib::commands::LocalShellDto {
                id: "cmd".into(),
                label: "Command Prompt".into(),
            }),
        ),
        (
            "ActiveSession",
            to(&ActiveSession {
                id: "s".into(),
                title: "t".into(),
                host_id: Some(uid()),
            }),
        ),
        (
            "ActiveTransfer",
            to(&ActiveTransfer {
                job_id: "j".into(),
                host_id: uid(),
                direction: core_api::Direction::Upload,
                local: "/l".into(),
                remote: "/r".into(),
            }),
        ),
        (
            "RemoteEntry",
            to(&RemoteEntryDto {
                name: "n".into(),
                path: "/n".into(),
                is_dir: false,
                size: 1,
                mtime: 2,
                mode: 0o644,
                is_symlink: false,
            }),
        ),
        (
            "LocalEntry",
            to(&LocalEntryDto {
                name: "n".into(),
                path: "/n".into(),
                is_dir: false,
                size: 1,
                mtime: 2,
                mode: Some(0o644),
                is_symlink: false,
            }),
        ),
        (
            "EnqueuedTransfer",
            to(&EnqueuedTransfer { job_id: "j".into() }),
        ),
        (
            "TransferEventPayload",
            to(&TransferEventPayload {
                host_id: uid(),
                event: TransferEvent::Skipped { job: job() },
            }),
        ),
        (
            "TransferEvent.progress",
            to(&TransferEvent::Progress {
                job: job(),
                bytes: 1,
                total: 2,
            }),
        ),
        (
            "TransferEvent.done",
            to(&TransferEvent::Done {
                job: job(),
                dest: "/d".into(),
            }),
        ),
        (
            "TransferEvent.skipped",
            to(&TransferEvent::Skipped { job: job() }),
        ),
        (
            "TransferEvent.cancelled",
            to(&TransferEvent::Cancelled { job: job() }),
        ),
        (
            "TransferEvent.failed",
            to(&TransferEvent::Failed {
                job: job(),
                error: "e".into(),
            }),
        ),
        (
            "ForwardEvent",
            to(&ForwardEvent {
                rule_id: uid(),
                status: ForwardStatus::Stopped,
            }),
        ),
        (
            "ForwardStatusEntry",
            to(&ForwardStatusEntry {
                rule_id: uid(),
                host_id: uid(),
                status: ForwardStatus::Stopped,
            }),
        ),
        (
            "KeepAliveStatus",
            to(&KeepAliveStatus {
                enabled: true,
                authorized: true,
                running: false,
            }),
        ),
        ("ForwardStatus.starting", to(&ForwardStatus::Starting)),
        (
            "ForwardStatus.active",
            to(&ForwardStatus::Active {
                bound: "127.0.0.1:1".parse().unwrap(),
            }),
        ),
        (
            "ForwardStatus.active.detail",
            to(&ForwardStatus::Active {
                bound: "127.0.0.1:1".parse().unwrap(),
            })["detail"]
                .clone(),
        ),
        ("ForwardStatus.error", to(&ForwardStatus::Error("e".into()))),
        ("ForwardStatus.stopped", to(&ForwardStatus::Stopped)),
        (
            "PromptEvent",
            to(&PromptEvent {
                id: "p".into(),
                prompt: Prompt::Passphrase {
                    key_label: "k".into(),
                },
            }),
        ),
        ("PromptWithdrawn", to(&PromptWithdrawn { id: "p".into() })),
        (
            "Prompt.hostKeyUnknown",
            to(&Prompt::HostKeyUnknown {
                host: "h".into(),
                port: 22,
                key: host_key(),
            }),
        ),
        (
            "Prompt.hostKeyChanged",
            to(&Prompt::HostKeyChanged {
                host: "h".into(),
                port: 22,
                old: host_key(),
                new: host_key(),
            }),
        ),
        (
            "Prompt.passphrase",
            to(&Prompt::Passphrase {
                key_label: "k".into(),
            }),
        ),
        (
            "Prompt.keyboardInteractive",
            to(&Prompt::KeyboardInteractive {
                name: "n".into(),
                instructions: "i".into(),
                prompts: vec![("p".into(), false)],
                save_to: Some(save_target()),
            }),
        ),
        (
            "Prompt.password",
            to(&Prompt::Password {
                user: "u".into(),
                host: "h".into(),
                save_to: Some(save_target()),
            }),
        ),
        (
            "Prompt.transferConflict",
            to(&Prompt::TransferConflict {
                job_id: "j".into(),
                path: "/p".into(),
                existing_size: 1,
                incoming_size: 2,
            }),
        ),
        ("HostKeyInfo", to(&host_key())),
        ("SaveTarget", to(&save_target())),
        ("CmdError", to(&CmdError::referenced("in use", vec![uid()]))),
        (
            "VaultLocked",
            to(&VaultLocked {
                reason: LockReason::PasswordChanged,
            }),
        ),
        (
            "SyncStatus",
            to(&SyncStatusDto {
                account: Some(SyncAccountDto {
                    server: "https://sync.example".into(),
                    username: "alice".into(),
                    device_id: uid(),
                }),
                status: SyncStateDto::Off,
            }),
        ),
        (
            "SyncAccount",
            to(&SyncAccountDto {
                server: "https://sync.example".into(),
                username: "alice".into(),
                device_id: uid(),
            }),
        ),
        ("SyncState.off", to(&SyncStateDto::Off)),
        ("SyncState.idle", to(&SyncStateDto::Idle { last_sync: 5 })),
        (
            "SyncState.syncing",
            to(&SyncStateDto::Syncing {
                done: 1,
                total: Some(2),
            }),
        ),
        (
            "SyncState.offline",
            to(&SyncStateDto::Offline { reason: "r".into() }),
        ),
        (
            "SyncState.error",
            to(&SyncStateDto::Error {
                kind: SyncErrorKindDto::Partial,
                message: "m".into(),
            }),
        ),
        (
            "CodeInfo",
            to(&CodeInfoDto {
                kind: "invite".into(),
                server: "https://sync.example".into(),
                fingerprint: Some("sha256:abcd".into()),
            }),
        ),
        (
            "EnableResult",
            to(&EnableOutcome {
                new_recovery_key: Some("r".into()),
            }),
        ),
        (
            "EnableError",
            to(&EnableFailure {
                code: "storage".into(),
                message: "m".into(),
                refs: vec![],
                new_recovery_key: Some("r".into()),
            }),
        ),
        (
            "JoinResult",
            to(&JoinOutcome {
                backup: Some("/b".into()),
                reconnected: false,
            }),
        ),
        (
            "RecoverResult",
            to(&RecoverOutcome {
                needs_device_code: true,
                server: "https://sync.example".into(),
            }),
        ),
        (
            "LinkCode",
            to(&LinkCodeDto {
                code: "tether1-x".into(),
                server: "https://sync.example".into(),
                expires_at: 7,
            }),
        ),
        (
            "Device",
            to(&DeviceDto {
                id: "d".into(),
                name: "n".into(),
                platform: "p".into(),
                created_at: 1,
                last_seen: 2,
                current: true,
            }),
        ),
        (
            "Conflict",
            to(&ConflictDto {
                id: uid(),
                record_id: uid(),
                kind: "identity".into(),
                current: Some(json!({"password": {"hasSecret": true}})),
                other: None,
                changed_fields: vec!["password".into()],
                detected_at: 3,
            }),
        ),
    ]
}

/// Serialize behind a trait object so the sample list stays one `vec!`.
mod erased {
    pub trait Ser {
        fn json(&self) -> serde_json::Value;
    }
    impl<T: serde::Serialize> Ser for T {
        fn json(&self) -> serde_json::Value {
            serde_json::to_value(self).unwrap()
        }
    }
}

/// Input-only DTOs: a JSON object with exactly the bindings' fields must
/// deserialize (inputs deny unknown fields, so a field Rust lacks fails).
type Decode = fn(Value) -> Result<(), String>;

fn input_samples() -> Vec<(&'static str, Value, Decode)> {
    fn de<T: serde::de::DeserializeOwned>(v: Value) -> Result<(), String> {
        serde_json::from_value::<T>(v)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    use tether_app_lib::commands::{ConnectParamsDto, SessionTarget};
    use tether_app_lib::vault_cmds::{IdentityInput, OwnLoginInput, SshKeyInput};
    vec![
        (
            "OwnLoginInput",
            json!({"ownerId": uid(), "username": "u", "password": "pw"}),
            de::<OwnLoginInput>,
        ),
        (
            "IdentityInput",
            json!({"id": uid(), "label": "l", "username": "u", "password": "pw", "keyId": null}),
            de::<IdentityInput>,
        ),
        (
            "SshKeyInput",
            json!({"id": uid(), "label": "l"}),
            de::<SshKeyInput>,
        ),
        (
            "RecoveryChoice.existing",
            json!({"existing": "key"}),
            de::<RecoveryInput>,
        ),
        (
            "RecoveryChoice.new",
            json!({"new": true}),
            de::<RecoveryInput>,
        ),
        (
            "Resolution.keepCurrent",
            json!({"keepCurrent": true}),
            de::<ResolutionDto>,
        ),
        (
            "Resolution.useOther",
            json!({"useOther": true}),
            de::<ResolutionDto>,
        ),
        (
            "Resolution.fields",
            json!({"fields": ["password"]}),
            de::<ResolutionDto>,
        ),
        (
            "SessionTarget.host",
            json!({"kind": "host", "hostId": uid(), "reattachTmux": "work"}),
            de::<SessionTarget>,
        ),
        (
            "SessionTarget.local",
            json!({"kind": "local", "cwd": "/tmp", "shellId": "cmd"}),
            de::<SessionTarget>,
        ),
        (
            "TransferJob",
            json!({"direction": "upload", "local": "/l", "remote": "/r", "policy": "ask"}),
            de::<core_api::TransferJob>,
        ),
        (
            "ConnectParams",
            json!({"host": "h", "port": 22, "username": "u", "password": "p",
                   "privateKeyPem": "k", "passphrase": "pp", "cols": 80, "rows": 24}),
            de::<ConnectParamsDto>,
        ),
    ]
}

#[test]
fn cmd_dto_roundtrip() {
    let bindings = bindings();
    let mut checked = BTreeSet::new();
    for (name, value) in output_samples() {
        let expected = bindings
            .get(name)
            .unwrap_or_else(|| panic!("bindings.json has no entry for {name}"));
        assert_eq!(&keys(&value), expected, "field names of {name}");
        checked.insert(name.to_owned());
    }
    for (name, value, de) in input_samples() {
        let expected = bindings
            .get(name)
            .unwrap_or_else(|| panic!("bindings.json has no entry for {name}"));
        assert_eq!(
            &keys(&value),
            expected,
            "sample of {name} must match bindings"
        );
        de(value).unwrap_or_else(|e| panic!("{name} does not accept its bindings: {e}"));
        checked.insert(name.to_owned());
    }
    let unchecked: Vec<_> = bindings.keys().filter(|k| !checked.contains(*k)).collect();
    assert!(
        unchecked.is_empty(),
        "bindings without a Rust check: {unchecked:?}"
    );

    // Entity DTOs read back what they write.
    let host = HostDto {
        id: uid(),
        group_id: None,
        label: "ラベル 🚀".into(),
        address: "h".into(),
        tag_ids: vec![],
        settings: settings_all_set(),
        forward_rule_ids: vec![],
    };
    let back: HostDto = serde_json::from_value(serde_json::to_value(&host).unwrap()).unwrap();
    assert_eq!(back, host);

    // An input with an unknown field is refused, and secrets in inputs land
    // in SecretString (Debug never shows them).
    use tether_app_lib::vault_cmds::IdentityInput;
    assert!(
        serde_json::from_value::<IdentityInput>(
            json!({"label": "l", "username": "u", "pasword": "typo"})
        )
        .is_err()
    );
    let input: IdentityInput =
        serde_json::from_value(json!({"label": "l", "username": "u", "password": "hunter2"}))
            .unwrap();
    assert!(!format!("{input:?}").contains("hunter2"));
}

// ---------------------------------------------------------------------------
// Sessions

#[tokio::test]
async fn close_all_sessions_closes_orphans() {
    let h = harness();
    let (m1, s1) = open_fake(&h, "main", "one").await;
    let (m2, s2) = open_fake(&h, "main", "two").await;
    let (o1, so) = open_fake(&h, "other", "three").await;
    assert_eq!(h.transport.opened.load(Ordering::SeqCst), 3);

    let mut main_ids = h.state.session_ids("main");
    main_ids.sort();
    let mut expected = vec![m1.clone(), m2.clone()];
    expected.sort();
    assert_eq!(main_ids, expected);

    let closed = h.state.close_all_sessions("main").await;
    assert_eq!(closed, 2);

    assert_eq!(*s1.closed.lock().unwrap(), vec![None]);
    assert_eq!(*s2.closed.lock().unwrap(), vec![None]);
    assert!(h.state.session_ids("main").is_empty());
    assert!(
        h.state
            .write_session("main", &m1, b"x".to_vec())
            .await
            .is_err()
    );
    assert!(
        h.state
            .write_session("main", &m2, b"x".to_vec())
            .await
            .is_err()
    );

    // The other webview's session is untouched and still works.
    assert!(so.closed.lock().unwrap().is_empty());
    assert_eq!(h.state.session_ids("other"), vec![o1.clone()]);
    // Owner check: `main` may not touch `other`'s session.
    let err = h
        .state
        .write_session("main", &o1, b"x".to_vec())
        .await
        .unwrap_err();
    assert_eq!(err.code, "notOwner");
    assert_eq!(
        h.state
            .resize_session("main", &o1, 100, 30)
            .await
            .unwrap_err()
            .code,
        "notOwner"
    );
    assert_eq!(
        h.state.ack_session("main", &o1, 10).unwrap_err().code,
        "notOwner"
    );
    assert_eq!(
        h.state.close_session("main", &o1).await.unwrap_err().code,
        "notOwner"
    );
    assert!(so.closed.lock().unwrap().is_empty());
    h.state
        .write_session("other", &o1, b"ls\n".to_vec())
        .await
        .unwrap();
    h.state.resize_session("other", &o1, 100, 30).await.unwrap();
    h.state.ack_session("other", &o1, 10).unwrap();
    h.state.close_session("other", &o1).await.unwrap();
    assert_eq!(*so.closed.lock().unwrap(), vec![None]);
    assert!(h.state.session_ids("other").is_empty());
}

/// A session that ends by itself (remote EOF) leaves the registry too.
#[tokio::test]
async fn session_ending_on_its_own_is_forgotten() {
    let h = harness();
    let (id, sink) = open_fake(&h, "main", "one").await;
    // Dropping the fake shell's output sender is EOF.
    h.transport.shells.lock().unwrap().clear();
    eventually("closed", || !sink.closed.lock().unwrap().is_empty()).await;
    eventually("deregistered", || h.state.session_ids("main").is_empty()).await;
    assert!(
        h.state
            .active_work("main")
            .sessions
            .iter()
            .all(|s| s.id != id)
    );
}

/// Backstop: when the webview's output channel is gone (send fails), the
/// session is closed instead of running on unseen.
#[tokio::test]
async fn output_send_failure_closes_session() {
    let h = harness();
    let closed_seen = Arc::new(AtomicUsize::new(0));
    let output = Channel::<InvokeResponseBody>::new(|_| Err(tauri::Error::WebviewNotFound));
    let on_closed = Channel::<tether_app_lib::SessionClosed>::new({
        let closed_seen = closed_seen.clone();
        move |_| {
            closed_seen.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    });
    let id = h
        .state
        .open_target("main", meta("doomed"), fake_ssh_target(), move |reaper| {
            Arc::new(tether_app_lib::ChannelSink::new(output, on_closed).on_dead(reaper))
                as Arc<dyn OutputSink>
        })
        .await
        .unwrap();
    // Output arrives; delivering it fails; the session gets closed.
    let tx = h.transport.shells.lock().unwrap()[0].0.clone();
    tx.send(Bytes::from_static(b"hello")).await.unwrap();
    eventually("session reaped", || h.state.session_ids("main").is_empty()).await;
    eventually("closed notified", || {
        closed_seen.load(Ordering::SeqCst) == 1
    })
    .await;
    assert!(
        h.state
            .write_session("main", &id, b"x".to_vec())
            .await
            .is_err()
    );
}

/// Records keep-alive's start/stop calls; permission is set by the test.
#[derive(Default)]
struct RecordingDriver(Mutex<Vec<bool>>);

impl LocationDriver for RecordingDriver {
    fn request_permission(&self) {}
    fn set_updating(&self, on: bool, _generation: u64) {
        self.0.lock().unwrap().push(on);
    }
}

/// Keep-alive runs while any session is open, whoever owns it, and stops
/// when the last one closes (by the user or by itself).
#[tokio::test]
async fn keepalive_follows_the_open_sessions() {
    let h = harness();
    let driver = Arc::new(RecordingDriver::default());
    let keepalive = h.state.keepalive();
    keepalive.install(driver.clone());
    keepalive.set_permission(Permission::Granted);
    // Off: sessions alone never start it.
    let (a, _) = open_fake(&h, "main", "one").await;
    assert!(!keepalive.status().running);
    assert!(keepalive.set_enabled(true).await.unwrap().running);
    let (b, _) = open_fake(&h, "other", "two").await;
    h.state.close_session("main", &a).await.unwrap();
    assert!(keepalive.status().running);
    h.state.close_session("other", &b).await.unwrap();
    assert!(!keepalive.status().running);
    assert_eq!(*driver.0.lock().unwrap(), vec![true, false]);

    // A session that ends on its own (remote EOF) stops it too.
    let (_c, sink) = open_fake(&h, "main", "three").await;
    assert!(keepalive.status().running);
    h.transport.shells.lock().unwrap().clear();
    eventually("closed", || !sink.closed.lock().unwrap().is_empty()).await;
    eventually("stopped", || !keepalive.status().running).await;
    assert_eq!(*driver.0.lock().unwrap(), vec![true, false, true, false]);

    // "Allow Once" ends while a session is open: stopped, turned off, and
    // the UI hears it (the `keepalive` event) to uncheck the box.
    let (_d, _) = open_fake(&h, "main", "four").await;
    assert!(keepalive.status().running);
    keepalive.set_permission(Permission::NotDetermined);
    assert_eq!(
        *driver.0.lock().unwrap(),
        vec![true, false, true, false, true, false]
    );
    let sent: Vec<Value> = h
        .events
        .0
        .lock()
        .unwrap()
        .iter()
        .filter(|(name, _)| name == "keepalive")
        .map(|(_, p)| p.clone())
        .collect();
    assert_eq!(
        sent.last(),
        Some(&json!({"enabled": false, "authorized": false, "running": false}))
    );
    assert!(
        sent.contains(&json!({"enabled": true, "authorized": true, "running": true})),
        "{sent:?}"
    );
}

/// The steps a progress channel received, as their wire names.
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

fn no_op_channels() -> (
    Channel<InvokeResponseBody>,
    Channel<tether_app_lib::SessionClosed>,
) {
    (
        Channel::new(|_| Ok(())),
        Channel::<tether_app_lib::SessionClosed>::new(|_| Ok(())),
    )
}

/// A connect that fails after the auth step has shown tcp, hostKey and auth,
/// and never the shell step.
#[tokio::test]
async fn open_session_stops_the_steps_where_the_connect_fails() {
    let h = harness_with(FakeTransport {
        connect_steps: vec![ConnectStep::Tcp, ConnectStep::HostKey, ConnectStep::Auth],
        ..Default::default()
    });
    created(&h).await;
    let host = h
        .state
        .upsert_entity(
            "host",
            json!({"label": "h", "address": "example.test", "settings": {}}),
        )
        .await
        .unwrap();
    let target = serde_json::from_value(json!({"kind": "host", "hostId": host["id"]})).unwrap();
    let (out, closed) = no_op_channels();
    let (progress, steps) = recording_progress();
    h.state
        .open_session("main", target, 80, 24, out, closed, progress)
        .await
        .unwrap_err();
    assert_eq!(*steps.lock().unwrap(), ["tcp", "hostKey", "auth"]);
}

/// A local shell connects nothing: only the shell step.
#[tokio::test]
async fn open_session_for_a_local_target_sends_only_shell() {
    let h = harness();
    let target = serde_json::from_value(json!({"kind": "local"})).unwrap();
    let (out, closed) = no_op_channels();
    let (progress, steps) = recording_progress();
    let opened = h
        .state
        .open_session("main", target, 80, 24, out, closed, progress)
        .await
        .expect("local shell");
    assert_eq!(*steps.lock().unwrap(), ["shell"]);
    h.state
        .close_session("main", &opened.session_id)
        .await
        .unwrap();
}

/// A session that is not a host shell is never in tmux; an unknown or gone
/// session is an error (the UI keeps what it knew), as is another window's.
#[tokio::test]
async fn session_tmux_of_a_non_host_session_is_none() {
    let h = harness();
    let (id, _) = open_fake(&h, "main", "direct").await;
    assert_eq!(h.state.session_tmux("main", &id).await.unwrap(), None);
    let other = h.state.session_tmux("other", &id).await.unwrap_err();
    assert_eq!(other.code, "notOwner");
    let gone = h.state.session_tmux("main", "no-such").await.unwrap_err();
    assert_eq!(gone.code, "closed");
}

#[tokio::test]
async fn active_work_lists_only_the_callers_sessions() {
    let h = harness();
    let (m1, _s1) = open_fake(&h, "main", "prod-db").await;
    let (_o1, _so) = open_fake(&h, "other", "elsewhere").await;
    let work = h.state.active_work("main");
    assert_eq!(work.sessions.len(), 1);
    assert_eq!(work.sessions[0].id, m1);
    assert_eq!(work.sessions[0].title, "prod-db");
    assert!(work.transfers.is_empty());
    h.state.close_all_sessions("main").await;
    h.state.close_all_sessions("other").await;
}

/// `write_shell` takes the bytes as the raw IPC body and the session id in
/// a header.
#[test]
fn write_args_read_raw_body_and_session_header() {
    let mut headers = HeaderMap::new();
    headers.insert(SESSION_ID_HEADER, HeaderValue::from_static("sess-1"));
    let body = InvokeBody::Raw(vec![0, 159, 255, b'a']);
    let (id, bytes) = write_args(&headers, &body).unwrap();
    assert_eq!((id.as_str(), bytes), ("sess-1", vec![0, 159, 255, b'a']));

    assert!(write_args(&HeaderMap::new(), &body).is_err(), "missing id");
    let json_body = InvokeBody::Json(json!({"data": [1, 2]}));
    assert!(
        write_args(&headers, &json_body).is_err(),
        "JSON body refused"
    );
}

/// Android's IPC cannot carry a raw body, so the `Uint8Array` arrives as a
/// JSON number array; there (only) that is the bytes. Anything else in JSON
/// is still refused.
#[test]
fn write_args_take_a_json_byte_array_where_raw_bodies_cannot_be_read() {
    let mut headers = HeaderMap::new();
    headers.insert(SESSION_ID_HEADER, HeaderValue::from_static("sess-1"));
    let bytes = InvokeBody::Json(json!([0, 159, 255, 97]));
    let (id, got) = write_args_for(&headers, &bytes, true).unwrap();
    assert_eq!((id.as_str(), got), ("sess-1", vec![0, 159, 255, b'a']));
    let (_, empty) = write_args_for(&headers, &InvokeBody::Json(json!([])), true).unwrap();
    assert!(empty.is_empty());

    for bad in [
        json!([256]),
        json!([-1]),
        json!([1.5]),
        json!(["a"]),
        json!({"data": [1, 2]}),
        json!("abc"),
    ] {
        assert!(
            write_args_for(&headers, &InvokeBody::Json(bad.clone()), true).is_err(),
            "{bad} refused"
        );
    }
    assert!(
        write_args_for(&headers, &bytes, false).is_err(),
        "a JSON array is refused where raw bodies work"
    );
}

// ---------------------------------------------------------------------------
// Vault

#[tokio::test]
async fn vault_lifecycle_and_crud() {
    let h = harness();
    let status = h.state.vault_status().await;
    assert_eq!(status.state, VaultState::Uninitialized);
    // Locking an uncreated vault succeeds (the mock contract matches).
    h.state.vault_lock().await.unwrap();
    assert_eq!(
        h.state.vault_status().await.state,
        VaultState::Uninitialized
    );

    let created = created(&h).await;
    assert!(!created.recovery_key.is_empty());
    assert_eq!(h.state.vault_status().await.state, VaultState::Unlocked);

    h.state.vault_lock().await.unwrap();
    assert_eq!(h.state.vault_status().await.state, VaultState::Locked);
    assert!(h.events.names().contains(&"vault-locked".to_owned()));
    let err = h.state.vault_unlock(secret("nope")).await.unwrap_err();
    assert_eq!(err.code, "wrongPassword");
    let err = h.state.list_entities("host").await.unwrap_err();
    assert_eq!(err.code, "locked");
    h.state.vault_unlock(secret("master-pw")).await.unwrap();

    // Group, identity (with a password), tag, host.
    let group = h
        .state
        .upsert_entity(
            "group",
            json!({"name": "prod", "parentId": null,
            "settings": {"port": 2200}}),
        )
        .await
        .unwrap();
    let group_id = group["id"].as_str().unwrap().to_owned();
    let identity = h
        .state
        .upsert_entity(
            "identity",
            json!({"label": "ops", "username": "root", "password": "canary-pw-1"}),
        )
        .await
        .unwrap();
    assert_eq!(identity["hasPassword"], json!(true));
    assert!(!identity.to_string().contains("canary-pw-1"));
    let identity_id = identity["id"].as_str().unwrap().to_owned();
    let tag = h
        .state
        .upsert_entity("tag", json!({"name": "db", "color": "#f00"}))
        .await
        .unwrap();
    let host = h
        .state
        .upsert_entity(
            "host",
            json!({"label": "db 🐘", "address": "10.0.0.5", "groupId": group_id,
                "tagIds": [tag["id"]], "settings": {"identityId": identity_id}}),
        )
        .await
        .unwrap();
    let host_id = host["id"].as_str().unwrap().to_owned();
    assert_eq!(host["label"], json!("db 🐘"));

    let hosts = h.state.list_entities("host").await.unwrap();
    assert_eq!(hosts.len(), 1);
    let identities = h.state.list_entities("identity").await.unwrap();
    assert!(
        !Value::Array(identities.clone())
            .to_string()
            .contains("canary-pw-1")
    );

    // Updating the identity without a password keeps it; null clears it.
    let kept = h
        .state
        .upsert_entity(
            "identity",
            json!({"id": identity_id, "label": "ops2", "username": "root"}),
        )
        .await
        .unwrap();
    assert_eq!(kept["hasPassword"], json!(true));
    let cleared = h
        .state
        .upsert_entity(
            "identity",
            json!({"id": identity_id, "label": "ops2", "username": "root", "password": null}),
        )
        .await
        .unwrap();
    assert_eq!(cleared["hasPassword"], json!(false));
    let restored = h
        .state
        .upsert_entity(
            "identity",
            json!({"id": identity_id, "label": "ops", "username": "root", "password": "canary-pw-2"}),
        )
        .await
        .unwrap();
    assert_eq!(restored["hasPassword"], json!(true));

    // Effective settings show where each value comes from.
    let eff = h
        .state
        .effective_settings(host_id.parse().unwrap())
        .await
        .unwrap();
    assert_eq!(eff.port.value, 2200);
    assert_eq!(
        eff.port.source,
        SourceDto::Group {
            group_id: group_id.parse().unwrap()
        }
    );
    assert_eq!(eff.identity_id.source, SourceDto::Host);
    assert_eq!(eff.term_type.source, SourceDto::Default);
    let eff_json = serde_json::to_value(&eff).unwrap();
    assert_eq!(
        eff_json["port"]["source"],
        json!({"kind": "group", "groupId": group_id})
    );

    // A referenced identity cannot be deleted; the error lists the users.
    let err = h
        .state
        .delete_entity("identity", identity_id.parse().unwrap())
        .await
        .unwrap_err();
    assert_eq!(err.code, "referenced");
    assert_eq!(err.refs, vec![host_id.clone()]);

    // A group with children needs a policy.
    let err = h
        .state
        .delete_entity("group", group_id.parse().unwrap())
        .await
        .unwrap_err();
    assert_eq!(err.code, "referenced");
    h.state
        .delete_group(group_id.parse().unwrap(), ChildPolicyDto::MoveToParent)
        .await
        .unwrap();
    let hosts = h.state.list_entities("host").await.unwrap();
    assert_eq!(hosts[0]["groupId"], Value::Null);

    // Unknown kinds and bad JSON are input errors.
    assert_eq!(
        h.state.list_entities("preference").await.unwrap_err().code,
        "invalid"
    );
    assert_eq!(
        h.state
            .upsert_entity("host", json!({"label": 5}))
            .await
            .unwrap_err()
            .code,
        "invalid"
    );
    // A misspelt field is refused, not silently dropped.
    for (kind, bad) in [
        (
            "host",
            json!({"label": "x", "address": "y", "groupID": group_id}),
        ),
        (
            "host",
            json!({"label": "x", "address": "y", "settings": {"Port": 22}}),
        ),
        ("group", json!({"name": "g", "parentID": null})),
        (
            "tag",
            json!({"name": "t", "color": "#fff", "colour": "#000"}),
        ),
        (
            "known_host",
            json!({"hostPattern": "h", "port": 22, "algorithm": "ssh-ed25519",
            "publicKey": "AAAA", "addedAT": 1}),
        ),
        (
            "forward_rule",
            json!({"hostId": host_id, "label": "f", "kind": "local",
            "bindAddr": "127.0.0.1", "bindPort": 1, "destport": 2}),
        ),
    ] {
        let err = h.state.upsert_entity(kind, bad.clone()).await.unwrap_err();
        assert_eq!(err.code, "invalid", "{kind} {bad}: {err}");
        assert!(err.message.contains("unknown field"), "{err}");
    }
    // A listed known host (with its output-only fingerprint) can be sent back.
    let kh = h
        .state
        .upsert_entity(
            "known_host",
            json!({"hostPattern": "kh.test", "port": 22, "algorithm": "ssh-ed25519",
                "publicKey": "AAAA"}),
        )
        .await
        .unwrap();
    h.state.upsert_entity("known_host", kh).await.unwrap();

    // Deleting the host cascades its forward rules.
    h.state
        .upsert_entity(
            "forward_rule",
            json!({"hostId": host_id, "label": "web", "kind": "local",
                "bindAddr": "127.0.0.1", "bindPort": 8080, "destHost": "localhost",
                "destPort": 80, "autostart": false}),
        )
        .await
        .unwrap();
    h.state
        .delete_entity("host", host_id.parse().unwrap())
        .await
        .unwrap();
    assert!(
        h.state
            .list_entities("forward_rule")
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn vault_recovery_and_quick_unlock() {
    let h = harness();
    let created = created(&h).await;
    h.state.vault_lock().await.unwrap();
    let err = h
        .state
        .vault_unlock_recovery(secret("not-a-key"), secret("new-pw"))
        .await
        .unwrap_err();
    assert_eq!(err.code, "wrongRecoveryKey");
    h.state
        .vault_unlock_recovery(secret(&created.recovery_key), secret("new-pw"))
        .await
        .unwrap();
    h.state.vault_lock().await.unwrap();
    h.state.vault_unlock(secret("new-pw")).await.unwrap();

    assert_eq!(
        h.state.vault_quick_unlock().await.unwrap_err().code,
        "quickUnlockNotEnabled"
    );
    h.state.vault_enable_quick_unlock().await.unwrap();
    assert!(h.state.vault_status().await.quick_unlock_enabled);
    h.state.vault_lock().await.unwrap();
    h.state.vault_quick_unlock().await.unwrap();
    assert_eq!(h.state.vault_status().await.state, VaultState::Unlocked);
    h.state.vault_disable_quick_unlock().await.unwrap();
    assert!(!h.state.vault_status().await.quick_unlock_enabled);
}

/// Touch ID: status names it, and a declined or changed-fingers attempt
/// maps to its own error code with the vault still locked.
#[tokio::test]
async fn vault_touch_id_status_and_errors() {
    let h = harness();
    let gate = FakeGate::new(b"fingers-1");
    {
        let mut v = h.vault.lock().unwrap();
        let path = h._dir.path().join("touch.sqlite");
        *v = VaultService::open_for_tests(&path, Box::new(MemoryKeyStore::default()))
            .unwrap()
            .with_gate(Box::new(gate.clone()));
    }
    assert_eq!(
        h.state.vault_status().await.quick_unlock_kind,
        GateKind::TouchId
    );
    created(&h).await;
    h.state.vault_enable_quick_unlock().await.unwrap();
    h.state.vault_lock().await.unwrap();

    gate.answer(false);
    let err = h.state.vault_quick_unlock().await.unwrap_err();
    assert_eq!(err.code, "presenceDenied");
    assert_eq!(h.state.vault_status().await.state, VaultState::Locked);

    gate.set_state(Err("lid closed".into()));
    let err = h.state.vault_quick_unlock().await.unwrap_err();
    assert_eq!(err.code, "biometryUnavailable");

    gate.set_state(Ok(b"fingers-2".to_vec()));
    let err = h.state.vault_quick_unlock().await.unwrap_err();
    assert_eq!(err.code, "biometryChanged");
    assert!(!h.state.vault_status().await.quick_unlock_enabled);
}

/// R26: a vault that failed to open does not stop the app; status says why
/// and every vault call fails cleanly.
#[tokio::test]
async fn vault_unavailable_is_reported_not_fatal() {
    let state = AppState::new(
        VaultSlot::Failed("vault.sqlite is damaged".into()),
        Arc::new(PromptBridge::new(Arc::new(|_, _| Ok(())))),
        Arc::new(Recorder::default()),
        Arc::new(FakeTransport::default()),
    );
    let status = state.vault_status().await;
    assert_eq!(status.state, VaultState::Unavailable);
    assert_eq!(status.error.as_deref(), Some("vault.sqlite is damaged"));
    let err = state.vault_unlock(secret("x")).await.unwrap_err();
    assert_eq!(err.code, "vaultUnavailable");
    assert_eq!(
        state.list_entities("host").await.unwrap_err().code,
        "vaultUnavailable"
    );
    // Local work still goes on.
    assert!(state.active_work("main").sessions.is_empty());
}

/// An existing 0-byte vault file (a truncated vault) is unavailable, named
/// by path, and left untouched; it is never offered as a new vault.
#[tokio::test]
async fn empty_vault_file_is_unavailable_and_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(tether_app_lib::VAULT_FILE);
    std::fs::write(&path, b"").unwrap();
    let slot = tether_app_lib::open_vault(
        Ok(dir.path().to_path_buf()),
        core_api::vault::default_gate(),
    );
    let VaultSlot::Failed(why) = &slot else {
        panic!("a 0-byte vault file opened");
    };
    assert!(why.contains(&path.display().to_string()), "{why}");
    assert!(why.contains("empty"), "{why}");
    let state = AppState::new(
        slot,
        Arc::new(PromptBridge::new(Arc::new(|_, _| Ok(())))),
        Arc::new(Recorder::default()),
        Arc::new(FakeTransport::default()),
    );
    assert_eq!(state.vault_status().await.state, VaultState::Unavailable);
    let Err(err) = state.vault_create(secret("x")).await else {
        panic!("created a vault over the empty file");
    };
    assert_eq!(err.code, "vaultUnavailable");
    assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
}

#[tokio::test]
async fn connect_params_come_from_settings_and_identity() {
    let h = harness();
    created(&h).await;
    let key = h
        .state
        .key_generate(KeyAlgoDto::Ed25519, "deploy".into(), Some(secret("kp")))
        .await
        .unwrap();
    let pw_identity = h
        .state
        .upsert_entity(
            "identity",
            json!({"label": "pw", "username": "alice", "password": "s3"}),
        )
        .await
        .unwrap();
    let key_identity = h
        .state
        .upsert_entity(
            "identity",
            json!({"label": "key", "username": "bob", "keyId": key.id}),
        )
        .await
        .unwrap();
    let bare_identity = h
        .state
        .upsert_entity("identity", json!({"label": "ask", "username": "carol"}))
        .await
        .unwrap();
    // Keepalive and env come from a group, so inheritance is covered too.
    let group = h
        .state
        .upsert_entity(
            "group",
            json!({"name": "g", "settings": {"keepaliveSecs": 15,
                "env": [["LANG", "C.UTF-8"], ["TZ", "UTC"]]}}),
        )
        .await
        .unwrap();
    let mk_host = |identity: Option<&Value>| {
        json!({"label": "h", "address": "example.test", "groupId": group["id"],
            "settings": {"identityId": identity.map(|i| i["id"].clone()), "port": 2022,
                "termType": "vt220"}})
    };
    let mut ids = vec![];
    for identity in [
        Some(&pw_identity),
        Some(&key_identity),
        Some(&bare_identity),
        None,
    ] {
        let host = h
            .state
            .upsert_entity("host", mk_host(identity))
            .await
            .unwrap();
        ids.push(host["id"].as_str().unwrap().parse::<Uuid>().unwrap());
    }

    let p = h.state.connect_params(ids[0], 100, 30).await.unwrap();
    assert_eq!(
        (
            p.host.as_str(),
            p.port,
            p.username.as_str(),
            p.term.as_str(),
            p.cols,
            p.rows
        ),
        ("example.test", 2022, "alice", "vt220", 100, 30)
    );
    assert_eq!(p.keepalive, Some(Duration::from_secs(15)));
    assert_eq!(
        p.env,
        vec![
            ("LANG".to_owned(), "C.UTF-8".to_owned()),
            ("TZ".to_owned(), "UTC".to_owned())
        ]
    );
    let AuthMethod::Password(pw) = &p.auth else {
        panic!("expected password auth");
    };
    assert_eq!(pw.expose_secret(), "s3");

    let p = h.state.connect_params(ids[1], 80, 24).await.unwrap();
    assert_eq!(p.username, "bob");
    let AuthMethod::PrivateKey { pem, passphrase } = &p.auth else {
        panic!("expected key auth");
    };
    assert!(pem.expose_secret().contains("OPENSSH PRIVATE KEY"));
    assert_eq!(passphrase.as_ref().unwrap().expose_secret(), "kp");

    let p = h.state.connect_params(ids[2], 80, 24).await.unwrap();
    assert!(matches!(p.auth, AuthMethod::KeyboardInteractive));

    // Outside the group: the app default keepalive (30 s) and no env; a host
    // can switch keepalives off with 0.
    for (secs, expected) in [(None, 30), (Some(0), 0)] {
        let host = h
            .state
            .upsert_entity(
                "host",
                json!({"label": "plain", "address": "plain.test",
                    "settings": {"identityId": pw_identity["id"], "keepaliveSecs": secs}}),
            )
            .await
            .unwrap();
        let p = h
            .state
            .connect_params(host["id"].as_str().unwrap().parse().unwrap(), 80, 24)
            .await
            .unwrap();
        assert_eq!(p.keepalive, Some(Duration::from_secs(expected)));
        assert!(p.env.is_empty());
    }
    assert_eq!(
        ConnectParams {
            keepalive: Some(Duration::ZERO),
            ..h.state.connect_params(ids[2], 80, 24).await.unwrap()
        }
        .keepalive_interval(),
        None,
        "0 s disables keepalives"
    );

    // No identity (R31): the local login name and keyboard-interactive.
    let login = tether_app_lib::vault_cmds::local_login_name().expect("USER or USERNAME set");
    let p = h.state.connect_params(ids[3], 80, 24).await.unwrap();
    assert_eq!(p.username, login);
    assert!(matches!(p.auth, AuthMethod::KeyboardInteractive));

    // An identity with an empty username (an imported `IdentityFile`
    // without `User`) keeps its credentials and uses the local login name.
    let no_user = h
        .state
        .upsert_entity(
            "identity",
            json!({"label": "id_k", "username": "", "keyId": key.id}),
        )
        .await
        .unwrap();
    let host = h
        .state
        .upsert_entity("host", mk_host(Some(&no_user)))
        .await
        .unwrap();
    let p = h
        .state
        .connect_params(host["id"].as_str().unwrap().parse().unwrap(), 80, 24)
        .await
        .unwrap();
    assert_eq!(p.username, login);
    assert!(matches!(p.auth, AuthMethod::PrivateKey { .. }));
}

#[test]
fn login_name_falls_back_user_logname_username() {
    use tether_app_lib::vault_cmds::login_name_from;
    let env = |pairs: &'static [(&'static str, &'static str)]| {
        move |name: &str| {
            pairs
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| (*v).to_owned())
        }
    };
    assert_eq!(
        login_name_from(env(&[("USER", "ann"), ("USERNAME", "win")])).as_deref(),
        Some("ann")
    );
    assert_eq!(
        login_name_from(env(&[("USER", " "), ("LOGNAME", "lou")])).as_deref(),
        Some("lou")
    );
    assert_eq!(
        login_name_from(env(&[("USERNAME", "win")])).as_deref(),
        Some("win")
    );
    assert_eq!(login_name_from(env(&[])), None);
}

#[tokio::test]
async fn key_save_private_writes_the_picked_file_after_the_password() {
    let h = harness();
    let master = created_fresh(&h).await;
    let key = h
        .state
        .key_generate(KeyAlgoDto::Ed25519, "work laptop".into(), None)
        .await
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("saved");

    // A wrong password fails before the dialog opens.
    let err = h
        .state
        .key_save_private(key.id, secret(&fresh_password()), |_| panic!("no dialog"))
        .await
        .err()
        .unwrap();
    assert_eq!(err.code, "wrongPassword");
    // Cancelled: nothing written.
    let cancelled = h
        .state
        .key_save_private(key.id, secret(&master), |_| None)
        .await
        .unwrap();
    assert_eq!(cancelled, None);
    assert!(!path.exists());

    let target = path.clone();
    let saved = h
        .state
        .key_save_private(key.id, secret(&master), move |name| {
            assert_eq!(name, "work_laptop");
            Some(target)
        })
        .await
        .unwrap();
    assert_eq!(saved.as_deref(), Some(path.as_path()));
    assert!(
        std::fs::read_to_string(&path)
            .unwrap()
            .contains("BEGIN OPENSSH PRIVATE KEY")
    );
}

#[tokio::test]
async fn keys_generate_import_export() {
    let h = harness();
    let master = created_fresh(&h).await;
    let generated = h
        .state
        .key_generate(KeyAlgoDto::Ed25519, "laptop".into(), None)
        .await
        .unwrap();
    assert_eq!(generated.algorithm, "ssh-ed25519");
    assert!(generated.fingerprint.starts_with("SHA256:"));
    assert!(!generated.has_passphrase);
    let listed = h.state.list_entities("ssh_key").await.unwrap();
    assert_eq!(listed.len(), 1);
    assert!(!Value::Array(listed).to_string().contains("PRIVATE KEY"));

    // Export asks for the master password again.
    let err = h
        .state
        .key_export_private(generated.id, secret(&fresh_password()))
        .await
        .err()
        .unwrap();
    assert_eq!(err.code, "wrongPassword");
    let exported = h
        .state
        .key_export_private(generated.id, secret(&master))
        .await
        .unwrap();
    assert!(
        exported
            .private_openssh
            .contains("BEGIN OPENSSH PRIVATE KEY")
    );

    // Importing it back yields the same public key.
    let imported = h
        .state
        .key_import(
            secret(&exported.private_openssh),
            None,
            Some("again".into()),
        )
        .await
        .unwrap();
    assert_eq!(imported.fingerprint, generated.fingerprint);
    assert_eq!(imported.label, "again");
    // A passphrase given for an unencrypted key is not kept (the mock
    // contract test checks the same).
    let plain = h
        .state
        .key_import(
            secret(&exported.private_openssh),
            Some(secret("ignored")),
            None,
        )
        .await
        .unwrap();
    assert!(!plain.has_passphrase);
    // An encrypted key needs its passphrase, which is then kept.
    let locked = h
        .state
        .key_generate(KeyAlgoDto::Ed25519, "locked".into(), Some(secret("kp")))
        .await
        .unwrap();
    let locked_text = h
        .state
        .key_export_private(locked.id, secret(&master))
        .await
        .unwrap()
        .private_openssh;
    let err = h
        .state
        .key_import(secret(&locked_text), None, None)
        .await
        .unwrap_err();
    assert_eq!(err.code, "needsPassphrase");
    let err = h
        .state
        .key_import(secret(&locked_text), Some(secret("nope")), None)
        .await
        .unwrap_err();
    assert_eq!(err.code, "badPassphrase");
    let again = h
        .state
        .key_import(secret(&locked_text), Some(secret("kp")), None)
        .await
        .unwrap();
    assert!(again.has_passphrase);
    assert_eq!(again.fingerprint, locked.fingerprint);

    let err = h
        .state
        .key_import(secret("garbage"), None, None)
        .await
        .unwrap_err();
    assert_eq!(err.code, "key");

    // A referenced key cannot be deleted.
    let identity = h
        .state
        .upsert_entity(
            "identity",
            json!({"label": "k", "username": "u", "keyId": generated.id}),
        )
        .await
        .unwrap();
    let err = h
        .state
        .delete_entity("ssh_key", generated.id)
        .await
        .unwrap_err();
    assert_eq!(err.code, "referenced");
    assert_eq!(err.refs, vec![identity["id"].as_str().unwrap().to_owned()]);

    // Renaming keeps the key material.
    let renamed = h
        .state
        .upsert_entity("ssh_key", json!({"id": generated.id, "label": "renamed"}))
        .await
        .unwrap();
    assert_eq!(renamed["label"], json!("renamed"));
    assert_eq!(renamed["fingerprint"], json!(generated.fingerprint));
}

#[tokio::test]
async fn known_hosts_import_counts_and_dedupes() {
    let h = harness();
    created(&h).await;
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("known_hosts");
    let key = include_str!("../../../../tests/fixtures/keys/id_ed25519_test.pub");
    let blob: Vec<&str> = key.split_whitespace().take(2).collect();
    std::fs::write(
        &file,
        format!(
            "# comment\nalpha.test,[beta.test]:2222 {0} {1}\n@revoked gamma {0} {1}\n",
            blob[0], blob[1]
        ),
    )
    .unwrap();
    let report = h.state.known_hosts_import(file.clone()).await.unwrap();
    assert_eq!((report.added, report.already_known), (2, 0));
    let again = h.state.known_hosts_import(file).await.unwrap();
    assert_eq!((again.added, again.already_known), (0, 2));
    let listed = h.state.list_entities("known_host").await.unwrap();
    assert_eq!(listed.len(), 2);
    assert!(listed.iter().all(|k| k["source"] == json!("import")));
    assert!(
        listed
            .iter()
            .any(|k| k["hostPattern"] == json!("beta.test") && k["port"] == json!(2222))
    );

    let err = h
        .state
        .known_hosts_import(dir.path().join("missing"))
        .await
        .unwrap_err();
    assert_eq!(err.code, "io");
}

#[tokio::test]
async fn import_ssh_config_reports_counts() {
    let h = harness();
    created(&h).await;
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("config");
    std::fs::write(
        &cfg,
        "Host web\n  HostName web.example.test\n  User deploy\n  Port 2200\n\
         Host jump\n  HostName j.test\n  ProxyJump bastion\n",
    )
    .unwrap();
    let report = h.state.import_ssh_config(cfg).await.unwrap();
    assert_eq!(report.hosts_added, 1);
    assert_eq!(report.identities_added, 1);
    assert!(
        report.skipped.iter().any(|s| s.name == "jump"),
        "{:?}",
        report.skipped
    );
    let hosts = h.state.list_entities("host").await.unwrap();
    assert_eq!(hosts[0]["address"], json!("web.example.test"));
}

// ---------------------------------------------------------------------------
// Events

#[tokio::test]
async fn prompt_events_and_withdrawal_reach_the_event_sink() {
    let events = Arc::new(Recorder::default());
    let bridge = tether_app_lib::prompt_cmds::prompt_bridge_with_timeout(
        events.clone(),
        Duration::from_millis(50),
    );
    let answer = bridge.ask(Prompt::Password {
        user: "u".into(),
        host: "h".into(),
        save_to: None,
    });
    assert!(matches!(answer.await, core_api::PromptResponse::Reject));
    let recorded = events.0.lock().unwrap().clone();
    assert_eq!(recorded.len(), 2, "{recorded:?}");
    assert_eq!(recorded[0].0, "prompt");
    assert_eq!(recorded[0].1["prompt"]["kind"], json!("password"));
    assert_eq!(recorded[1].0, "prompt-withdrawn");
    assert_eq!(recorded[1].1, json!({"id": recorded[0].1["id"]}));
}

/// Without a vault the SFTP and forward commands fail cleanly (they need a
/// host from the vault).
#[tokio::test]
async fn sftp_and_forwards_need_an_unlocked_vault() {
    let h = harness();
    created(&h).await;
    h.state.vault_lock().await.unwrap();
    let host = uid();
    assert_eq!(h.state.sftp_open(host).await.unwrap_err().code, "locked");
    assert_eq!(
        h.state.sftp_list(host, "/".into()).await.unwrap_err().code,
        "notOpen"
    );
    assert_eq!(
        h.state.forward_start(uid()).await.unwrap_err().code,
        "locked"
    );
    assert!(!h.state.transfer_cancel("nope".into()));
    assert!(h.state.forward_statuses().is_empty());
    // `vault` stays usable from the harness (no lock held).
    assert!(!h.vault.lock().unwrap().is_unlocked());
}

/// A host's or group's own login: set through `set_own_login`, listed with
/// its owner, inherited by the group's hosts, and pruned once unlinked.
#[tokio::test]
async fn own_login_command_inherit_and_prune() {
    let h = harness();
    created(&h).await;
    let group = h
        .state
        .upsert_entity("group", json!({"name": "Production"}))
        .await
        .unwrap();
    let group_id: Uuid = group["id"].as_str().unwrap().parse().unwrap();
    let host = h
        .state
        .upsert_entity(
            "host",
            json!({"label": "web", "address": "10.0.0.5", "groupId": group_id}),
        )
        .await
        .unwrap();
    let host_id: Uuid = host["id"].as_str().unwrap().parse().unwrap();

    let login = |owner: Uuid, password: Value| {
        serde_json::from_value::<tether_app_lib::vault_cmds::OwnLoginInput>(
            json!({"ownerId": owner, "username": "alice", "password": password}),
        )
        .unwrap()
    };
    let own = h
        .state
        .set_own_login(login(group_id, json!("lab-pw")))
        .await
        .unwrap();
    assert_eq!(own.owner, Some(group_id));
    assert!(own.has_password);
    assert_eq!(own.label, "Production");
    let listed = h.state.list_entities("identity").await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["owner"], json!(group_id));
    assert!(listed[0].get("password").is_none());

    // The group's hosts connect with it.
    let params = h.state.connect_params(host_id, 80, 24).await.unwrap();
    assert_eq!(params.username, "alice");
    match &params.auth {
        AuthMethod::Password(p) => assert_eq!(p.expose_secret(), "lab-pw"),
        _ => panic!("expected the group's password"),
    }

    // Omitting the password keeps it.
    let kept = serde_json::from_value::<tether_app_lib::vault_cmds::OwnLoginInput>(
        json!({"ownerId": group_id, "username": "root"}),
    )
    .unwrap();
    let again = h.state.set_own_login(kept).await.unwrap();
    assert_eq!(again.id, own.id);
    assert!(again.has_password);

    // Editing an own login as an identity keeps its owner.
    h.state
        .upsert_entity(
            "identity",
            json!({"id": own.id, "label": "Production", "username": "root2"}),
        )
        .await
        .unwrap();
    let listed = h.state.list_entities("identity").await.unwrap();
    assert_eq!(listed[0]["owner"], json!(group_id));

    // The group stops using it: it is pruned on save.
    h.state
        .upsert_entity(
            "group",
            json!({"id": group_id, "name": "Production", "settings": {"identityId": null}}),
        )
        .await
        .unwrap();
    assert!(h.state.list_entities("identity").await.unwrap().is_empty());

    // A host's own login goes with the host.
    h.state
        .set_own_login(login(host_id, json!(null)))
        .await
        .unwrap();
    assert_eq!(h.state.list_entities("identity").await.unwrap().len(), 1);
    h.state.delete_entity("host", host_id).await.unwrap();
    assert!(h.state.list_entities("identity").await.unwrap().is_empty());
}

/// An own login is named after its owner, also after the owner is renamed.
#[tokio::test]
async fn own_login_follows_owner_rename() {
    let h = harness();
    created(&h).await;
    let group = h
        .state
        .upsert_entity("group", json!({"name": "Production"}))
        .await
        .unwrap();
    let group_id: Uuid = group["id"].as_str().unwrap().parse().unwrap();
    let login = serde_json::from_value::<tether_app_lib::vault_cmds::OwnLoginInput>(
        json!({"ownerId": group_id, "username": "alice"}),
    )
    .unwrap();
    let own = h.state.set_own_login(login).await.unwrap();
    h.state
        .upsert_entity(
            "group",
            json!({"id": group_id, "name": "Prod", "settings": {"identityId": own.id}}),
        )
        .await
        .unwrap();
    let listed = h.state.list_entities("identity").await.unwrap();
    assert_eq!(listed[0]["label"], json!("Prod"));
}

// ---------------------------------------------------------------------------
// Sync, password change and conflicts (against an in-process TLS server)

mod sync {
    use super::*;
    use std::net::SocketAddr;
    use std::sync::atomic::AtomicBool;
    use tether_server::testing::{TestServer, spawn_tls};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::task::AbortHandle;

    /// Appears in every password of these scenarios; no DTO or event may
    /// carry it.
    pub const CANARY: &str = "CANARY-7f3a";
    pub const MASTER: &str = "CANARY-7f3a-master";
    pub const USER: &str = "alice";
    const WAIT: Duration = Duration::from_secs(20);

    pub fn master() -> SecretString {
        secret(MASTER)
    }

    /// Polls the sync status until `pred` holds.
    pub async fn wait_status(
        h: &Harness,
        what: &str,
        pred: impl Fn(&SyncStatusDto) -> bool,
    ) -> SyncStatusDto {
        let start = std::time::Instant::now();
        loop {
            let status = h.state.sync_status().await;
            if pred(&status) {
                return status;
            }
            assert!(
                start.elapsed() < WAIT,
                "timed out waiting for {what}; last status {status:?}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Waits until the device is idle with nothing left to push. A new
    /// engine starts out `Idle { last_sync: 0 }` before its first cycle, so
    /// that does not count: it would return while the engine is still
    /// signing in.
    pub async fn settled(h: &Harness) {
        wait_status(
            h,
            "an idle sync",
            |s| matches!(s.status, SyncStateDto::Idle { last_sync } if last_sync > 0),
        )
        .await;
        let start = std::time::Instant::now();
        while h.vault.lock().unwrap().outbox_len().unwrap() != 0 {
            assert!(start.elapsed() < WAIT, "outbox never emptied");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    pub async fn until<F, Fut>(what: &str, mut f: F)
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = bool>,
    {
        let start = std::time::Instant::now();
        while !f().await {
            assert!(start.elapsed() < WAIT, "timed out waiting for {what}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    pub fn locked_event(h: &Harness, reason: &str) -> bool {
        h.events
            .0
            .lock()
            .unwrap()
            .iter()
            .any(|(n, p)| n == "vault-locked" && p["reason"] == json!(reason))
    }

    /// The same code pointed at `url` (the test server's codes name its public
    /// URL, not the ephemeral port).
    pub fn relink(code: &str, url: &str) -> SecretString {
        let mut parsed = core_sync::parse_join_code(code).unwrap();
        parsed.url = url.to_owned();
        secret(&parsed.encode())
    }

    pub async fn invite(srv: &TestServer) -> SecretString {
        secret(&srv.make_invite().await)
    }

    /// Turns sync on for `h`'s vault (made with [`MASTER`]) and waits for the
    /// first sync.
    pub async fn enable(srv: &TestServer, h: &Harness, recovery: &str) {
        let out = h
            .state
            .sync_enable(
                invite(srv).await,
                USER.into(),
                master(),
                RecoveryInput::Existing(secret(recovery)),
            )
            .await
            .expect("enable");
        assert!(out.new_recovery_key.is_none());
        settled(h).await;
    }

    /// A new device joined through an add-device code from `from`, reaching
    /// the server at `url`.
    pub async fn join(url: &str, from: &Harness, to: &Harness, password: SecretString) {
        let link = from.state.sync_link_code().await.expect("link code");
        to.state
            .sync_join(relink(&link.code, url), USER.into(), password, false)
            .await
            .expect("join");
        settled(to).await;
    }

    /// Server, first device (with a host and an identity whose password is the
    /// canary) and a second device that has joined and synced.
    pub struct Pair {
        pub srv: TestServer,
        pub a: Harness,
        pub b: Harness,
        pub recovery: String,
        pub host_id: Uuid,
        pub identity_id: Uuid,
    }

    pub async fn pair() -> Pair {
        let srv = spawn_tls().await;
        let a = harness();
        let recovery = a.state.vault_create(master()).await.unwrap().recovery_key;
        let host = a
            .state
            .upsert_entity("host", json!({"label": "web", "address": "10.0.0.5"}))
            .await
            .unwrap();
        let identity = a
            .state
            .upsert_entity(
                "identity",
                json!({"label": "ops", "username": "root", "password": format!("{CANARY}-ops")}),
            )
            .await
            .unwrap();
        enable(&srv, &a, &recovery).await;
        let b = harness();
        join(&srv.url, &a, &b, master()).await;
        until("the host on device B", || async {
            !b.state.list_entities("host").await.unwrap().is_empty()
        })
        .await;
        Pair {
            srv,
            a,
            b,
            recovery,
            host_id: host["id"].as_str().unwrap().parse().unwrap(),
            identity_id: identity["id"].as_str().unwrap().parse().unwrap(),
        }
    }

    /// A TCP forwarder in front of the server whose connections can be cut,
    /// to take it "offline" for one device.
    pub struct Proxy {
        pub url: String,
        up: Arc<AtomicBool>,
        conns: Arc<Mutex<Vec<AbortHandle>>>,
    }

    impl Proxy {
        pub async fn start(target: SocketAddr) -> Proxy {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("https://{}", listener.local_addr().unwrap());
            let up = Arc::new(AtomicBool::new(true));
            let conns: Arc<Mutex<Vec<AbortHandle>>> = Arc::default();
            let (up2, conns2) = (up.clone(), conns.clone());
            tokio::spawn(async move {
                while let Ok((mut inbound, _)) = listener.accept().await {
                    if !up2.load(Ordering::SeqCst) {
                        continue; // dropped: the client sees a reset
                    }
                    let task = tokio::spawn(async move {
                        if let Ok(mut outbound) = TcpStream::connect(target).await {
                            let _ =
                                tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
                        }
                    });
                    conns2.lock().unwrap().push(task.abort_handle());
                }
            });
            Proxy { url, up, conns }
        }

        /// Cuts every connection and refuses new ones.
        pub fn down(&self) {
            self.up.store(false, Ordering::SeqCst);
            for c in self.conns.lock().unwrap().drain(..) {
                c.abort();
            }
        }

        pub fn up(&self) {
            self.up.store(true, Ordering::SeqCst);
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn sync_enable_and_status() {
    use sync::*;
    let srv = tether_server::testing::spawn_tls().await;
    let h = harness();
    let recovery = h.state.vault_create(master()).await.unwrap().recovery_key;
    h.state
        .upsert_entity("host", json!({"label": "web", "address": "10.0.0.5"}))
        .await
        .unwrap();

    // Off until enabled; commands that need an account say so.
    let off = h.state.sync_status().await;
    assert_eq!(off.status, SyncStateDto::Off);
    assert!(off.account.is_none());
    assert_eq!(h.state.sync_now().await.unwrap_err().code, "syncOff");
    assert_eq!(h.state.sync_devices().await.unwrap_err().code, "syncOff");
    assert_eq!(h.state.sync_link_code().await.unwrap_err().code, "syncOff");

    // The code says what it is and shows the server's fingerprint: exactly
    // the first line `tether-server fingerprint` prints, character for character.
    let code = srv.make_invite().await;
    let info = h.state.sync_parse_code(secret(&code)).unwrap();
    assert_eq!(info.kind, "invite");
    assert_eq!(info.server, srv.url);
    let printed = tether_server::cli::fingerprint_report(&srv.state.cfg).unwrap();
    assert_eq!(
        info.fingerprint.as_deref(),
        printed.lines().next(),
        "{printed}"
    );
    let shown = info.fingerprint.as_deref().unwrap();
    assert!(shown.starts_with("sha256:") && !shown.contains(' ') && shown.len() == 71);
    assert_eq!(
        h.state
            .sync_parse_code(secret("not a code"))
            .unwrap_err()
            .code,
        "invalidCode"
    );
    // A short code names no server yet; a bad address is its own error.
    let short = h
        .state
        .sync_parse_code(secret("TLX-ABCD-EFGH-JKMN-PQRS"))
        .unwrap();
    assert_eq!(
        (
            short.kind.as_str(),
            short.server.as_str(),
            short.fingerprint
        ),
        ("short", "", None)
    );

    // A wrong password or recovery key is refused before anything is sent;
    // an add-device code is refused for a vault that already exists.
    let err = h
        .state
        .sync_enable(
            invite(&srv).await,
            USER.into(),
            secret("not-the-password"),
            RecoveryInput::Existing(secret(&recovery)),
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, "wrongPassword");
    let err = h
        .state
        .sync_enable(
            invite(&srv).await,
            USER.into(),
            master(),
            RecoveryInput::Existing(secret("not-a-recovery-key")),
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, "wrongRecoveryKey");
    assert_eq!(h.state.sync_status().await.status, SyncStateDto::Off);

    enable(&srv, &h, &recovery).await;
    // (The engine may start another quiet cycle, e.g. after its socket
    // connects, so wait for an idle state rather than read one instant.)
    let status = wait_status(&h, "idle", |s| {
        matches!(s.status, SyncStateDto::Idle { .. })
    })
    .await;
    let account = status.account.expect("account");
    assert_eq!(account.username, USER);
    assert_eq!(account.server, srv.url);
    // The UI hears about it: status events, ending idle.
    let states = || -> Vec<String> {
        h.events
            .0
            .lock()
            .unwrap()
            .iter()
            .filter(|(n, _)| n == "sync-status")
            .map(|(_, p)| p["status"]["state"].as_str().unwrap().to_owned())
            .collect()
    };
    until("an idle status event", || async {
        states().last().map(String::as_str) == Some("idle")
    })
    .await;
    assert!(states().iter().any(|s| s == "syncing"), "{:?}", states());

    // Sync is on; enabling again is refused, and the server keeps the host.
    let err = h
        .state
        .sync_enable(
            invite(&srv).await,
            USER.into(),
            master(),
            RecoveryInput::Existing(secret(&recovery)),
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, "syncOn");

    // "I lost it": a new recovery key, shown once, replaces the old one.
    let srv2 = tether_server::testing::spawn_tls().await;
    let h2 = harness();
    let old = h2.state.vault_create(master()).await.unwrap().recovery_key;
    let out = h2
        .state
        .sync_enable(
            invite(&srv2).await,
            USER.into(),
            master(),
            RecoveryInput::New(true),
        )
        .await
        .unwrap();
    let fresh = out.new_recovery_key.expect("a new recovery key");
    assert_ne!(fresh, old);
    h2.state.vault_lock().await.unwrap();
    h2.state
        .vault_unlock_recovery(secret(&fresh), secret("after-recovery"))
        .await
        .unwrap();
    h2.state.stop_sync().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn sync_join_second_app_state_sees_hosts() {
    use sync::*;
    let p = pair().await;
    // B has A's host and identity (password stored, never shown).
    let hosts = p.b.state.list_entities("host").await.unwrap();
    assert_eq!(hosts[0]["label"], json!("web"));
    let identities = p.b.state.list_entities("identity").await.unwrap();
    assert_eq!(identities[0]["hasPassword"], json!(true));
    assert!(!Value::Array(identities).to_string().contains(CANARY));
    assert_eq!(p.b.state.vault_status().await.state, VaultState::Unlocked);
    assert!(p.b.state.sync_status().await.account.is_some());

    // Live edits travel on their own, without "Sync now", in both directions.
    p.a.state
        .upsert_entity(
            "host",
            json!({"id": p.host_id, "label": "web-renamed", "address": "10.0.0.5"}),
        )
        .await
        .unwrap();
    until("the rename on B", || async {
        p.b.state.list_entities("host").await.unwrap()[0]["label"] == json!("web-renamed")
    })
    .await;
    p.b.state.delete_entity("host", p.host_id).await.unwrap();
    until("the delete on A", || async {
        p.a.state.list_entities("host").await.unwrap().is_empty()
    })
    .await;

    // Devices: both listed, this one marked; rename; revoke.
    let devices = p.a.state.sync_devices().await.unwrap();
    assert_eq!(devices.len(), 2);
    assert_eq!(devices.iter().filter(|d| d.current).count(), 1);
    let other = devices.iter().find(|d| !d.current).unwrap();
    let other_id: Uuid = other.id.parse().unwrap();
    p.a.state
        .sync_rename_device(other_id, "Work laptop".into())
        .await
        .unwrap();
    let devices = p.a.state.sync_devices().await.unwrap();
    assert!(
        devices
            .iter()
            .any(|d| d.name == "Work laptop" && !d.current)
    );
    p.a.state.sync_revoke_device(other_id).await.unwrap();
    assert_eq!(p.a.state.sync_devices().await.unwrap().len(), 1);
    // The revoked device stops with a plain error; its vault still works.
    p.b.state.sync_now().await.unwrap();
    let status = wait_status(&p.b, "B revoked", |s| {
        matches!(
            s.status,
            SyncStateDto::Error {
                kind: SyncErrorKindDto::Revoked,
                ..
            }
        )
    })
    .await;
    let SyncStateDto::Error { message, .. } = status.status else {
        unreachable!()
    };
    assert_eq!(message, "device revoked");
    p.b.state.list_entities("host").await.unwrap();

    // A device that already has a vault is not silently replaced.
    let c = harness();
    c.state.vault_create(secret("c-pw")).await.unwrap();
    c.state
        .upsert_entity("host", json!({"label": "mine", "address": "h"}))
        .await
        .unwrap();
    let link = p.a.state.sync_link_code().await.unwrap();
    let code = relink(&link.code, &p.srv.url);
    // Refused before the single-use code is spent.
    let err = c
        .state
        .sync_join(secret(code.expose_secret()), USER.into(), master(), false)
        .await
        .unwrap_err();
    assert_eq!(err.code, "vaultExists");
    let joined = c
        .state
        .sync_join(code, USER.into(), master(), true)
        .await
        .unwrap();
    assert!(
        joined
            .backup
            .as_deref()
            .is_some_and(|b| std::path::Path::new(b).exists())
    );
    settled(&c).await;
    c.state.stop_sync().await;
    p.a.state.stop_sync().await;
}

#[tokio::test]
async fn vault_change_password_without_sync() {
    let h = harness();
    created(&h).await;
    let err = h
        .state
        .vault_change_password(secret("wrong"), secret("next-pw"))
        .await
        .unwrap_err();
    assert_eq!(err.code, "wrongPassword");
    h.state
        .vault_change_password(secret("master-pw"), secret("next-pw"))
        .await
        .unwrap();
    // The session stays open; the new password is what unlocks.
    assert_eq!(h.state.vault_status().await.state, VaultState::Unlocked);
    h.state.vault_lock().await.unwrap();
    assert_eq!(
        h.state
            .vault_unlock(secret("master-pw"))
            .await
            .unwrap_err()
            .code,
        "wrongPassword"
    );
    h.state.vault_unlock(secret("next-pw")).await.unwrap();
}

/// A change pulled from another device reaches the UI as `vault-changed`
/// (no payload), so open lists read again.
#[tokio::test(flavor = "multi_thread")]
async fn remote_changes_emit_vault_changed() {
    use sync::*;
    let p = pair().await;
    settled(&p.b).await;
    p.b.events.0.lock().unwrap().clear();
    p.a.state
        .upsert_entity(
            "host",
            json!({"id": p.host_id, "label": "web-renamed", "address": "10.0.0.5"}),
        )
        .await
        .unwrap();
    until("vault-changed on B", || async {
        p.b.events
            .0
            .lock()
            .unwrap()
            .iter()
            .any(|(n, payload)| n == "vault-changed" && *payload == json!({}))
    })
    .await;
    p.a.state.stop_sync().await;
    p.b.state.stop_sync().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn password_change_propagates_and_locks_the_other_device() {
    use sync::*;
    let p = pair().await;
    p.a.state
        .vault_change_password(master(), secret("CANARY-7f3a-next"))
        .await
        .unwrap();
    assert_eq!(p.a.state.vault_status().await.state, VaultState::Unlocked);
    // B locks itself and says why.
    // The vault locks, then the UI is told why.
    until("B locked by the change", || async {
        p.b.state.vault_status().await.state == VaultState::Locked
            && locked_event(&p.b, "passwordChanged")
    })
    .await;
    assert!(!locked_event(&p.a, "passwordChanged"));
    p.b.state
        .vault_unlock(secret("CANARY-7f3a-next"))
        .await
        .unwrap();
    assert_eq!(p.b.state.list_entities("host").await.unwrap().len(), 1);
    // Once the new password has opened B, the old one no longer does.
    p.b.state.vault_lock().await.unwrap();
    assert_eq!(
        p.b.state.vault_unlock(master()).await.unwrap_err().code,
        "wrongPassword"
    );
    p.a.state.stop_sync().await;
    p.b.state.stop_sync().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn unlock_fallback_after_remote_password_change() {
    use sync::*;
    // B reaches the server through a proxy that can be cut.
    let srv = tether_server::testing::spawn_tls().await;
    let a = harness();
    let recovery = a.state.vault_create(master()).await.unwrap().recovery_key;
    enable(&srv, &a, &recovery).await;
    let proxy = Proxy::start(srv.addr()).await;
    let b = harness();
    join(&proxy.url, &a, &b, master()).await;

    b.state.vault_lock().await.unwrap();
    proxy.down();
    a.state
        .vault_change_password(master(), secret("CANARY-7f3a-next"))
        .await
        .unwrap();
    // B has not heard of it, and cannot ask: the new password fails. The
    // fallback cannot reach the server.
    let err = b
        .state
        .vault_unlock(secret("CANARY-7f3a-next"))
        .await
        .unwrap_err();
    assert_eq!(err.code, "wrongPassword");
    assert_eq!(b.state.vault_status().await.state, VaultState::Locked);

    // Back online (B's own engine is still backing off): the failed unlock
    // asks the server for the current key material and retries once.
    proxy.up();
    b.state
        .vault_unlock(secret("CANARY-7f3a-next"))
        .await
        .unwrap();
    assert_eq!(b.state.vault_status().await.state, VaultState::Unlocked);
    // A wrong password stays wrong after the refresh.
    b.state.vault_lock().await.unwrap();
    assert_eq!(
        b.state.vault_unlock(master()).await.unwrap_err().code,
        "wrongPassword"
    );
    a.state.stop_sync().await;
    b.state.stop_sync().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn recovery_with_sync_changes_the_password_everywhere() {
    use sync::*;
    let p = pair().await;
    p.a.state.vault_lock().await.unwrap();
    let err =
        p.a.state
            .vault_unlock_recovery(secret("not-a-recovery-key"), secret("CANARY-7f3a-reset"))
            .await
            .unwrap_err();
    assert_eq!(err.code, "wrongRecoveryKey");
    p.a.state
        .vault_unlock_recovery(secret(&p.recovery), secret("CANARY-7f3a-reset"))
        .await
        .unwrap();
    assert_eq!(p.a.state.vault_status().await.state, VaultState::Unlocked);
    until("B locked by the reset", || async {
        p.b.state.vault_status().await.state == VaultState::Locked
            && locked_event(&p.b, "passwordChanged")
    })
    .await;
    p.b.state
        .vault_unlock(secret("CANARY-7f3a-reset"))
        .await
        .unwrap();
    p.a.state.stop_sync().await;
    p.b.state.stop_sync().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn sync_recover_on_a_new_device_then_join() {
    use sync::*;
    let p = pair().await;
    // A new device that forgot the password: any code from that server names
    // it (the code is not used up).
    let c = harness();
    let out = c
        .state
        .sync_recover(
            invite(&p.srv).await,
            USER.into(),
            secret(&p.recovery),
            secret("CANARY-7f3a-reset"),
        )
        .await
        .unwrap();
    assert!(out.needs_device_code);
    assert_eq!(out.server, p.srv.url);
    // Nothing was written here; the next step is an add-device code.
    assert_eq!(
        c.state.vault_status().await.state,
        VaultState::Uninitialized
    );
    let err = c
        .state
        .sync_recover(
            invite(&p.srv).await,
            USER.into(),
            secret("not-a-recovery-key"),
            secret("x"),
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, "wrongRecoveryKey");

    // The other devices pick the change up (they lock with the message).
    until("A locked by the reset", || async {
        p.a.state.vault_status().await.state == VaultState::Locked
    })
    .await;
    p.a.state
        .vault_unlock(secret("CANARY-7f3a-reset"))
        .await
        .unwrap();
    let link = p.a.state.sync_link_code().await.unwrap();
    c.state
        .sync_join(
            relink(&link.code, &p.srv.url),
            USER.into(),
            secret("CANARY-7f3a-reset"),
            false,
        )
        .await
        .unwrap();
    until("the host on the new device", || async {
        !c.state.list_entities("host").await.unwrap().is_empty()
    })
    .await;
    for h in [&p.a, &p.b, &c] {
        h.state.stop_sync().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn sync_turn_off_keeps_the_records() {
    use sync::*;
    let p = pair().await;
    p.b.state.sync_turn_off().await.unwrap();
    let status = p.b.state.sync_status().await;
    assert_eq!(status.status, SyncStateDto::Off);
    assert!(status.account.is_none());
    assert_eq!(p.b.state.list_entities("host").await.unwrap().len(), 1);
    assert_eq!(p.b.state.sync_now().await.unwrap_err().code, "syncOff");
    // Local edits no longer travel.
    p.b.state
        .upsert_entity("host", json!({"label": "local only", "address": "h"}))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(900)).await;
    assert_eq!(p.a.state.list_entities("host").await.unwrap().len(), 1);
    // Turning sync off ends the session but does not revoke the device: the
    // server lists it until another device removes it.
    let devices = p.a.state.sync_devices().await.unwrap();
    assert_eq!(devices.len(), 2);
    let gone: Uuid = devices
        .iter()
        .find(|d| !d.current)
        .unwrap()
        .id
        .parse()
        .unwrap();
    p.a.state.sync_revoke_device(gone).await.unwrap();
    assert_eq!(p.a.state.sync_devices().await.unwrap().len(), 1);
    p.a.state.stop_sync().await;
}

/// Two devices edit the same identity while one is not syncing; the loser
/// is kept as a conflict whose secrets show as markers.
async fn conflicted() -> sync::Pair {
    use sync::*;
    let p = pair().await;
    p.b.state.stop_sync().await;
    p.b.state
        .upsert_entity(
            "identity",
            json!({"id": p.identity_id, "label": "ops-b", "username": "root",
                   "password": format!("{CANARY}-from-b")}),
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(5)).await;
    p.a.state
        .upsert_entity(
            "identity",
            json!({"id": p.identity_id, "label": "ops-a", "username": "root",
                   "password": format!("{CANARY}-from-a")}),
        )
        .await
        .unwrap();
    settled(&p.a).await;
    p.b.state.start_sync().await;
    until("the conflict on B", || async {
        !p.b.state.conflicts_list().await.unwrap().is_empty()
    })
    .await;
    p
}

#[tokio::test(flavor = "multi_thread")]
async fn conflicts_list_masks_secrets() {
    use sync::*;
    let p = conflicted().await;
    let list = p.b.state.conflicts_list().await.unwrap();
    assert_eq!(list.len(), 1);
    let c = &list[0];
    assert_eq!(c.kind, "identity");
    assert_eq!(c.record_id, p.identity_id);
    assert!(c.changed_fields.contains(&"password".to_owned()));
    assert!(c.changed_fields.contains(&"label".to_owned()));
    // Secrets are markers on both sides; nothing in the DTO has the canary.
    for side in [&c.current, &c.other] {
        assert_eq!(
            side.as_ref().unwrap()["password"],
            json!({"hasSecret": true})
        );
    }
    let text = serde_json::to_string(&list).unwrap();
    assert!(!text.contains(CANARY), "{text}");
    // The later edit (A's) won; B's version is the other side.
    assert_eq!(c.current.as_ref().unwrap()["label"], json!("ops-a"));
    assert_eq!(c.other.as_ref().unwrap()["label"], json!("ops-b"));

    // Reveal is the one way a secret comes out, one field at a time, and
    // only for secret fields.
    let reveal = |side, field: &str| {
        let state = p.b.state.clone();
        let (id, field) = (c.id, field.to_owned());
        async move { state.conflicts_reveal(id, side, &field).await }
    };
    assert_eq!(
        reveal(ConflictSideDto::Other, "password")
            .await
            .unwrap()
            .expose_secret(),
        format!("{CANARY}-from-b")
    );
    assert_eq!(
        reveal(ConflictSideDto::Current, "password")
            .await
            .unwrap()
            .expose_secret(),
        format!("{CANARY}-from-a")
    );
    assert_eq!(
        reveal(ConflictSideDto::Other, "label")
            .await
            .unwrap_err()
            .code,
        "invalid"
    );

    // Pick one field from the other version; the conflict is dismissed.
    p.b.state
        .conflicts_resolve(c.id, ResolutionDto::Fields(vec!["password".into()]))
        .await
        .unwrap();
    assert!(p.b.state.conflicts_list().await.unwrap().is_empty());
    let identity =
        p.b.vault
            .lock()
            .unwrap()
            .get::<core_model::Identity>(p.identity_id)
            .unwrap()
            .unwrap();
    assert_eq!(identity.label, "ops-a");
    assert_eq!(
        identity.password.as_ref().unwrap().expose_secret(),
        &format!("{CANARY}-from-b")
    );
    assert_eq!(
        p.b.state
            .conflicts_resolve(c.id, ResolutionDto::KeepCurrent)
            .await
            .unwrap_err()
            .code,
        "notFound"
    );
    p.a.state.stop_sync().await;
    p.b.state.stop_sync().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn conflict_use_other_and_keep_current() {
    use sync::*;
    let p = conflicted().await;
    let id = p.b.state.conflicts_list().await.unwrap()[0].id;
    p.b.state
        .conflicts_resolve(id, ResolutionDto::UseOther)
        .await
        .unwrap();
    let identity = |h: &Harness| {
        h.vault
            .lock()
            .unwrap()
            .get::<core_model::Identity>(p.identity_id)
            .unwrap()
            .unwrap()
    };
    assert_eq!(identity(&p.b).label, "ops-b");
    // The resolution is a normal edit: it reaches A.
    until("the resolution on A", || async {
        identity(&p.a).label == "ops-b"
    })
    .await;
    assert!(p.b.state.conflicts_list().await.unwrap().is_empty());
    p.a.state.stop_sync().await;
    p.b.state.stop_sync().await;
}

/// No DTO, event or error of the sync commands carries a password, whatever
/// the scenario touches.
#[tokio::test(flavor = "multi_thread")]
async fn dto_never_contains_password_canary() {
    use sync::*;
    let p = conflicted().await;
    let mut seen: Vec<String> = Vec::new();

    seen.push(serde_json::to_string(&p.a.state.sync_status().await).unwrap());
    seen.push(serde_json::to_string(&p.b.state.sync_status().await).unwrap());
    seen.push(serde_json::to_string(&p.b.state.conflicts_list().await.unwrap()).unwrap());
    seen.push(serde_json::to_string(&p.a.state.sync_devices().await.unwrap()).unwrap());
    seen.push(serde_json::to_string(&p.a.state.sync_link_code().await.unwrap()).unwrap());
    seen.push(serde_json::to_string(&p.b.state.vault_status().await).unwrap());
    seen.push(Value::Array(p.b.state.list_entities("identity").await.unwrap()).to_string());
    // Failures name no secret either.
    let errors = vec![
        p.b.state
            .vault_change_password(secret("CANARY-7f3a-wrong"), secret("CANARY-7f3a-x"))
            .await
            .unwrap_err(),
        p.b.state
            .sync_enable(
                secret("tether1-CANARY-7f3a"),
                USER.into(),
                secret("CANARY-7f3a-pw"),
                RecoveryInput::New(true),
            )
            .await
            .map_err(CmdError::from)
            .unwrap_err(),
        p.b.state
            .sync_recover(
                secret("CANARY-7f3a-code"),
                USER.into(),
                secret("CANARY-7f3a-key"),
                secret("CANARY-7f3a-new"),
            )
            .await
            .unwrap_err(),
        p.b.state
            .sync_join(
                secret("CANARY-7f3a-code"),
                USER.into(),
                secret("CANARY-7f3a-pw"),
                true,
            )
            .await
            .unwrap_err(),
    ];
    for e in errors {
        seen.push(format!("{e:?}"));
        seen.push(serde_json::to_string(&e).unwrap());
    }
    // Everything the UI was told, on both devices.
    for h in [&p.a, &p.b] {
        for (name, payload) in h.events.0.lock().unwrap().iter() {
            seen.push(format!("{name} {payload}"));
        }
    }
    assert!(seen.len() > 12);
    for text in &seen {
        assert!(!text.contains(CANARY), "{text}");
    }
    p.a.state.stop_sync().await;
    p.b.state.stop_sync().await;
}

#[test]
fn sync_error_codes_are_distinct_and_plain() {
    use core_sync::SyncError;
    use tether_app_lib::sync_cmds::cmd_error;
    let cases: Vec<(SyncError, &str, &str)> = vec![
        (
            SyncError::CertificateChanged,
            "certificateChanged",
            "server certificate changed",
        ),
        (SyncError::Revoked, "revoked", "device revoked"),
        (SyncError::UpdateClient, "updateRequired", "update Termlyx"),
        (
            SyncError::Unreachable("connection refused (os error 61)".into()),
            "unreachable",
            "server unreachable (connection refused (os error 61))",
        ),
        (SyncError::WrongPassword, "wrongPassword", "wrong password"),
        (
            SyncError::InvalidCode("bad checksum".into()),
            "invalidCode",
            "invalid code: bad checksum",
        ),
        (
            SyncError::ClockSkew,
            "clockSkew",
            "device clock is off; check the date and time",
        ),
        (
            SyncError::RateLimited,
            "rateLimited",
            "too many attempts; try again later",
        ),
        (
            SyncError::Storage("no secret service".into()),
            "storage",
            "could not access stored credentials: no secret service",
        ),
        (
            SyncError::Server {
                code: core_proto::ErrorCode::Internal,
                message: "boom".into(),
            },
            "server",
            "boom",
        ),
        (
            SyncError::VaultExists,
            "vaultExists",
            "this device already has a vault",
        ),
        (SyncError::Locked, "locked", "the vault is locked"),
        (
            SyncError::NeedsDeviceCode,
            "needsDeviceCode",
            "this device must be added to the account again with an add-device code",
        ),
        (
            SyncError::Protocol("not a Termlyx server".into()),
            "protocol",
            "unexpected response from the server: not a Termlyx server",
        ),
    ];
    let mut codes = BTreeSet::new();
    for (err, code, message) in cases {
        let mapped = cmd_error(err);
        assert_eq!(
            (mapped.code.as_str(), mapped.message.as_str()),
            (code, message)
        );
        codes.insert(code);
    }
    assert_eq!(codes.len(), 14, "every case has its own code");
}

/// Turning sync off holds the engine slot throughout: an unlock that races it
/// must not start a second engine on the device's credentials.
#[tokio::test(flavor = "multi_thread")]
async fn turn_off_is_not_raced_by_a_concurrent_unlock() {
    use sync::*;
    let p = pair().await;
    p.b.state.vault_lock().await.unwrap();
    // Everything that starts the engine (an unlock, the setup task, a join)
    // is `start_sync`; it keeps trying all through the turn-off.
    let done = std::sync::atomic::AtomicBool::new(false);
    let starter = async {
        while !done.load(Ordering::SeqCst) {
            p.b.state.start_sync().await;
            tokio::task::yield_now().await;
        }
    };
    let turn_off = async {
        let r = p.b.state.sync_turn_off().await;
        done.store(true, Ordering::SeqCst);
        r
    };
    let (off, ()) = tokio::join!(turn_off, starter);
    // And an unlock right after finds sync off.
    p.b.state.vault_unlock(master()).await.unwrap();
    off.unwrap();
    // Off, with no engine left running on the logged-out credentials.
    assert_eq!(p.b.state.sync_status().await.status, SyncStateDto::Off);
    assert_eq!(p.b.state.sync_now().await.unwrap_err().code, "syncOff");
    // The server never saw a second session: the device is still in good standing.
    assert_eq!(p.a.state.sync_devices().await.unwrap().len(), 2);
    p.a.state.stop_sync().await;
}

/// Replacing a vault stops the old engine and starts the new one under one
/// hold of the slot: nothing restarts the old vault's engine in between.
#[tokio::test(flavor = "multi_thread")]
async fn join_replace_with_a_running_engine_ends_on_the_new_account() {
    use sync::*;
    let srv = tether_server::testing::spawn_tls().await;
    let a = harness();
    let a_recovery = a.state.vault_create(master()).await.unwrap().recovery_key;
    a.state
        .upsert_entity(
            "host",
            json!({"label": "from-alice", "address": "10.0.0.5"}),
        )
        .await
        .unwrap();
    enable(&srv, &a, &a_recovery).await;

    // C syncs as carol, with its own vault and a running engine.
    let c = harness();
    let c_recovery = c.state.vault_create(master()).await.unwrap().recovery_key;
    c.state
        .sync_enable(
            invite(&srv).await,
            "carol".into(),
            master(),
            RecoveryInput::Existing(secret(&c_recovery)),
        )
        .await
        .unwrap();
    settled(&c).await;
    let carol_device = c.state.sync_status().await.account.unwrap().device_id;

    // C replaces its vault with alice's while unlocks keep arriving.
    let link = a.state.sync_link_code().await.unwrap();
    let code = relink(&link.code, &srv.url);
    let unlocker = async {
        for _ in 0..25 {
            tokio::time::sleep(Duration::from_millis(2)).await;
            let _ = c.state.vault_unlock(master()).await;
        }
    };
    let (joined, ()) = tokio::join!(
        c.state.sync_join(code, USER.into(), master(), true),
        unlocker
    );
    joined.unwrap();

    // One engine, on the new account and the new vault.
    let status = wait_status(&c, "an idle sync as alice", |s| {
        s.account.as_ref().is_some_and(|a| a.username == USER)
            && matches!(s.status, SyncStateDto::Idle { .. })
    })
    .await;
    assert_ne!(status.account.unwrap().device_id, carol_device);
    until("alice's host on C", || async {
        c.state.list_entities("host").await.unwrap().len() == 1
    })
    .await;
    assert_eq!(c.state.sync_devices().await.unwrap().len(), 2);
    c.state
        .upsert_entity("host", json!({"label": "from-c", "address": "h"}))
        .await
        .unwrap();
    until("C's host on A", || async {
        a.state.list_entities("host").await.unwrap().len() == 2
    })
    .await;
    a.state.stop_sync().await;
    c.state.stop_sync().await;
}

const SYNC_ERROR_CASES: &str = include_str!("../../src/api/sync-error-codes.json");

/// Precondition and server failures reject with the same codes the mock
/// gives (`contract.test.ts` checks the mock against the same file).
#[tokio::test(flavor = "multi_thread")]
async fn sync_failures_use_the_codes_the_mock_documents() {
    use sync::*;
    let expected: BTreeMap<String, String> = serde_json::from_str(SYNC_ERROR_CASES).unwrap();
    let mut got: BTreeMap<String, String> = BTreeMap::new();
    let code_of = |e: CmdError| e.code;

    // Sync off.
    let off = harness();
    got.insert(
        "nowWhileOff".into(),
        code_of(off.state.sync_now().await.unwrap_err()),
    );
    got.insert(
        "devicesWhileOff".into(),
        code_of(off.state.sync_devices().await.unwrap_err()),
    );
    got.insert(
        "signInWhileOff".into(),
        code_of(off.state.sync_sign_in(master()).await.unwrap_err()),
    );
    got.insert(
        "parseGarbageCode".into(),
        code_of(off.state.sync_parse_code(secret("not a code")).unwrap_err()),
    );

    got.insert(
        "checkServerBadAddress".into(),
        code_of(
            off.state
                .sync_check_server("bad address/".into(), secret("TLX-ABCD-EFGH-JKMN-PQRS"))
                .await
                .unwrap_err(),
        ),
    );

    // Vault preconditions of turning sync on.
    let srv = tether_server::testing::spawn_tls().await;
    async fn enable(h: &Harness, code: SecretString) -> Result<EnableOutcome, CmdError> {
        h.state
            .sync_enable(
                code,
                sync::USER.into(),
                sync::master(),
                RecoveryInput::New(true),
            )
            .await
            .map_err(CmdError::from)
    }
    got.insert(
        "enableBeforeCreate".into(),
        code_of(enable(&off, invite(&srv).await).await.unwrap_err()),
    );
    off.state.vault_create(master()).await.unwrap();
    off.state.vault_lock().await.unwrap();
    got.insert(
        "enableWhileLocked".into(),
        code_of(enable(&off, invite(&srv).await).await.unwrap_err()),
    );
    off.state.vault_unlock(master()).await.unwrap();

    // With an account: an add-device code cannot turn sync on; a vault of
    // its own is not replaced; unknown devices are not found.
    let p = pair().await;
    let add_device = relink(&p.a.state.sync_link_code().await.unwrap().code, &p.srv.url);
    got.insert(
        "linkCodeBadAddress".into(),
        code_of(
            p.a.state
                .sync_link_code_at(Some("a b".into()))
                .await
                .unwrap_err(),
        ),
    );
    got.insert(
        "enableWithAddDeviceCode".into(),
        code_of(enable(&off, add_device).await.unwrap_err()),
    );
    let add_device = relink(&p.a.state.sync_link_code().await.unwrap().code, &p.srv.url);
    got.insert(
        "joinOverExistingVault".into(),
        code_of(
            off.state
                .sync_join(add_device, USER.into(), master(), false)
                .await
                .unwrap_err(),
        ),
    );
    let unknown = Uuid::now_v7();
    got.insert(
        "renameUnknownDevice".into(),
        code_of(
            p.a.state
                .sync_rename_device(unknown, "x".into())
                .await
                .unwrap_err(),
        ),
    );
    got.insert(
        "revokeUnknownDevice".into(),
        code_of(p.a.state.sync_revoke_device(unknown).await.unwrap_err()),
    );
    p.a.state.stop_sync().await;
    p.b.state.stop_sync().await;

    assert_eq!(got, expected);
}

fn signing_in(s: &SyncStatusDto) -> bool {
    matches!(
        s.status,
        SyncStateDto::Error {
            kind: SyncErrorKindDto::SignInAgain,
            ..
        }
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn sign_in_again_after_the_session_is_lost() {
    use sync::*;
    let srv = tether_server::testing::spawn_tls().await;
    let a = harness();
    let recovery = a.state.vault_create(master()).await.unwrap().recovery_key;
    enable(&srv, &a, &recovery).await;
    let device = a.state.sync_status().await.account.unwrap().device_id;

    // The session is gone: sync stops with "sign in again".
    srv.end_all_sessions().await;
    a.state.sync_now().await.unwrap();
    wait_status(&a, "sign in again", signing_in).await;

    // A wrong password is refused; the password signs the device back in.
    let err = a.state.sync_sign_in(secret("nope")).await.unwrap_err();
    assert_eq!(err.code, "wrongPassword");
    a.state.sync_sign_in(master()).await.unwrap();
    settled(&a).await;

    // Unlocking with the password while sync waits for it signs in by
    // itself.
    srv.end_all_sessions().await;
    a.state.sync_now().await.unwrap();
    wait_status(&a, "sign in again", signing_in).await;
    a.state.vault_lock().await.unwrap();
    a.state.vault_unlock(master()).await.unwrap();
    settled(&a).await;
    a.state
        .upsert_entity("host", json!({"label": "after", "address": "10.0.0.9"}))
        .await
        .unwrap();
    settled(&a).await;
    assert_eq!(
        a.state.sync_status().await.account.unwrap().device_id,
        device
    );

    // Without an engine there is nothing to sign in.
    a.state.stop_sync().await;
    assert_eq!(
        a.state.sync_sign_in(master()).await.unwrap_err().code,
        "syncUnavailable"
    );
    let off = harness();
    assert_eq!(
        off.state.sync_sign_in(master()).await.unwrap_err().code,
        "syncOff"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn rejoining_the_same_vault_keeps_its_records() {
    use sync::*;
    let p = pair().await;
    p.b.state.sync_turn_off().await.unwrap();
    p.b.state
        .upsert_entity("host", json!({"label": "while off", "address": "h"}))
        .await
        .unwrap();
    // The same vault: no replace warning (`vaultExists`), no backup, records kept.
    let link = p.a.state.sync_link_code().await.unwrap();
    let out =
        p.b.state
            .sync_join(relink(&link.code, &p.srv.url), USER.into(), master(), false)
            .await
            .unwrap();
    assert_eq!(
        out,
        JoinOutcome {
            backup: None,
            reconnected: true
        }
    );
    settled(&p.b).await;
    until("B's edit on A", || async {
        p.a.state.list_entities("host").await.unwrap().len() == 2
    })
    .await;
    assert_eq!(p.b.state.list_entities("host").await.unwrap().len(), 2);
    p.a.state.stop_sync().await;
    p.b.state.stop_sync().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_replace_warning_reuses_the_spent_code() {
    use sync::*;
    let p = pair().await;
    // C has a vault of its own: the first try is refused without changing
    // anything, and the confirmed retry uses the same (now spent) code.
    let c = harness();
    c.state.vault_create(master()).await.unwrap();
    c.state
        .upsert_entity("host", json!({"label": "only on c", "address": "h"}))
        .await
        .unwrap();
    let link = p.a.state.sync_link_code().await.unwrap();
    let code = || relink(&link.code, &p.srv.url);
    let err = c
        .state
        .sync_join(code(), USER.into(), master(), false)
        .await
        .unwrap_err();
    assert_eq!(err.code, "vaultExists");
    assert_eq!(c.state.list_entities("host").await.unwrap().len(), 1);
    let out = c
        .state
        .sync_join(code(), USER.into(), master(), true)
        .await
        .unwrap();
    assert!(!out.reconnected && out.backup.is_some());
    settled(&c).await;
    until("A's host on C", || async {
        let hosts = c.state.list_entities("host").await.unwrap();
        hosts.len() == 1 && hosts[0]["label"] == json!("web")
    })
    .await;
    // A, B and C: no device left behind by the refused try.
    assert_eq!(p.a.state.sync_devices().await.unwrap().len(), 3);
    for h in [&p.a, &p.b, &c] {
        h.state.stop_sync().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn recovery_unlock_with_sync_on_but_no_engine_is_refused() {
    use sync::*;
    let srv = tether_server::testing::spawn_tls().await;
    let a = harness();
    let recovery = a.state.vault_create(master()).await.unwrap().recovery_key;
    enable(&srv, &a, &recovery).await;
    // Sync is on, but the engine is not running (it failed to start).
    a.state.stop_sync().await;
    a.state.vault_lock().await.unwrap();
    let err = a
        .state
        .vault_unlock_recovery(secret(&recovery), secret("CANARY-7f3a-reset"))
        .await
        .unwrap_err();
    assert_eq!(err.code, "syncUnavailable");
    // Nothing changed locally: the account and this device keep one password.
    assert_eq!(a.state.vault_status().await.state, VaultState::Locked);
    assert_eq!(
        a.state
            .vault_unlock(secret("CANARY-7f3a-reset"))
            .await
            .unwrap_err()
            .code,
        "wrongPassword"
    );
    a.state.vault_unlock(master()).await.unwrap();
    a.state.stop_sync().await;
}

// ---------------------------------------------------------------------------
// Auto-lock

fn idle_locks(h: &Harness) -> usize {
    h.events
        .0
        .lock()
        .unwrap()
        .iter()
        .filter(|(n, p)| n == "vault-locked" && p["reason"] == json!("idle"))
        .count()
}

fn after(at: Moment, d: Duration) -> Moment {
    Moment {
        mono: at.mono + d,
        wall: at.wall + d,
    }
}

const MINUTE: Duration = Duration::from_secs(60);

#[tokio::test]
async fn auto_lock_locks_on_return_and_keeps_sessions() {
    let h = harness();
    created(&h).await;
    let (session, _sink) = open_fake(&h, "main", "web-1").await;
    let slot = h.state.autolock();
    slot.set_minutes(Some(5)).unwrap();

    let t0 = Moment::now();
    assert!(
        !slot.front(&h.state, false, t0).await,
        "still unlocked while away"
    );
    // Back after 6 minutes: the answer comes after the lock.
    assert!(slot.front(&h.state, true, after(t0, 6 * MINUTE)).await);
    assert_eq!(h.state.vault_status().await.state, VaultState::Locked);
    assert_eq!(idle_locks(&h), 1);
    // Like Lock vault: the session keeps running under the unlock screen.
    let work = h.state.active_work("main");
    assert_eq!(work.sessions.len(), 1);
    assert_eq!(work.sessions[0].id, session);

    // An absence that ends while the vault is already locked says nothing.
    let t1 = after(t0, 7 * MINUTE);
    slot.front(&h.state, false, t1).await;
    assert!(slot.front(&h.state, true, after(t1, 6 * MINUTE)).await);
    assert_eq!(idle_locks(&h), 1);
}

#[tokio::test]
async fn auto_lock_short_absence_or_never_keeps_the_vault_open() {
    let h = harness();
    created(&h).await;
    let slot = h.state.autolock();
    let t0 = Moment::now();
    // Never (the desktop default).
    slot.front(&h.state, false, t0).await;
    assert!(!slot.front(&h.state, true, after(t0, 600 * MINUTE)).await);
    // 5 minutes, back after 4.
    slot.set_minutes(Some(5)).unwrap();
    slot.front(&h.state, false, t0).await;
    assert!(!slot.front(&h.state, true, after(t0, 4 * MINUTE)).await);
    assert_eq!(h.state.vault_status().await.state, VaultState::Unlocked);
    assert_eq!(slot.set_minutes(Some(0)).unwrap_err().code, "invalid");
}

/// The window's focus event sees the return first and only records the
/// owed lock: the webview's `app_front(true)` right after must still answer
/// "locked", never "unlocked" while that lock is pending.
#[tokio::test]
async fn auto_lock_owed_by_a_focus_event_is_in_place_before_app_front_answers() {
    let h = harness();
    created(&h).await;
    let slot = h.state.autolock();
    slot.set_minutes(Some(5)).unwrap();
    let t0 = Moment::now();
    slot.note_focus(false, t0);
    assert!(slot.note_focus(true, after(t0, 6 * MINUTE)), "lock owed");
    // The background settle has not run yet.
    assert_eq!(h.state.vault_status().await.state, VaultState::Unlocked);
    assert!(slot.front(&h.state, true, after(t0, 6 * MINUTE)).await);
    assert_eq!(h.state.vault_status().await.state, VaultState::Locked);
    // The late settle finds nothing more to do.
    slot.settle(&h.state).await;
    assert_eq!(idle_locks(&h), 1);
}

#[tokio::test]
async fn auto_lock_timer_locks_while_away_once() {
    let h = harness();
    created(&h).await;
    let slot = h.state.autolock();
    slot.set_minutes(Some(5)).unwrap();
    let t0 = Moment::now();
    slot.note_focus(false, t0);
    assert!(!slot.note_due(after(t0, 4 * MINUTE)));
    assert!(slot.note_due(after(t0, 5 * MINUTE)));
    slot.settle(&h.state).await;
    assert_eq!(h.state.vault_status().await.state, VaultState::Locked);
    // Locked once for this absence.
    assert!(!slot.note_due(after(t0, 30 * MINUTE)));
    // Unlocked while the window still reads unfocused (a Touch ID prompt had
    // the focus, say): the absence starts over, and locks a full limit later.
    h.state.vault_unlock(secret("master-pw")).await.unwrap();
    let t1 = Moment::now();
    assert!(!slot.note_due(after(t1, 4 * MINUTE)));
    assert!(slot.note_due(after(t1, 6 * MINUTE)));
    slot.settle(&h.state).await;
    assert_eq!(h.state.vault_status().await.state, VaultState::Locked);
    assert_eq!(idle_locks(&h), 2);
}

/// Focus lost, Tether covered, then uncovered but still unfocused: still
/// away. The page being shown past the limit locks, and answers so.
#[tokio::test]
async fn auto_lock_shown_again_unfocused_is_still_away() {
    let h = harness();
    created(&h).await;
    let slot = h.state.autolock();
    slot.set_minutes(Some(5)).unwrap();
    let t0 = Moment::now();
    slot.note_focus(false, t0);
    assert!(!slot.front(&h.state, false, after(t0, MINUTE)).await);
    // Shown again after 2 minutes, no focus: the absence goes on.
    assert!(!slot.front(&h.state, true, after(t0, 2 * MINUTE)).await);
    assert!(
        slot.note_due(after(t0, 5 * MINUTE)),
        "the timer still sees it away"
    );
    slot.settle(&h.state).await;
    assert_eq!(h.state.vault_status().await.state, VaultState::Locked);

    // Same, but the page is shown only after the limit: that report locks.
    h.state.vault_unlock(secret("master-pw")).await.unwrap();
    let t1 = Moment::now();
    slot.note_focus(true, t1);
    slot.note_focus(false, t1);
    slot.front(&h.state, false, after(t1, MINUTE)).await;
    assert!(slot.front(&h.state, true, after(t1, 6 * MINUTE)).await);
    assert_eq!(h.state.vault_status().await.state, VaultState::Locked);
    assert_eq!(idle_locks(&h), 2);
}

#[tokio::test]
async fn idle_lock_keeps_a_password_changed_reason() {
    let h = harness();
    created(&h).await;
    h.state.vault_lock().await.unwrap();
    let before = h.events.names().len();
    // An auto-lock of a locked vault: no second event to replace the one the UI shows.
    let slot = h.state.autolock();
    slot.set_minutes(Some(1)).unwrap();
    let t0 = Moment::now();
    slot.note_focus(false, t0);
    assert!(slot.front(&h.state, true, after(t0, 2 * MINUTE)).await);
    assert_eq!(h.events.names().len(), before);
}

#[test]
fn snippet_dto_debug_hides_the_command() {
    let dto = SnippetDto {
        id: Uuid::now_v7(),
        label: "Login".into(),
        text: "sshpass -p hunter2-canary ssh root@box".into(),
        run: false,
    };
    let debug = format!("{dto:?}");
    assert!(debug.contains("Login"), "{debug}");
    assert!(!debug.contains("hunter2-canary"), "{debug}");
}
