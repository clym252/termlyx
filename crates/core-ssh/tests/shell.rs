//! Integration tests against the docker sshd fixture (127.0.0.1:2222).
//! Run with `TETHER_SSH_FIXTURE=1`; otherwise every test prints a skip line
//! and returns early.

use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use bytes::Bytes;
use core_ssh::{
    AcceptAll, AuthMethod, AuthPrompter, ConnectParams, HostKeyDecision, HostKeyInfo,
    HostKeyVerifier, RusshTransport, ShellHandle, SshError, SshTransport,
};
use secrecy::{ExposeSecret, SecretString};
use tokio::sync::RwLock;

const KEY_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/keys/id_ed25519_test"
);
const COMPOSE_FILE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/docker-compose.yml"
);

/// Tests run in parallel threads; the restart test takes the write side so no
/// other test is mid-connection while sshd bounces.
static FIXTURE: RwLock<()> = RwLock::const_new(());

/// True when the docker sshd fixture is enabled; otherwise prints one skip
/// line for the calling test.
fn fixture_enabled() -> bool {
    let on = std::env::var("TETHER_SSH_FIXTURE").as_deref() == Ok("1");
    if !on {
        eprintln!("skipping: set TETHER_SSH_FIXTURE=1 (and run tests/fixtures/up.sh)");
    }
    on
}

fn params(auth: AuthMethod) -> ConnectParams {
    ConnectParams {
        host: "127.0.0.1".into(),
        port: 2222,
        username: "tether".into(),
        auth,
        term: "xterm-256color".into(),
        cols: 80,
        rows: 24,
        connect_timeout: Duration::from_secs(15),
        keepalive: None,
        env: Vec::new(),
    }
}

fn password(pw: &str) -> AuthMethod {
    AuthMethod::Password(SecretString::from(pw.to_string()))
}

struct RejectAll;

#[async_trait]
impl HostKeyVerifier for RejectAll {
    async fn verify(&self, _host: &str, _port: u16, _key: &HostKeyInfo) -> HostKeyDecision {
        HostKeyDecision::Reject
    }
}

async fn read_until(shell: &mut ShellHandle, needle: &str, within: Duration) -> String {
    let deadline = Instant::now() + within;
    let mut acc = String::new();
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match tokio::time::timeout(left, shell.output.recv()).await {
            Ok(Some(chunk)) => {
                acc.push_str(&String::from_utf8_lossy(&chunk));
                if acc.contains(needle) {
                    return acc;
                }
            }
            Ok(None) => panic!("output closed before {needle:?}; got {acc:?}"),
            Err(_) => panic!("timed out waiting for {needle:?}; got {acc:?}"),
        }
    }
}

async fn echo_roundtrip(auth: AuthMethod) {
    let transport = RusshTransport;
    let mut shell = transport
        .open_shell(params(auth), Arc::new(AcceptAll))
        .await
        .expect("open_shell");
    shell
        .control
        .write(Bytes::from_static(b"echo hello-tether\n"))
        .await
        .expect("write");
    read_until(&mut shell, "hello-tether", Duration::from_secs(5)).await;
    shell.control.resize(120, 40).await.expect("resize");
    shell.control.close().await;
}

#[tokio::test]
async fn password_shell_echo() {
    if !fixture_enabled() {
        return;
    }
    let _g = FIXTURE.read().await;
    echo_roundtrip(password("tether-test")).await;
}

#[tokio::test]
async fn key_shell_echo() {
    if !fixture_enabled() {
        return;
    }
    let _g = FIXTURE.read().await;
    let pem = std::fs::read_to_string(KEY_PATH).expect("fixture key");
    echo_roundtrip(AuthMethod::PrivateKey {
        pem: SecretString::from(pem),
        passphrase: None,
    })
    .await;
}

#[tokio::test]
async fn wrong_password_is_auth_failed() {
    if !fixture_enabled() {
        return;
    }
    let _g = FIXTURE.read().await;
    let err = RusshTransport
        .open_shell(params(password("definitely-wrong")), Arc::new(AcceptAll))
        .await
        .err()
        .expect("should fail");
    assert!(matches!(err, SshError::AuthFailed), "got {err:?}");
}

#[tokio::test]
async fn closed_port_fails_fast() {
    if !fixture_enabled() {
        return;
    }
    let mut p = params(password("tether-test"));
    p.port = 2299;
    p.connect_timeout = Duration::from_secs(2);
    let start = Instant::now();
    let err = RusshTransport
        .open_shell(p, Arc::new(AcceptAll))
        .await
        .err()
        .expect("should fail");
    assert!(matches!(err, SshError::Connect(_)), "got {err:?}");
    assert!(
        start.elapsed() < Duration::from_secs(3),
        "took {:?}",
        start.elapsed()
    );
}

#[tokio::test]
async fn rejecting_verifier_blocks() {
    if !fixture_enabled() {
        return;
    }
    let _g = FIXTURE.read().await;
    let err = RusshTransport
        .open_shell(params(password("tether-test")), Arc::new(RejectAll))
        .await
        .err()
        .expect("should fail");
    assert!(matches!(err, SshError::HostKeyRejected), "got {err:?}");
}

#[tokio::test]
async fn shell_reports_eof_when_server_dies() {
    if !fixture_enabled() {
        return;
    }
    let _g = FIXTURE.write().await;
    let mut shell = RusshTransport
        .open_shell(params(password("tether-test")), Arc::new(AcceptAll))
        .await
        .expect("open_shell");

    let status = Command::new("docker")
        .args(["compose", "-f", COMPOSE_FILE, "restart", "sshd"])
        .status()
        .expect("run docker compose");
    assert!(status.success(), "docker compose restart failed");

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match tokio::time::timeout(left, shell.output.recv()).await {
            Ok(Some(_)) => continue, // drain any trailing output
            Ok(None) => break,
            Err(_) => panic!("output did not close within 10s of sshd restart"),
        }
    }

    // Leave the fixture usable for whatever runs next.
    let ready = Instant::now() + Duration::from_secs(20);
    while Instant::now() < ready {
        if RusshTransport
            .open_shell(params(password("tether-test")), Arc::new(AcceptAll))
            .await
            .is_ok()
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    panic!("sshd did not come back after restart");
}

/// Regression: a write larger than the SSH window, with the consumer not
/// reading output (the pty echoes everything back), must not wedge the shell.
#[tokio::test]
async fn large_write_does_not_deadlock() {
    if !fixture_enabled() {
        return;
    }
    let _g = FIXTURE.read().await;
    let body = async {
        let mut shell = RusshTransport
            .open_shell(params(password("tether-test")), Arc::new(AcceptAll))
            .await
            .expect("open_shell");
        shell
            .control
            .write(Bytes::from_static(b"cat > /dev/null\n"))
            .await
            .expect("write");

        // 8 MiB in 1 KiB lines (the pty line discipline caps line length).
        let writer = {
            let control = shell.control.clone();
            tokio::spawn(async move {
                let line = Bytes::from(format!("{}\n", "x".repeat(1023)));
                for _ in 0..8192 {
                    control.write(line.clone()).await.expect("write chunk");
                }
                control
                    .write(Bytes::from_static(b"\x04echo tether-$((1+1))-done\n"))
                    .await
                    .expect("write tail");
            })
        };

        // Do not read for 2 s: echo backs up all the way to the socket.
        tokio::time::sleep(Duration::from_secs(2)).await;

        // Now drain until the marker (it is computed, so the echoed command
        // line does not contain it).
        let mut tail: Vec<u8> = Vec::new();
        loop {
            let chunk = shell.output.recv().await.expect("output closed early");
            tail.extend_from_slice(&chunk);
            if tail.len() > 8192 {
                tail.drain(..tail.len() - 8192);
            }
            if String::from_utf8_lossy(&tail).contains("tether-2-done") {
                break;
            }
        }
        writer.await.expect("writer task");
        shell.control.close().await;
        while shell.output.recv().await.is_some() {}
    };
    tokio::time::timeout(Duration::from_secs(30), body)
        .await
        .expect("shell deadlocked on a large write");
}

/// Accepts every key, but only after `delay` (a human thinking it over).
struct SlowAccept {
    delay: Duration,
}

#[async_trait]
impl HostKeyVerifier for SlowAccept {
    async fn verify(&self, _host: &str, _port: u16, _key: &HostKeyInfo) -> HostKeyDecision {
        tokio::time::sleep(self.delay).await;
        HostKeyDecision::Accept
    }
}

/// Answers every prompt with `answer` after `delay`, recording what it was
/// asked.
struct SlowPrompter {
    delay: Duration,
    answer: &'static str,
    asked: std::sync::Mutex<Vec<String>>,
}

impl SlowPrompter {
    fn new(delay: Duration, answer: &'static str) -> Self {
        Self {
            delay,
            answer,
            asked: std::sync::Mutex::new(Vec::new()),
        }
    }

    async fn reply(&self, what: String) -> SecretString {
        self.asked.lock().unwrap().push(what);
        tokio::time::sleep(self.delay).await;
        SecretString::from(self.answer)
    }

    fn asked(&self) -> Vec<String> {
        self.asked.lock().unwrap().clone()
    }
}

#[async_trait]
impl AuthPrompter for SlowPrompter {
    async fn password(&self, user: &str, host: &str) -> Option<SecretString> {
        Some(self.reply(format!("password {user}@{host}")).await)
    }

    async fn passphrase(&self, key_label: &str) -> Option<SecretString> {
        Some(self.reply(format!("passphrase {key_label}")).await)
    }

    async fn keyboard_interactive(
        &self,
        _name: &str,
        _instructions: &str,
        prompts: &[(String, bool)],
    ) -> Option<Vec<SecretString>> {
        let mut answers = Vec::new();
        for (text, _) in prompts {
            answers.push(self.reply(format!("kbd {text}")).await);
        }
        Some(answers)
    }
}

#[tokio::test]
async fn slow_verifier_does_not_trip_connect_timeout() {
    if !fixture_enabled() {
        return;
    }
    let _g = FIXTURE.read().await;
    let mut p = params(password("tether-test"));
    p.connect_timeout = Duration::from_secs(2);
    let start = Instant::now();
    let shell = RusshTransport
        .open_shell(
            p,
            Arc::new(SlowAccept {
                delay: Duration::from_secs(5),
            }),
        )
        .await
        .expect("a slow human must not trip the connect timeout");
    assert!(start.elapsed() >= Duration::from_secs(5));
    shell.control.close().await;
}

/// `AuthMethod::KeyboardInteractive` against the fixture: sshd advertises
/// keyboard-interactive but (built without PAM) fails it without asking
/// anything, so the transport falls back to asking for a password.
#[tokio::test]
async fn slow_human_prompt_does_not_trip_connect_timeout() {
    if !fixture_enabled() {
        return;
    }
    let _g = FIXTURE.read().await;
    let mut p = params(AuthMethod::KeyboardInteractive);
    p.connect_timeout = Duration::from_secs(2);
    let prompter = Arc::new(SlowPrompter::new(Duration::from_secs(5), "tether-test"));
    let start = Instant::now();
    let mut shell = RusshTransport
        .open_shell_with_prompter(p, Arc::new(AcceptAll), prompter.clone())
        .await
        .expect("a slow human must not trip the connect timeout");
    assert!(start.elapsed() >= Duration::from_secs(5));
    assert_eq!(
        prompter.asked(),
        vec!["password tether@127.0.0.1".to_string()]
    );
    shell
        .control
        .write(Bytes::from_static(b"echo slow-$((40+2))\n"))
        .await
        .expect("write");
    read_until(&mut shell, "slow-42", Duration::from_secs(5)).await;
    shell.control.close().await;
}

#[tokio::test]
async fn keyboard_interactive_without_prompter_answer_is_auth_failed() {
    if !fixture_enabled() {
        return;
    }
    let _g = FIXTURE.read().await;
    // `open_shell` uses `NoPrompter`: nobody can answer, so auth fails.
    let err = RusshTransport
        .open_shell(params(AuthMethod::KeyboardInteractive), Arc::new(AcceptAll))
        .await
        .err()
        .expect("should fail");
    assert!(matches!(err, SshError::AuthFailed), "got {err:?}");
}

/// The fixture key, re-encrypted under `passphrase` (OpenSSH format).
fn encrypted_fixture_key(passphrase: &str) -> SecretString {
    use russh::keys::ssh_key::LineEnding;
    let pem = std::fs::read_to_string(KEY_PATH).expect("fixture key");
    let key = russh::keys::PrivateKey::from_openssh(&pem).expect("parse fixture key");
    let enc = key
        .encrypt(&mut russh::keys::key::safe_rng(), passphrase)
        .expect("encrypt");
    SecretString::from(enc.to_openssh(LineEnding::LF).expect("encode").to_string())
}

#[tokio::test]
async fn passphrase_prompt_unlocks_encrypted_key() {
    if !fixture_enabled() {
        return;
    }
    let _g = FIXTURE.read().await;
    let pem = encrypted_fixture_key("open-sesame");
    assert!(pem.expose_secret().contains("BEGIN OPENSSH PRIVATE KEY"));
    let prompter = Arc::new(SlowPrompter::new(Duration::ZERO, "open-sesame"));
    let shell = RusshTransport
        .open_shell_with_prompter(
            params(AuthMethod::PrivateKey {
                pem,
                passphrase: None,
            }),
            Arc::new(AcceptAll),
            prompter.clone(),
        )
        .await
        .expect("open_shell with prompted passphrase");
    let asked = prompter.asked();
    assert_eq!(asked.len(), 1, "asked: {asked:?}");
    assert!(
        asked[0].starts_with("passphrase SHA256:"),
        "asked: {asked:?}"
    );
    shell.control.close().await;
}

#[tokio::test]
async fn passphrase_prompt_wrong_answer_is_auth_failed() {
    if !fixture_enabled() {
        return;
    }
    let _g = FIXTURE.read().await;
    let err = RusshTransport
        .open_shell_with_prompter(
            params(AuthMethod::PrivateKey {
                pem: encrypted_fixture_key("open-sesame"),
                passphrase: None,
            }),
            Arc::new(AcceptAll),
            Arc::new(SlowPrompter::new(Duration::ZERO, "wrong")),
        )
        .await
        .err()
        .expect("should fail");
    assert!(matches!(err, SshError::AuthFailed), "got {err:?}");
}
