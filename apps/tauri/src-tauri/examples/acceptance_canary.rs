//! Phase 1 acceptance canary, driven by `scripts/acceptance.sh`.
//!
//! Creates a real on-disk vault in `<app-data-dir>` exactly as the app does
//! (`open_vault`: production Argon2 parameters, OS-keychain key store, never
//! touched here because quick unlock stays off) and fills it through the
//! same `AppState` methods the `#[tauri::command]` wrappers call:
//! `vault_create`, `vault_lock`, `vault_unlock`, `key_generate` (one key
//! with a passphrase, one without), `upsert_entity` (an identity with a
//! password and a host labelled with the canary), `list_entities`,
//! `key_export_private`, and a password saved the way a connect prompt's
//! "save" stores it (`save_host_password` through `with_vault`, on a second
//! host without an identity). It does not cover the webview -> IPC hop.
//!
//! Phase 2: when `TETHER_CANARY_SYNC_INVITE` (an invite code) and
//! `TETHER_CANARY_SYNC_USER` are set, it then turns sync on through
//! `AppState::sync_enable` (the command behind "Turn on sync", with an
//! in-memory credential store so the Keychain stays untouched), waits for
//! the first sync to finish, and the script greps what the real server
//! container holds (its database, WAL, a backup, its logs). With a third
//! argument, a directory the script fills with those server files, this
//! program also counts its in-memory secrets (recovery key, private keys)
//! in them.
//!
//! The script greps the vault file, its `-wal` and `-shm`, and this
//! program's own output for the canary and the secrets it passed in. Some
//! secrets only exist in memory here (the recovery key, the generated
//! private keys, a public key blob); this program counts those itself and
//! prints only the counts.
//!
//! Handshake with the script (files in `<handshake-dir>`):
//! 1. writes `open` once everything is stored (and synced), with the vault
//!    still open (WAL not checkpointed), then waits for `release`;
//! 2. stops the engine, drops the state (closing the database), reopens the
//!    vault and checks
//!    the canary host is there (so a clean grep means "encrypted", not
//!    "never written"), drops it again and exits 0.
//!
//! Secrets come from the environment (`TETHER_CANARY_*`) and are never
//! printed: everything this program prints is grepped too.
//!
//! Run: `cargo run -p tether-app --example acceptance_canary -- <app-data-dir> <handshake-dir> [<server-files-dir>]`

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use secrecy::SecretString;
use serde_json::{Value, json};
use tether_app_lib::state::{AppState, CmdError, EventSink, VaultSlot};
use tether_app_lib::sync_cmds::{RecoveryInput, SyncStateDto};
use tether_app_lib::vault_cmds::{KeyAlgoDto, VaultState};
use tether_app_lib::{VAULT_FILE, init_tracing, open_vault, prompt_cmds};

/// How long to wait for the script's `release`.
const RELEASE_TIMEOUT: Duration = Duration::from_secs(300);
/// How long the first sync (upload of the whole vault) may take.
const SYNC_TIMEOUT: Duration = Duration::from_secs(120);

/// Prints every event as the app would send it to the webview, so the
/// script's log grep covers event payloads too.
struct PrintEvents;

impl EventSink for PrintEvents {
    fn emit(&self, event: &str, payload: Value) -> Result<(), String> {
        println!("[canary] event {event}: {payload}");
        Ok(())
    }
}

/// Fails with a message that never contains a secret (`CmdError` carries
/// none; `what` is ours).
fn check<T>(what: &str, r: Result<T, CmdError>) -> Result<T, String> {
    r.map_err(|e| format!("{what} failed: {e}"))
}

fn env_secret(name: &str) -> Result<String, String> {
    std::env::var(name).map_err(|_| format!("{name} is not set"))
}

struct Inputs {
    label: String,
    master: String,
    identity_password: String,
    key_passphrase: String,
    saved_password: String,
    /// Turn sync on with this invite code and account name, when both are set.
    sync: Option<(String, String)>,
}

/// The secrets this program learns at run time (the script never sees
/// them): checked in-process, only counts are printed.
#[derive(Default)]
struct MemNeedles(Vec<(&'static str, Vec<u8>)>);

impl MemNeedles {
    fn add(&mut self, name: &'static str, value: &str) {
        if value.len() >= 16 {
            self.0.push((name, value.as_bytes().to_vec()));
        }
    }

    /// Every base64 body line of an OpenSSH private key (each is unique to
    /// the key; the BEGIN/END armour is not).
    fn add_private_key(&mut self, name: &'static str, pem: &str) {
        for line in pem.lines().map(str::trim) {
            if !line.starts_with("-----") && line.len() >= 16 {
                self.add(name, line);
            }
        }
    }
}

fn count(hay: &[u8], needle: &[u8]) -> usize {
    if needle.is_empty() || hay.len() < needle.len() {
        return 0;
    }
    hay.windows(needle.len()).filter(|w| *w == needle).count()
}

/// Counts the in-memory needles in `files` (missing ones count as empty) and
/// prints one line per needle kind.
fn scan_files(phase: &str, files: &[PathBuf], needles: &MemNeedles) -> usize {
    let contents: Vec<Vec<u8>> = files
        .iter()
        .map(|f| std::fs::read(f).unwrap_or_default())
        .collect();
    let mut total = 0;
    let mut by_kind: Vec<(&str, usize, usize)> = Vec::new();
    for (name, needle) in &needles.0 {
        let n: usize = contents.iter().map(|c| count(c, needle)).sum();
        total += n;
        match by_kind.iter_mut().find(|(k, _, _)| k == name) {
            Some(entry) => {
                entry.1 += 1;
                entry.2 += n;
            }
            None => by_kind.push((name, 1, n)),
        }
    }
    let sizes: Vec<String> = files
        .iter()
        .zip(&contents)
        .map(|(f, c)| {
            let name = f.file_name().unwrap_or_default().to_string_lossy();
            if f.exists() {
                format!("{name}={}B", c.len())
            } else {
                format!("{name}=absent")
            }
        })
        .collect();
    println!(
        "[canary] in-process scan ({phase}): files {}",
        sizes.join(" ")
    );
    for (name, needles, matches) in by_kind {
        println!("[canary]   {name}: {needles} needle(s), {matches} match(es)");
    }
    total
}

/// [`scan_files`] on the vault file, its WAL and shm.
fn scan(phase: &str, dir: &Path, needles: &MemNeedles) -> usize {
    let files: Vec<PathBuf> = ["", "-wal", "-shm"]
        .iter()
        .map(|s| dir.join(format!("{VAULT_FILE}{s}")))
        .collect();
    scan_files(phase, &files, needles)
}

/// Every regular file in `dir` (the server files the script collected).
fn files_in(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();
    files.sort();
    files
}

/// The app's state with an in-memory sync credential store: the canary
/// must never touch the OS keychain.
fn new_state(slot: VaultSlot) -> AppState {
    let events: Arc<dyn EventSink> = Arc::new(PrintEvents);
    let prompts = Arc::new(prompt_cmds::prompt_bridge(events.clone()));
    AppState::with_token_store(
        slot,
        prompts,
        events,
        Arc::new(core_ssh::RusshTransport),
        Arc::new(core_sync::MemoryTokenStore::new()),
    )
}

/// Opens the vault the app's way; also returns a weak handle to prove the
/// database is closed once the state is dropped.
fn open(
    dir: &Path,
) -> Result<
    (
        AppState,
        Weak<std::sync::Mutex<core_api::vault::VaultService>>,
    ),
    String,
> {
    let slot = open_vault(Ok(dir.to_path_buf()), core_api::vault::default_gate());
    let weak = match &slot {
        VaultSlot::Ready(v) => Arc::downgrade(v),
        VaultSlot::Failed(why) => return Err(format!("vault did not open: {why}")),
    };
    Ok((new_state(slot), weak))
}

async fn wait_for(path: &Path) -> Result<(), String> {
    let deadline = Instant::now() + RELEASE_TIMEOUT;
    while !path.exists() {
        if Instant::now() > deadline {
            return Err(format!("timed out waiting for {}", path.display()));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Ok(())
}

async fn run(
    dir: &Path,
    handshake: &Path,
    server_files: Option<&Path>,
    inp: Inputs,
) -> Result<(), String> {
    let mut mem = MemNeedles::default();

    // -- 1. create, lock, unlock -------------------------------------------
    let (state, weak) = open(dir)?;
    let status = state.vault_status().await;
    if status.state != VaultState::Uninitialized {
        return Err(format!(
            "expected a fresh vault, found {:?} (is {} empty?)",
            status.state,
            dir.display()
        ));
    }
    let created = check(
        "vault_create",
        state
            .vault_create(SecretString::from(inp.master.clone()))
            .await,
    )?;
    mem.add("recovery_key", &created.recovery_key);
    check("vault_lock", state.vault_lock().await)?;
    check(
        "vault_unlock",
        state
            .vault_unlock(SecretString::from(inp.master.clone()))
            .await,
    )?;
    println!("[canary] vault created, locked and unlocked (production KDF)");

    // -- 2. keys, identity, host --------------------------------------------
    let key_with_pass = check(
        "key_generate (with passphrase)",
        state
            .key_generate(
                KeyAlgoDto::Ed25519,
                format!("{} key A", inp.label),
                Some(SecretString::from(inp.key_passphrase.clone())),
            )
            .await,
    )?;
    let key_plain = check(
        "key_generate (no passphrase)",
        state
            .key_generate(KeyAlgoDto::Ed25519, format!("{} key B", inp.label), None)
            .await,
    )?;
    for key in [&key_with_pass, &key_plain] {
        let export = check("key_export_private", state.key_export_private(key.id).await)?;
        mem.add_private_key("private_key_line", &export.private_openssh);
        if let Some(blob) = key.public_openssh.split_whitespace().nth(1) {
            mem.add("public_key_blob", blob);
        }
    }
    let identity = check(
        "upsert_entity identity",
        state
            .upsert_entity(
                "identity",
                json!({
                    "label": format!("{} identity", inp.label),
                    "username": "canary-user",
                    "password": inp.identity_password,
                    "keyId": key_plain.id,
                }),
            )
            .await,
    )?;
    let identity_id = identity["id"].clone();
    let host = check(
        "upsert_entity host",
        state
            .upsert_entity(
                "host",
                json!({
                    "label": inp.label,
                    "address": "canary-host.invalid",
                    "settings": { "port": 2222, "identityId": identity_id },
                }),
            )
            .await,
    )?;
    let host_id = host["id"].as_str().unwrap_or_default().to_owned();
    let hosts = check("list_entities host", state.list_entities("host").await)?;
    if !hosts.iter().any(|h| h["label"] == inp.label.as_str()) {
        return Err("the canary host is not listed after upsert".into());
    }
    // A password typed at a connect prompt with "save" checked.
    let saved_host = check(
        "upsert_entity host (no identity)",
        state
            .upsert_entity(
                "host",
                json!({ "label": format!("{} saved", inp.label), "address": "canary-host.invalid" }),
            )
            .await,
    )?;
    let saved_host_id: core_model::Id = saved_host["id"]
        .as_str()
        .and_then(|id| id.parse().ok())
        .ok_or("the saved-password host has no id")?;
    let saved_pw = SecretString::from(inp.saved_password.clone());
    let saved = check(
        "save_host_password",
        state
            .with_vault(move |v| {
                Ok(v.save_host_password(saved_host_id, "canary-user", saved_pw)?)
            })
            .await,
    )?;
    if saved.is_none() {
        return Err("save_host_password saved nothing".into());
    }
    println!(
        "[canary] stored: 2 keys, 2 identities (passwords set, one saved from a prompt), host {host_id} (canary label)"
    );

    // -- 2b. sync: turn it on and let the first sync finish ------------------
    if let Some((invite, username)) = &inp.sync {
        check(
            "sync_enable",
            state
                .sync_enable(
                    SecretString::from(invite.clone()),
                    username.clone(),
                    SecretString::from(inp.master.clone()),
                    RecoveryInput::Existing(SecretString::from(created.recovery_key.clone())),
                )
                .await
                .map_err(CmdError::from),
        )?;
        let deadline = Instant::now() + SYNC_TIMEOUT;
        loop {
            match state.sync_status().await.status {
                SyncStateDto::Idle { last_sync } if last_sync > 0 => break,
                SyncStateDto::Error { kind, message } => {
                    return Err(format!("sync stopped: {kind:?}: {message}"));
                }
                _ if Instant::now() > deadline => {
                    return Err("the first sync did not finish in time".into());
                }
                _ => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        }
        println!("[canary] sync enabled; the first sync finished (status idle)");
    }

    // -- 3. open-state check ------------------------------------------------
    let mut leaks = scan("vault open, WAL not checkpointed", dir, &mem);
    std::fs::write(handshake.join("open"), b"").map_err(|e| e.to_string())?;
    println!("[canary] waiting for the script's open-state grep");
    wait_for(&handshake.join("release")).await?;

    // The script has now collected the server's files; look for the
    // in-memory secrets in them too.
    if let Some(server_dir) = server_files {
        let files = files_in(server_dir);
        if files.is_empty() {
            return Err("the script collected no server files".into());
        }
        leaks += scan_files("server files", &files, &mem);
    }

    // -- 4. close, reopen, verify persisted ----------------------------------
    // The engine holds the vault open; stop it first.
    state.stop_sync().await;
    drop(state);
    // The forward-event task and any vault task let go asynchronously.
    for _ in 0..50 {
        if weak.strong_count() == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    if weak.strong_count() != 0 {
        return Err("the vault is still open after dropping the state".into());
    }
    println!("[canary] state dropped; database closed");
    leaks += scan("after close", dir, &mem);

    let (state, weak) = open(dir)?;
    check(
        "vault_unlock (reopened)",
        state
            .vault_unlock(SecretString::from(inp.master.clone()))
            .await,
    )?;
    let hosts = check("list_entities host", state.list_entities("host").await)?;
    let persisted = hosts
        .iter()
        .any(|h| h["label"] == inp.label.as_str() && h["id"] == host_id.as_str());
    let identities = check(
        "list_entities identity",
        state.list_entities("identity").await,
    )?;
    let has_password = identities
        .iter()
        .any(|i| i["id"] == identity_id && i["hasPassword"] == true);
    drop(state);
    for _ in 0..50 {
        if weak.strong_count() == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    if !persisted || !has_password {
        return Err(format!(
            "after reopening: canary host persisted = {persisted}, identity password stored = {has_password}"
        ));
    }
    println!("[canary] reopened: canary host and identity password persisted (decrypted OK)");
    leaks += scan("after reopen + close", dir, &mem);

    if leaks != 0 {
        return Err(format!(
            "{leaks} in-memory secret match(es) in the vault files"
        ));
    }
    println!("[canary] in-process secret matches: 0");
    Ok(())
}

fn main() -> ExitCode {
    init_tracing();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (dir, handshake, server_files) = match args.as_slice() {
        [dir, handshake] => (dir, handshake, None),
        [dir, handshake, server_files] => (dir, handshake, Some(server_files)),
        _ => {
            eprintln!(
                "usage: acceptance_canary <app-data-dir> <handshake-dir> [<server-files-dir>]"
            );
            return ExitCode::from(2);
        }
    };
    let inputs = (|| -> Result<Inputs, String> {
        Ok(Inputs {
            label: env_secret("TETHER_CANARY_LABEL")?,
            master: env_secret("TETHER_CANARY_MASTER_PW")?,
            identity_password: env_secret("TETHER_CANARY_IDENTITY_PW")?,
            key_passphrase: env_secret("TETHER_CANARY_KEY_PASSPHRASE")?,
            saved_password: env_secret("TETHER_CANARY_SAVED_PW")?,
            sync: match (
                std::env::var("TETHER_CANARY_SYNC_INVITE"),
                std::env::var("TETHER_CANARY_SYNC_USER"),
            ) {
                (Ok(invite), Ok(user)) => Some((invite, user)),
                _ => None,
            },
        })
    })();
    let inputs = match inputs {
        Ok(i) => i,
        Err(e) => {
            eprintln!("[canary] {e}");
            return ExitCode::from(2);
        }
    };
    let result = tauri::async_runtime::block_on(run(
        Path::new(dir),
        Path::new(handshake),
        server_files.map(|d| Path::new(d.as_str())),
        inputs,
    ));
    match result {
        Ok(()) => {
            println!("[canary] done");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("[canary] FAILED: {e}");
            ExitCode::FAILURE
        }
    }
}
