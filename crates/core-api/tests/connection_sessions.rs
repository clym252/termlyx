//! `ShellTarget::Connection`: several sessions on one `SshConnection`
//! against the docker sshd fixture. Run with `TETHER_SSH_FIXTURE=1`.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use core_api::{OutputSink, SessionManager, ShellTarget};
use core_ssh::{AcceptAll, AuthMethod, ConnectParams, NoPrompter, RusshTransport, SshConnection};
use secrecy::SecretString;
use tokio::time::{sleep, timeout};

fn fixture_enabled() -> bool {
    let on = std::env::var("TETHER_SSH_FIXTURE").as_deref() == Ok("1");
    if !on {
        eprintln!("skipping: set TETHER_SSH_FIXTURE=1 (and run tests/fixtures/up.sh)");
    }
    on
}

#[derive(Default)]
struct TextSink {
    text: Mutex<String>,
    closed: Mutex<Vec<Option<String>>>,
    exit_statuses: Mutex<Vec<Option<u32>>>,
}

impl TextSink {
    async fn wait_for(&self, needle: &str) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !self.text.lock().unwrap().contains(needle) {
            assert!(
                Instant::now() < deadline,
                "no {needle:?} in {:?}",
                self.text.lock().unwrap()
            );
            sleep(Duration::from_millis(20)).await;
        }
    }
}

impl OutputSink for TextSink {
    fn frame(&self, bytes: Vec<u8>) {
        self.text
            .lock()
            .unwrap()
            .push_str(&String::from_utf8_lossy(&bytes));
    }
    fn closed(&self, reason: Option<String>, exit_status: Option<u32>) {
        self.closed.lock().unwrap().push(reason);
        self.exit_statuses.lock().unwrap().push(exit_status);
    }
}

fn target(conn: &SshConnection) -> ShellTarget {
    ShellTarget::Connection {
        conn: conn.clone(),
        term: "xterm-256color".into(),
        cols: 80,
        rows: 24,
        env: Vec::new(),
        startup: None,
    }
}

#[tokio::test]
async fn sessions_share_a_connection_and_close_independently() {
    if !fixture_enabled() {
        return;
    }
    let params = ConnectParams {
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
    };
    let conn = SshConnection::connect(&params, Arc::new(AcceptAll), Arc::new(NoPrompter))
        .await
        .expect("connect");
    let closed = conn.close_signal();
    let mgr = SessionManager::new(Arc::new(RusshTransport));

    let (a_sink, b_sink) = (Arc::new(TextSink::default()), Arc::new(TextSink::default()));
    let a = mgr
        .open(target(&conn), Arc::new(AcceptAll), a_sink.clone())
        .await
        .expect("open a");
    let b = mgr
        .open(target(&conn), Arc::new(AcceptAll), b_sink.clone())
        .await
        .expect("open b");
    drop(conn);

    mgr.write(&a, b"echo a-$((1+1))\n".to_vec()).await.unwrap();
    a_sink.wait_for("a-2").await;

    // Closing one session closes only its channel.
    mgr.close(&a).await;
    assert_eq!(*a_sink.closed.lock().unwrap(), vec![None]);
    mgr.write(&b, b"echo b-$((2+2))\n".to_vec()).await.unwrap();
    b_sink.wait_for("b-4").await;
    assert!(!closed.is_closed());

    // The last session's close ends the connection.
    mgr.close(&b).await;
    timeout(Duration::from_secs(5), closed.wait())
        .await
        .expect("connection outlived its last session");
}

fn fixture_params() -> ConnectParams {
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

impl TextSink {
    async fn wait_closed(&self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.closed.lock().unwrap().is_empty() {
            assert!(Instant::now() < deadline, "the session never ended");
            sleep(Duration::from_millis(20)).await;
        }
    }
}

/// The remote shell exiting is a normal end (`None`) with its exit status.
#[tokio::test]
async fn remote_exit_is_a_normal_end_with_exit_status() {
    if !fixture_enabled() {
        return;
    }
    let conn = SshConnection::connect(&fixture_params(), Arc::new(AcceptAll), Arc::new(NoPrompter))
        .await
        .expect("connect");
    let mgr = SessionManager::new(Arc::new(RusshTransport));
    let sink = Arc::new(TextSink::default());
    let id = mgr
        .open(target(&conn), Arc::new(AcceptAll), sink.clone())
        .await
        .expect("open");
    drop(conn);
    mgr.write(&id, b"exit 3\n".to_vec()).await.unwrap();
    sink.wait_closed().await;
    assert_eq!(*sink.closed.lock().unwrap(), vec![None]);
    assert_eq!(*sink.exit_statuses.lock().unwrap(), vec![Some(3)]);
}

/// A connection that dies under the shell (its sshd process is killed, so
/// no channel close ever arrives) is a lost session with a reason.
#[tokio::test]
async fn dropped_connection_is_lost_with_a_reason() {
    if !fixture_enabled() {
        return;
    }
    let conn = SshConnection::connect(&fixture_params(), Arc::new(AcceptAll), Arc::new(NoPrompter))
        .await
        .expect("connect");
    let mgr = SessionManager::new(Arc::new(RusshTransport));
    let sink = Arc::new(TextSink::default());
    let id = mgr
        .open(target(&conn), Arc::new(AcceptAll), sink.clone())
        .await
        .expect("open");
    drop(conn);
    // The shell's parent is the per-session sshd process, owned by the user.
    mgr.write(&id, b"kill -9 $PPID\n".to_vec()).await.unwrap();
    // Keep typing while it dies: should a write fail on the dead connection
    // first, the reason must still be why the connection went away (the
    // race is rare against the fixture; this does not force it).
    let deadline = Instant::now() + Duration::from_secs(10);
    while sink.closed.lock().unwrap().is_empty() && Instant::now() < deadline {
        let _ = mgr.write(&id, b"x".to_vec()).await;
        sleep(Duration::from_millis(2)).await;
    }
    sink.wait_closed().await;
    let closed = sink.closed.lock().unwrap().clone();
    assert_eq!(closed.len(), 1, "{closed:?}");
    let reason = closed[0].clone().expect("a lost connection has a reason");
    assert!(
        !reason.is_empty() && !reason.starts_with("write failed"),
        "{reason}"
    );
    assert_eq!(*sink.exit_statuses.lock().unwrap(), vec![None]);
}
