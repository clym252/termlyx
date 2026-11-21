//! `SshConnection` against the docker sshd fixture (127.0.0.1:2222): several
//! channels on one connection, per-channel flow control and lifetime.
//! Run with `TETHER_SSH_FIXTURE=1`; otherwise every test prints a skip line
//! and returns early.

use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use bytes::Bytes;
use core_ssh::{
    AcceptAll, AuthMethod, ConnectParams, ConnectProgress, ConnectStep, HostKeyDecision,
    HostKeyInfo, HostKeyVerifier, NoPrompter, ShellHandle, SshConnection, SshError,
};
use russh_sftp::client::SftpSession;
use secrecy::SecretString;
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;
use tokio::time::{sleep, timeout};

const CONTAINER: &str = "tether-sshd";
const BIG_FILE: &str = "/tmp/tether-10m.bin";
const BIG_LEN: usize = 10 * 1024 * 1024;

/// The tests inspect the fixture's processes, so they run one at a time.
static SERIAL: Mutex<()> = Mutex::const_new(());

fn fixture_enabled() -> bool {
    let on = std::env::var("TETHER_SSH_FIXTURE").as_deref() == Ok("1");
    if !on {
        eprintln!("skipping: set TETHER_SSH_FIXTURE=1 (and run tests/fixtures/up.sh)");
    }
    on
}

fn params() -> ConnectParams {
    ConnectParams {
        host: "127.0.0.1".into(),
        port: 2222,
        username: "tether".into(),
        auth: AuthMethod::Password(SecretString::from("tether-test")),
        term: "xterm-256color".into(),
        cols: 80,
        rows: 24,
        connect_timeout: Duration::from_secs(15),
        keepalive: None,
        env: Vec::new(),
    }
}

async fn connect() -> SshConnection {
    SshConnection::connect(&params(), Arc::new(AcceptAll), Arc::new(NoPrompter))
        .await
        .expect("connect")
}

/// Runs `script` with `sh -c` in the fixture container; returns stdout.
fn docker_sh(script: &str) -> String {
    let out = Command::new("docker")
        .args(["exec", CONTAINER, "sh", "-c", script])
        .output()
        .expect("run docker exec");
    assert!(
        out.status.success(),
        "docker exec {script:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("utf-8 output")
}

/// Creates the 10 MiB random file (fresh each run) and returns its SHA-256.
fn make_big_file() -> String {
    docker_sh(&format!(
        "head -c {BIG_LEN} /dev/urandom > {BIG_FILE} && chmod 644 {BIG_FILE}"
    ));
    let sum = docker_sh(&format!("sha256sum {BIG_FILE}"));
    sum.split_whitespace()
        .next()
        .expect("sha256sum output")
        .to_owned()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

async fn sftp(conn: &SshConnection) -> SftpSession {
    let channel = conn.open_sftp().await.expect("open_sftp");
    SftpSession::new(channel.into_stream())
        .await
        .expect("sftp init")
}

async fn assert_lists_etc(sftp: &SftpSession) {
    let names: Vec<String> = sftp
        .read_dir("/etc")
        .await
        .expect("read_dir /etc")
        .map(|e| e.file_name())
        .collect();
    assert!(names.iter().any(|n| n == "passwd"), "got {names:?}");
}

/// Reads shell output until `needle` shows up; returns everything read.
async fn read_until(shell: &mut ShellHandle, needle: &str, within: Duration) -> String {
    let deadline = Instant::now() + within;
    let mut acc = String::new();
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match timeout(left, shell.output.recv()).await {
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

/// The fixture's processes for the connection a shell runs on: the shell's
/// parent (the user's `sshd-session`) and that process's parent (the
/// privileged `sshd-session`).
async fn session_pids(shell: &mut ShellHandle) -> Vec<u32> {
    shell
        .control
        .write(Bytes::from_static(b"echo \"sid:$PPID:end\"\n"))
        .await
        .expect("write");
    let out = read_until(shell, ":end\r\n", Duration::from_secs(5)).await;
    // The echoed command line holds `$PPID` literally; the output holds digits.
    let user: u32 = out
        .split("sid:")
        .filter_map(|s| s.split(":end").next()?.parse().ok())
        .next()
        .unwrap_or_else(|| panic!("no pid in {out:?}"));
    // Read /proc directly: the fixture's busybox `ps` leaves sshd out.
    assert!(
        cmdline(user).contains("sshd-session"),
        "shell parent is {:?}",
        cmdline(user)
    );
    let status = docker_sh(&format!("cat /proc/{user}/status"));
    let parent: u32 = status
        .lines()
        .find_map(|l| l.strip_prefix("PPid:"))
        .and_then(|p| p.trim().parse().ok())
        .unwrap_or_else(|| panic!("no PPid in {status}"));
    assert!(
        cmdline(parent).contains("sshd-session"),
        "shell grandparent is {:?}",
        cmdline(parent)
    );
    vec![user, parent]
}

fn cmdline(pid: u32) -> String {
    docker_sh(&format!("tr '\\0' ' ' < /proc/{pid}/cmdline"))
}

/// Which of `pids` still run in the fixture, with their command lines.
fn alive(pids: &[u32]) -> Vec<String> {
    let script: String = pids
        .iter()
        .map(|p| format!("[ -d /proc/{p} ] && echo \"{p}: $(tr '\\0' ' ' < /proc/{p}/cmdline)\"; "))
        .collect();
    docker_sh(&format!("{script}true"))
        .lines()
        .map(str::to_owned)
        .collect()
}

#[tokio::test]
async fn stalled_shell_does_not_block_sftp_on_same_connection() {
    if !fixture_enabled() {
        return;
    }
    let _g = SERIAL.lock().await;
    let expected = make_big_file();

    let conn = connect().await;
    let shell = conn
        .open_shell("xterm-256color", 80, 24)
        .await
        .expect("open_shell");
    // Endless output that nobody ever reads.
    shell
        .control
        .write(Bytes::from_static(b"cat /dev/zero | base64\n"))
        .await
        .expect("write");
    // Give the shell time to fill everything between it and us.
    sleep(Duration::from_secs(1)).await;

    let body = async {
        let sftp = sftp(&conn).await;
        assert_lists_etc(&sftp).await;
        let data = sftp.read(BIG_FILE).await.expect("download");
        assert_eq!(data.len(), BIG_LEN);
        assert_eq!(hex(&Sha256::digest(&data)), expected);
    };
    timeout(Duration::from_secs(10), body)
        .await
        .expect("sftp stalled behind an unread shell on the same connection");

    // The shell was only paused: its output resumes, intact, once read
    // (well past what its window and buffer hold).
    assert!(!conn.is_closed());
    let mut shell = shell;
    let resumed = async {
        // Prompt and command echo (possibly twice) come first; from the
        // first full base64 line on, output must be base64 of zeros.
        let line = format!("{}\r\n", "A".repeat(76));
        let head = read_until(&mut shell, &line, Duration::from_secs(5)).await;
        let start = head.find(&line).expect("first line");
        let tail = &head.as_bytes()[start..];
        assert!(tail.iter().all(|b| matches!(b, b'A' | b'\r' | b'\n')));
        let mut seen = tail.len();
        while seen < 8 * 1024 * 1024 {
            let chunk = shell.output.recv().await.expect("shell output ended");
            seen += chunk.len();
            if let Some(at) = chunk
                .iter()
                .position(|b| !matches!(b, b'A' | b'\r' | b'\n'))
            {
                let ctx = &chunk[at.saturating_sub(80)..(at + 80).min(chunk.len())];
                panic!(
                    "unexpected bytes at {at} after {seen} bytes: {:?}",
                    String::from_utf8_lossy(ctx)
                );
            }
        }
    };
    timeout(Duration::from_secs(10), resumed)
        .await
        .expect("shell output did not resume");
    drop(shell);
}

#[tokio::test]
async fn connection_closes_when_last_channel_drops() {
    if !fixture_enabled() {
        return;
    }
    let _g = SERIAL.lock().await;

    let conn = connect().await;
    let closed = conn.close_signal();
    let mut shell = conn
        .open_shell("xterm-256color", 80, 24)
        .await
        .expect("open_shell");
    let sftp = sftp(&conn).await;
    let pids = session_pids(&mut shell).await;

    // The channels keep the connection alive without the handle.
    drop(conn);
    assert_lists_etc(&sftp).await;
    shell
        .control
        .write(Bytes::from_static(b"echo still-$((20+1))\n"))
        .await
        .expect("write");
    read_until(&mut shell, "still-21", Duration::from_secs(5)).await;

    // Dropping the shell closes only its channel.
    drop(shell);
    sleep(Duration::from_millis(300)).await;
    assert!(!closed.is_closed());
    assert_lists_etc(&sftp).await;

    // Dropping the last channel closes the connection.
    drop(sftp);
    timeout(Duration::from_secs(5), closed.wait())
        .await
        .expect("connection did not close within 5 s of its last channel");

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let left = alive(&pids);
        if left.is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "sshd-session still running for the closed connection: {left:?}"
        );
        sleep(Duration::from_millis(100)).await;
    }
}

/// A channel whose writes wait on the server's window (the remote side never
/// reads) must not stop other channels' output: their window grants still
/// have to reach the server.
#[tokio::test(flavor = "multi_thread")]
async fn write_blocked_shell_does_not_stall_other_channels() {
    if !fixture_enabled() {
        return;
    }
    let _g = SERIAL.lock().await;

    let conn = connect().await;
    let mut a = conn
        .open_shell("xterm-256color", 80, 24)
        .await
        .expect("open shell a");
    let mut b = conn
        .open_shell("xterm-256color", 80, 24)
        .await
        .expect("open shell b");

    // B: about 10 windows of output, starting once A is write-blocked. (It
    // is sent first: while A's writes wait on the server's window, russh
    // sends no other channel's writes either, a known stock limitation.)
    b.control
        .write(Bytes::from_static(
            b"sleep 3; head -c 22000000 /dev/zero | base64; echo b-$((6*7))-done\n",
        ))
        .await
        .expect("write b");

    // A: a remote side that never reads its input again.
    a.control
        .write(Bytes::from_static(
            b"stty raw -echo; echo a-$((3*3))-ready; exec sleep 600\n",
        ))
        .await
        .expect("write a");
    read_until(&mut a, "a-9-ready", Duration::from_secs(5)).await;
    // Far more input than the pty buffer and A's window hold.
    let ctl = a.control.clone();
    let writer = tokio::spawn(async move {
        let block = Bytes::from(vec![b'a'; 32 * 1024]);
        for _ in 0..(8 * 1024 * 1024 / block.len()) {
            if ctl.write(block.clone()).await.is_err() {
                return;
            }
        }
    });
    let a_out = tokio::spawn(async move { while a.output.recv().await.is_some() {} });
    // Let A's writes fill the server's window.
    sleep(Duration::from_secs(1)).await;
    assert!(!writer.is_finished(), "A's writes should be blocked");

    // Read B as fast as it comes.
    let mut got = 0usize;
    let mut tail = String::new();
    let body = async {
        loop {
            let chunk = b.output.recv().await.expect("b ended");
            got += chunk.len();
            tail.push_str(&String::from_utf8_lossy(&chunk));
            if tail.contains("b-42-done") {
                return;
            }
            if tail.len() > 64 {
                tail.drain(..tail.len() - 64);
            }
        }
    };
    timeout(Duration::from_secs(30), body)
        .await
        .unwrap_or_else(|_| panic!("B stalled after {got} bytes while A was write-blocked"));
    assert!(got >= 20_000_000, "B delivered only {got} bytes");

    writer.abort();
    a_out.abort();
    drop(b);
}

/// `env` requests go out before the shell and a server that refuses them
/// (the fixture has no `AcceptEnv`) still starts the shell; a custom
/// keepalive interval connects too.
#[tokio::test]
async fn refused_env_and_custom_keepalive_still_open_a_shell() {
    if !fixture_enabled() {
        return;
    }
    let _serial = SERIAL.lock().await;
    let p = ConnectParams {
        keepalive: Some(Duration::from_secs(5)),
        env: vec![("TETHER_ENV_PROBE".into(), "on".into())],
        ..params()
    };
    let conn = SshConnection::connect(&p, Arc::new(AcceptAll), Arc::new(NoPrompter))
        .await
        .expect("connect");
    let mut shell = conn
        .open_shell("xterm-256color", 80, 24)
        .await
        .expect("shell");
    shell
        .control
        .write(Bytes::from_static(
            b"echo env-ok-$((3*7))-${TETHER_ENV_PROBE:-unset}\n",
        ))
        .await
        .unwrap();
    let out = read_until(&mut shell, "env-ok-21-", Duration::from_secs(10)).await;
    assert!(out.contains("env-ok-21-"), "{out}");
}

/// Records the connect steps reported, in order.
#[derive(Default)]
struct Recorder(std::sync::Mutex<Vec<ConnectStep>>);

impl Recorder {
    fn steps(&self) -> Vec<ConnectStep> {
        self.0.lock().unwrap().clone()
    }
}

impl ConnectProgress for Recorder {
    fn step(&self, step: ConnectStep) {
        self.0.lock().unwrap().push(step);
    }
}

#[tokio::test]
async fn progress_reports_tcp_hostkey_auth_in_order() {
    if !fixture_enabled() {
        return;
    }
    let _serial = SERIAL.lock().await;
    let rec = Arc::new(Recorder::default());
    SshConnection::connect_with_progress(
        &params(),
        Arc::new(AcceptAll),
        Arc::new(NoPrompter),
        rec.clone(),
    )
    .await
    .expect("connect");
    assert_eq!(
        rec.steps(),
        [ConnectStep::Tcp, ConnectStep::HostKey, ConnectStep::Auth]
    );
}

#[tokio::test]
async fn progress_stops_at_tcp_when_unreachable() {
    if !fixture_enabled() {
        return;
    }
    let _serial = SERIAL.lock().await;
    let rec = Arc::new(Recorder::default());
    let p = ConnectParams {
        port: 1,
        ..params()
    };
    let res = SshConnection::connect_with_progress(
        &p,
        Arc::new(AcceptAll),
        Arc::new(NoPrompter),
        rec.clone(),
    )
    .await;
    assert!(res.is_err());
    assert_eq!(rec.steps(), [ConnectStep::Tcp]);
}

#[tokio::test]
async fn progress_stops_at_auth_on_wrong_password() {
    if !fixture_enabled() {
        return;
    }
    let _serial = SERIAL.lock().await;
    let rec = Arc::new(Recorder::default());
    let p = ConnectParams {
        auth: AuthMethod::Password(SecretString::from("not-the-password")),
        ..params()
    };
    let err = SshConnection::connect_with_progress(
        &p,
        Arc::new(AcceptAll),
        Arc::new(NoPrompter),
        rec.clone(),
    )
    .await
    .expect_err("wrong password must fail");
    assert!(matches!(err, SshError::AuthFailed), "got {err:?}");
    assert_eq!(
        rec.steps(),
        [ConnectStep::Tcp, ConnectStep::HostKey, ConnectStep::Auth]
    );
}

/// A user (or policy) that refuses every host key.
struct RejectAll;

#[async_trait]
impl HostKeyVerifier for RejectAll {
    async fn verify(&self, _host: &str, _port: u16, _key: &HostKeyInfo) -> HostKeyDecision {
        HostKeyDecision::Reject
    }
}

#[tokio::test]
async fn progress_stops_at_hostkey_when_the_key_is_rejected() {
    if !fixture_enabled() {
        return;
    }
    let _serial = SERIAL.lock().await;
    let rec = Arc::new(Recorder::default());
    let err = SshConnection::connect_with_progress(
        &params(),
        Arc::new(RejectAll),
        Arc::new(NoPrompter),
        rec.clone(),
    )
    .await
    .expect_err("a rejected host key must fail");
    assert!(matches!(err, SshError::HostKeyRejected), "got {err:?}");
    assert_eq!(rec.steps(), [ConnectStep::Tcp, ConnectStep::HostKey]);
}
