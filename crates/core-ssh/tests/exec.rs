//! `exec_capture`, startup commands and per-channel env against the docker
//! sshd fixture (127.0.0.1:2222, which has tmux and `AcceptEnv LC_*`).
//! Run with `TETHER_SSH_FIXTURE=1`; otherwise every test prints a skip line
//! and returns early.

use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use core_ssh::{
    AcceptAll, AuthMethod, ConnectParams, NoPrompter, ShellHandle, ShellOptions, SshConnection,
    TAB_ENV, parse_tmux_tabs, tmux_reattach_command, tmux_tabs_command,
};
use secrecy::SecretString;
use tokio::sync::Mutex;

const WORK: &str = "my \"work\" $x";

/// The tmux tests share one tmux server (one user), so they run one at a time.
static SERIAL: Mutex<()> = Mutex::const_new(());

fn fixture_enabled() -> bool {
    let on = std::env::var("TETHER_SSH_FIXTURE").as_deref() == Ok("1");
    if !on {
        eprintln!("skipping: set TETHER_SSH_FIXTURE=1 (and run tests/fixtures/up.sh)");
    }
    on
}

async fn connect() -> SshConnection {
    let p = ConnectParams {
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
    SshConnection::connect(&p, Arc::new(AcceptAll), Arc::new(NoPrompter))
        .await
        .expect("connect")
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

async fn send(shell: &ShellHandle, text: &str) {
    shell
        .control
        .write(Bytes::from(text.to_owned()))
        .await
        .expect("write");
}

async fn shell_with(
    conn: &SshConnection,
    env: &[(String, String)],
    startup: Option<&str>,
) -> ShellHandle {
    conn.open_shell_with(ShellOptions {
        term: "xterm-256color",
        cols: 80,
        rows: 24,
        env,
        startup,
    })
    .await
    .expect("open_shell_with")
}

async fn out(conn: &SshConnection, cmd: &str) -> String {
    let o = conn.exec_capture(cmd, 64 * 1024).await.expect("exec");
    String::from_utf8_lossy(&o.stdout).into_owned()
}

async fn new_detached_session(conn: &SshConnection) {
    let o = conn
        .exec_capture(&format!("tmux new-session -d -s '{WORK}'"), 4096)
        .await
        .expect("exec");
    assert_eq!(o.exit_status, Some(0));
}

async fn kill_session(conn: &SshConnection) {
    let _ = conn
        .exec_capture(&format!("tmux kill-session -t '={WORK}'"), 4096)
        .await;
}

#[tokio::test]
async fn exec_capture_returns_stdout_and_status() {
    if !fixture_enabled() {
        return;
    }
    let conn = connect().await;
    let o = conn
        .exec_capture("echo hi; exit 3", 1024)
        .await
        .expect("exec");
    assert_eq!(o.stdout, b"hi\n");
    assert_eq!(o.exit_status, Some(3));
}

#[tokio::test]
async fn exec_capture_truncates_at_max_bytes() {
    if !fixture_enabled() {
        return;
    }
    let conn = connect().await;
    let o = conn
        .exec_capture("head -c 300000 /dev/zero", 1000)
        .await
        .expect("exec");
    assert_eq!(o.stdout.len(), 1000);
    assert_eq!(o.exit_status, Some(0));
}

#[tokio::test]
async fn per_channel_env_reaches_shell() {
    if !fixture_enabled() {
        return;
    }
    let conn = connect().await;
    let env = [(TAB_ENV.to_string(), "abc".to_string())];
    let mut shell = shell_with(&conn, &env, None).await;
    send(&shell, "echo tab=$LC_TETHER_TAB\r").await;
    read_until(&mut shell, "tab=abc", Duration::from_secs(5)).await;
    // A sibling shell on the same connection does not see it.
    let mut plain = shell_with(&conn, &[], None).await;
    send(&plain, "echo other=[$LC_TETHER_TAB]\r").await;
    read_until(&mut plain, "other=[]", Duration::from_secs(5)).await;
}

#[tokio::test]
async fn startup_reattaches_named_session() {
    if !fixture_enabled() {
        return;
    }
    let _g = SERIAL.lock().await;
    let conn = connect().await;
    new_detached_session(&conn).await;
    let cmd = tmux_reattach_command(WORK);
    let mut shell = shell_with(&conn, &[], Some(&cmd)).await;
    send(&shell, "tmux display -p '#S'\r").await;
    read_until(&mut shell, WORK, Duration::from_secs(10)).await;
    kill_session(&conn).await;
}

#[tokio::test]
async fn startup_falls_back_when_session_gone() {
    if !fixture_enabled() {
        return;
    }
    let _g = SERIAL.lock().await;
    let conn = connect().await;
    kill_session(&conn).await;
    let cmd = tmux_reattach_command(WORK);
    let mut shell = shell_with(&conn, &[], Some(&cmd)).await;
    send(&shell, "echo plain=$((20+22))\r").await;
    let seen = read_until(&mut shell, "plain=42", Duration::from_secs(10)).await;
    assert!(
        !seen.contains("tmux") && !seen.contains("can't find"),
        "{seen:?}"
    );
}

#[tokio::test]
async fn tmux_tabs_map_back_to_their_tab() {
    if !fixture_enabled() {
        return;
    }
    let _g = SERIAL.lock().await;
    let conn = connect().await;
    // No client yet: quiet success.
    kill_session(&conn).await;
    assert_eq!(out(&conn, tmux_tabs_command()).await, "");

    new_detached_session(&conn).await;
    let env = [(TAB_ENV.to_string(), "tab-7".to_string())];
    let cmd = tmux_reattach_command(WORK);
    let mut shell = shell_with(&conn, &env, Some(&cmd)).await;
    send(&shell, "tmux display -p '#S'\r").await;
    read_until(&mut shell, WORK, Duration::from_secs(10)).await;

    let listing = out(&conn, tmux_tabs_command()).await;
    assert_eq!(listing, format!("tab-7\t{WORK}\n"));
    kill_session(&conn).await;
}

/// The tab ids, in order, of the tabs that are in a tmux session.
async fn tabs(conn: &SshConnection) -> Vec<(String, String)> {
    let o = conn
        .exec_capture(tmux_tabs_command(), 64 * 1024)
        .await
        .expect("exec");
    let mut tabs: Vec<_> = parse_tmux_tabs(&o.stdout).into_iter().collect();
    tabs.sort();
    tabs
}

fn pair(tab: &str, session: &str) -> (String, String) {
    (tab.to_owned(), session.to_owned())
}

/// Polls the tab map until it equals `want` (a client takes a moment to
/// start).
async fn wait_for_tabs(conn: &SshConnection, want: &[(String, String)]) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let got = tabs(conn).await;
        if got == want {
            return;
        }
        assert!(Instant::now() < deadline, "tabs {got:?}, want {want:?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn tab(conn: &SshConnection, id: &str, startup: Option<&str>) -> ShellHandle {
    shell_with(conn, &[(TAB_ENV.to_string(), id.to_string())], startup).await
}

const A: &str = "tether a";
const B: &str = "tether 'b' $y";

/// Review focus 1: two tabs in two tmux sessions and a plain tab, on one
/// connection, drop together; each comes back to its own session (the plain
/// one to a plain shell), and the reattached tabs are tracked again.
#[tokio::test]
async fn tracks_two_tabs_in_two_sessions_and_a_plain_tab() {
    if !fixture_enabled() {
        return;
    }
    let _g = SERIAL.lock().await;
    let conn = connect().await;
    // The tmux tests run one at a time, so no other test's session is lost.
    let _ = conn.exec_capture("tmux kill-server", 4096).await;
    // Typed into the shells, as a user would.
    let mut t1 = tab(&conn, "t1", None).await;
    let mut t2 = tab(&conn, "t2", None).await;
    let mut t3 = tab(&conn, "t3", None).await;
    send(&t1, "tmux new -s 'tether a'\r").await;
    send(&t2, "tmux new -s \"tether 'b' \\$y\"\r").await;
    send(&t3, "echo ready=$((20+22))\r").await;
    read_until(&mut t3, "ready=42", Duration::from_secs(10)).await;
    wait_for_tabs(&conn, &[pair("t1", A), pair("t2", B)]).await;

    // The connection drops (its sshd process is killed, as a reset would);
    // every tab sees its shell end.
    let _ = conn
        .exec_capture("exec /bin/sh -c 'kill -9 $PPID'", 1024)
        .await;
    for shell in [&mut t1, &mut t2, &mut t3] {
        let end = tokio::time::timeout(Duration::from_secs(10), async {
            while shell.output.recv().await.is_some() {}
        })
        .await;
        assert!(end.is_ok(), "a shell did not end with the connection");
    }
    drop((t1, t2, t3, conn));

    // Reconnect: the tmux tabs reattach, the plain one stays plain. Each new
    // shell gets a fresh tab id.
    let started = Instant::now();
    let conn = connect().await;
    let connected = started.elapsed();
    let (r1, r2) = (tmux_reattach_command(A), tmux_reattach_command(B));
    let opened = Instant::now();
    let mut n1 = tab(&conn, "n1", Some(&r1)).await;
    send(&n1, "tmux display -p 'in=#S'\r").await;
    read_until(&mut n1, &format!("in={A}"), Duration::from_secs(10)).await;
    let reattach = opened.elapsed();
    let mut n2 = tab(&conn, "n2", Some(&r2)).await;
    send(&n2, "tmux display -p 'in=#S'\r").await;
    read_until(&mut n2, &format!("in={B}"), Duration::from_secs(10)).await;
    let opened = Instant::now();
    let mut n3 = tab(&conn, "n3", None).await;
    send(&n3, "echo plain=$((20+22)) tmux=[$TMUX]\r").await;
    read_until(&mut n3, "plain=42 tmux=[]", Duration::from_secs(10)).await;
    let plain = opened.elapsed();
    wait_for_tabs(&conn, &[pair("n1", A), pair("n2", B)]).await;
    eprintln!(
        "timing: connect {} ms, reattached shell answers in {} ms, plain shell in {} ms",
        connected.as_millis(),
        reattach.as_millis(),
        plain.as_millis()
    );

    let _ = conn.exec_capture("tmux kill-server", 4096).await;
}

#[tokio::test]
async fn detaching_lands_in_a_plain_login_shell() {
    if !fixture_enabled() {
        return;
    }
    let _g = SERIAL.lock().await;
    let conn = connect().await;
    new_detached_session(&conn).await;
    let cmd = tmux_reattach_command(WORK);
    let mut shell = shell_with(&conn, &[], Some(&cmd)).await;
    send(&shell, "tmux detach-client\r").await;
    // Only the login shell can answer after tmux's own "[detached" notice.
    read_until(&mut shell, "[detached", Duration::from_secs(10)).await;
    send(&shell, "echo plain=$((20+22))\r").await;
    read_until(&mut shell, "plain=42", Duration::from_secs(10)).await;
    kill_session(&conn).await;
}

/// Names that break naive quoting in some shell.
const NAMES: &[&str] = &[
    "work",
    WORK,
    "it's",
    "a b\\c `d` $(e) ; f",
    "-x",
    "a\\",
    "\\'",
    "a\\\\b",
    "\\' ; touch /tmp/tether-pwned ; '",
    "h\u{e9}llo \u{2713}",
    "two\nlines",
    "trailing newline\n",
];

const SHIM_DIR: &str = "/tmp/tether-shim";

fn docker_exec(args: &[&str]) -> std::process::Output {
    Command::new("docker")
        .arg("exec")
        .arg("tether-sshd")
        .args(args)
        .output()
        .expect("run docker exec")
}

/// The generated commands, run as `$SHELL -c <command>` by each login shell
/// in the fixture container (busybox ash, bash, fish), with a stand-in
/// `tmux` that prints the target it was given.
#[test]
fn startup_commands_survive_every_login_shell_in_the_container() {
    if !fixture_enabled() {
        return;
    }
    let setup = docker_exec(&[
        "sh",
        "-c",
        &format!(
            "mkdir -p {SHIM_DIR} && printf '%s\\n' \"$1\" >{SHIM_DIR}/tmux && \
             printf '%s\\n' \"$2\" >{SHIM_DIR}/login && chmod 755 {SHIM_DIR}/tmux {SHIM_DIR}/login"
        ),
        "sh",
        "#!/bin/sh\ncase $1 in has-session) exit 0;; attach) printf %s \"$3\";; \
         list-clients) echo \"$PPID  s one \";; esac",
        "#!/bin/sh\nprintf '|login %s' \"$*\"",
    ]);
    assert!(setup.status.success(), "{setup:?}");

    let path = format!("PATH={SHIM_DIR}:/usr/local/bin:/usr/bin:/bin");
    let login = format!("SHELL={SHIM_DIR}/login");
    let run = |shell: &str, command: &str| {
        let o = docker_exec(&["env", &path, &login, shell, "-c", command]);
        (
            String::from_utf8_lossy(&o.stdout).into_owned(),
            String::from_utf8_lossy(&o.stderr).into_owned(),
        )
    };
    for shell in ["/bin/sh", "/bin/bash", "/usr/bin/fish"] {
        for name in NAMES {
            let (got, err) = run(shell, &tmux_reattach_command(name));
            assert_eq!(got, format!("={name}|login -l"), "{shell}, {name:?}");
            assert_eq!(err, "", "{shell}, {name:?}");
        }
        // The stand-in's parent is the shell running the command, which has
        // the tab id in its environment.
        let o = docker_exec(&[
            "env",
            &path,
            &login,
            &format!("{TAB_ENV}=tab 1"),
            shell,
            "-c",
            tmux_tabs_command(),
        ]);
        let got = String::from_utf8_lossy(&o.stdout);
        assert_eq!(got, "tab 1\t s one \n", "{shell}");
    }
    let gone = docker_exec(&["test", "!", "-e", "/tmp/tether-pwned"]);
    docker_exec(&["rm", "-rf", SHIM_DIR]);
    assert!(gone.status.success(), "a session name was run as code");
}
