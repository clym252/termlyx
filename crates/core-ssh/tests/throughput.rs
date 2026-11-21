//! Headless shell throughput: `cat` of a large file through
//! `RusshTransport::open_shell`, drained as fast as possible (no batching,
//! IPC or xterm). It bounds what the SSH side of the terminal pipeline can
//! deliver; it is NOT the GUI throughput gate (see `apps/tauri/src-tauri/src/bench.rs`).
//!
//! Run (release, fixture up):
//! `TETHER_SSH_FIXTURE=1 cargo test --release -p core-ssh --test throughput -- --ignored --nocapture`

use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use core_ssh::{AcceptAll, AuthMethod, ConnectParams, RusshTransport, SshTransport};
use secrecy::SecretString;

/// Same payload as the GUI bench: 100 MB of random bytes, base64-encoded.
const PAYLOAD: &str = "/tmp/tether-bench-big";

fn docker_sh(script: &str) {
    let status = Command::new("docker")
        .args(["exec", "tether-sshd", "sh", "-c", script])
        .status()
        .expect("run docker exec");
    assert!(status.success(), "docker exec {script:?} failed");
}

/// Finds `needle` in `hay`, returning the index just past it.
fn find_end(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len())
        .position(|w| w == needle)
        .map(|i| i + needle.len())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "benchmark; run explicitly in release mode"]
async fn shell_cat_throughput() {
    if std::env::var("TETHER_SSH_FIXTURE").as_deref() != Ok("1") {
        eprintln!("skipping: set TETHER_SSH_FIXTURE=1 (and run tests/fixtures/up.sh)");
        return;
    }
    docker_sh(&format!(
        "[ -s {PAYLOAD} ] || (head -c 100000000 /dev/urandom | base64 > {PAYLOAD})"
    ));
    let params = ConnectParams {
        host: "127.0.0.1".into(),
        port: 2222,
        username: "tether".into(),
        auth: AuthMethod::Password(SecretString::from("tether-test")),
        term: "xterm-256color".into(),
        cols: 120,
        rows: 40,
        connect_timeout: Duration::from_secs(15),
        keepalive: None,
        env: Vec::new(),
    };
    let mut shell = RusshTransport
        .open_shell(params, Arc::new(AcceptAll))
        .await
        .expect("open_shell");
    // The markers are computed, so the echoed command line cannot match.
    let cmd = format!("echo BENCH-$((1+1))-START; cat {PAYLOAD}; echo BENCH-$((6*7))-END\n");
    shell.control.write(Bytes::from(cmd)).await.expect("write");

    let mut tail: Vec<u8> = Vec::new();
    let mut started: Option<Instant> = None;
    let mut bytes = 0usize;
    let body = async {
        loop {
            let chunk = shell.output.recv().await.expect("shell output ended");
            tail.extend_from_slice(&chunk);
            if started.is_none() {
                if let Some(at) = find_end(&tail, b"BENCH-2-START\r\n") {
                    started = Some(Instant::now());
                    bytes = tail.len() - at;
                    tail.drain(..at);
                }
                continue;
            }
            bytes += chunk.len();
            if find_end(&tail, b"BENCH-42-END").is_some() {
                return;
            }
            if tail.len() > 64 {
                tail.drain(..tail.len() - 64);
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(120), body)
        .await
        .expect("bench did not finish within 120 s");
    let secs = started.expect("start marker").elapsed().as_secs_f64();
    println!(
        "shell_cat_throughput: {:.1} MB in {:.2} s = {:.1} MB/s (1 MB = 1e6 bytes)",
        bytes as f64 / 1e6,
        secs,
        bytes as f64 / 1e6 / secs
    );
    shell.control.close().await;
}
