//! `ForwardManager` (local, remote and dynamic port forwards) against the
//! docker sshd fixture (127.0.0.1:2222, with `web:80` = traefik/whoami on
//! its internal network). Run with `TETHER_SSH_FIXTURE=1`; otherwise every
//! test prints a skip line and returns early. Every test lives in
//! `mod forwards` so `cargo test -p core-api forwards` selects them all.

mod forwards {
    use std::net::SocketAddr;
    use std::sync::Arc;
    use std::time::Duration;

    use core_api::{ForwardEvent, ForwardManager, ForwardStatus, RuleHandle};
    use core_model::{ForwardKind, ForwardRule};
    use core_ssh::{AcceptAll, AuthMethod, ConnectParams, NoPrompter, SshConnection};
    use secrecy::SecretString;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::sync::mpsc;
    use tokio::time::timeout;
    use uuid::Uuid;

    const CONTAINER: &str = "tether-sshd";
    const WAIT: Duration = Duration::from_secs(20);

    fn fixture_enabled() -> bool {
        let on = std::env::var("TETHER_SSH_FIXTURE").as_deref() == Ok("1");
        if !on {
            eprintln!("skipping: set TETHER_SSH_FIXTURE=1 (and run tests/fixtures/up.sh)");
        }
        on
    }

    async fn connect() -> SshConnection {
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
        SshConnection::connect(&params, Arc::new(AcceptAll), Arc::new(NoPrompter))
            .await
            .expect("connect")
    }

    fn rule(kind: ForwardKind, bind_port: u16, dest: Option<(&str, u16)>) -> ForwardRule {
        ForwardRule {
            id: Uuid::now_v7(),
            host_id: Uuid::now_v7(),
            label: format!("{kind:?} test"),
            kind,
            bind_addr: "127.0.0.1".into(),
            bind_port,
            dest_host: dest.map(|(h, _)| h.to_owned()),
            dest_port: dest.map(|(_, p)| p),
            autostart: false,
            extra: Default::default(),
        }
    }

    /// Waits until the rule leaves `Starting`.
    async fn settled(handle: &mut RuleHandle) -> ForwardStatus {
        timeout(WAIT, handle.wait_settled())
            .await
            .expect("rule settles in time")
    }

    async fn active(handle: &mut RuleHandle) -> SocketAddr {
        match settled(handle).await {
            ForwardStatus::Active { bound } => bound,
            other => panic!("expected Active, got {other:?}"),
        }
    }

    /// One HTTP/1.0 GET through `addr` (the server closes when done).
    async fn http_get(addr: SocketAddr) -> String {
        let mut sock = timeout(WAIT, TcpStream::connect(addr))
            .await
            .expect("connect in time")
            .expect("connect to the forward");
        sock.write_all(b"GET / HTTP/1.0\r\nHost: web\r\n\r\n")
            .await
            .expect("send request");
        let mut body = Vec::new();
        timeout(WAIT, sock.read_to_end(&mut body))
            .await
            .expect("response in time")
            .expect("read response");
        String::from_utf8_lossy(&body).into_owned()
    }

    /// Runs `sh -c script` in the fixture container; returns (success, stdout).
    async fn docker_sh(script: &str) -> (bool, String) {
        let out = tokio::process::Command::new("docker")
            .args(["exec", CONTAINER, "sh", "-c", script])
            .output()
            .await
            .expect("run docker exec");
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
        )
    }

    /// Collects the events seen so far for `rule_id`.
    fn drain_events(events: &mut mpsc::UnboundedReceiver<ForwardEvent>) -> Vec<ForwardEvent> {
        let mut seen = Vec::new();
        while let Ok(e) = events.try_recv() {
            seen.push(e);
        }
        seen
    }

    fn statuses_for(seen: &[ForwardEvent], rule_id: Uuid) -> Vec<ForwardStatus> {
        seen.iter()
            .filter(|e| e.rule_id == rule_id)
            .map(|e| e.status.clone())
            .collect()
    }

    #[tokio::test]
    async fn local_forward_reaches_whoami() {
        if !fixture_enabled() {
            return;
        }
        let (manager, mut events) = ForwardManager::new();
        let r = rule(ForwardKind::Local, 0, Some(("web", 80)));
        let mut handle = manager.start(&r, connect().await);
        let bound = active(&mut handle).await;
        assert_eq!(bound.ip().to_string(), "127.0.0.1");
        assert_ne!(bound.port(), 0, "port 0 reports the OS-assigned port");

        // Several connections, one after another and at once.
        let first = http_get(bound).await;
        assert!(first.contains("Hostname:"), "got: {first}");
        let many = futures_join(bound, 4).await;
        assert!(
            many.iter().all(|b| b.contains("Hostname:")),
            "got: {many:?}"
        );

        assert_eq!(manager.status(r.id), Some(ForwardStatus::Active { bound }));
        assert!(manager.stop(r.id).await);
        assert_eq!(manager.status(r.id), Some(ForwardStatus::Stopped));
        assert_eq!(
            statuses_for(&drain_events(&mut events), r.id),
            vec![
                ForwardStatus::Starting,
                ForwardStatus::Active { bound },
                ForwardStatus::Stopped
            ]
        );
    }

    async fn futures_join(addr: SocketAddr, n: usize) -> Vec<String> {
        let tasks: Vec<_> = (0..n).map(|_| tokio::spawn(http_get(addr))).collect();
        let mut out = Vec::new();
        for t in tasks {
            out.push(t.await.expect("request task"));
        }
        out
    }

    #[tokio::test]
    async fn dynamic_socks_reaches_whoami() {
        if !fixture_enabled() {
            return;
        }
        let (manager, _events) = ForwardManager::new();
        let r = rule(ForwardKind::Dynamic, 0, None);
        let mut handle = manager.start(&r, connect().await);
        let bound = active(&mut handle).await;

        // socks5h: the proxy (and so the ssh server) resolves "web".
        let proxy = reqwest::Proxy::all(format!("socks5h://{bound}")).expect("proxy url");
        let client = reqwest::Client::builder()
            .proxy(proxy)
            .timeout(WAIT)
            .build()
            .expect("http client");
        for _ in 0..3 {
            let body = client
                .get("http://web/")
                .send()
                .await
                .expect("GET through the socks proxy")
                .text()
                .await
                .expect("body");
            assert!(body.contains("Hostname:"), "got: {body}");
        }

        // A destination the server cannot reach gets "host unreachable"
        // (the server cannot resolve it), BIND gets "command not supported",
        // and the proxy keeps serving.
        let mut connect = vec![5, 1, 0, 3, 27];
        connect.extend_from_slice(b"tether-no-such-host.invalid");
        connect.extend_from_slice(&80u16.to_be_bytes());
        assert_eq!(socks_reply(bound, &connect).await, 0x04);
        assert_eq!(
            socks_reply(bound, &[5, 2, 0, 1, 127, 0, 0, 1, 0, 80]).await,
            0x07
        );
        let body = client
            .get("http://web/")
            .send()
            .await
            .expect("GET after a failed CONNECT")
            .text()
            .await
            .expect("body");
        assert!(body.contains("Hostname:"), "got: {body}");
        manager.stop(r.id).await;
    }

    /// Sends a no-auth greeting and `request` to the SOCKS proxy at `proxy`;
    /// returns the reply code.
    async fn socks_reply(proxy: SocketAddr, request: &[u8]) -> u8 {
        let mut sock = TcpStream::connect(proxy).await.expect("connect to proxy");
        sock.write_all(&[5, 1, 0]).await.expect("greeting");
        let mut method = [0u8; 2];
        timeout(WAIT, sock.read_exact(&mut method))
            .await
            .expect("method reply in time")
            .expect("method reply");
        assert_eq!(method, [5, 0], "no-auth accepted");
        sock.write_all(request).await.expect("request");
        let mut reply = [0u8; 10];
        timeout(WAIT, sock.read_exact(&mut reply))
            .await
            .expect("reply in time")
            .expect("reply (IPv4 form)");
        assert_eq!(reply[0], 5);
        reply[1]
    }

    /// Echoes every connection's bytes back until EOF, then half-closes.
    async fn echo_server() -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind echo");
        let addr = listener.local_addr().expect("echo addr");
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let (mut rd, mut wr) = sock.split();
                    if tokio::io::copy(&mut rd, &mut wr).await.is_ok() {
                        let _ = wr.shutdown().await;
                    }
                });
            }
        });
        addr
    }

    #[tokio::test]
    async fn remote_forward_exposes_local_listener() {
        if !fixture_enabled() {
            return;
        }
        let echo = echo_server().await;
        let (manager, _events) = ForwardManager::new();
        // Port 0: the server picks the port (routed before any connection).
        let r = rule(ForwardKind::Remote, 0, Some(("127.0.0.1", echo.port())));
        let mut handle = manager.start(&r, connect().await);
        let bound = active(&mut handle).await;
        assert_ne!(bound.port(), 0, "the server-picked port is reported");

        // Inside the container: nc sends a line, half-closes (-N) and prints
        // what comes back, which needs EOF to travel both ways.
        let script = format!(
            "printf 'hello from the server side\\n' | nc -N -w 10 127.0.0.1 {}",
            bound.port()
        );
        for _ in 0..3 {
            let (ok, out) = timeout(WAIT, docker_sh(&script)).await.expect("nc in time");
            assert!(ok, "nc failed");
            assert_eq!(out, "hello from the server side\n");
        }

        // Stopping sends cancel-tcpip-forward: the server stops listening.
        assert!(manager.stop(r.id).await);
        let (ok, _) = docker_sh(&format!("nc -z -w 2 127.0.0.1 {}", bound.port())).await;
        assert!(!ok, "the remote port still accepts after stop");
    }

    #[tokio::test]
    async fn bind_conflict_marks_rule_error() {
        if !fixture_enabled() {
            return;
        }
        let taken = std::net::TcpListener::bind("127.0.0.1:0").expect("pre-bind");
        let port = taken.local_addr().expect("taken addr").port();
        let conn = connect().await;
        let (manager, mut events) = ForwardManager::new();

        let clash = rule(ForwardKind::Local, port, Some(("web", 80)));
        let ok = rule(ForwardKind::Local, 0, Some(("web", 80)));
        let mut clash_handle = manager.start(&clash, conn.clone());
        let mut ok_handle = manager.start(&ok, conn);

        let status = settled(&mut clash_handle).await;
        assert_eq!(status, ForwardStatus::Error("address in use".into()));
        let ForwardStatus::Error(msg) = &status else {
            unreachable!()
        };
        assert!(msg.contains("in use"));
        let bound = active(&mut ok_handle).await;
        let body = http_get(bound).await;
        assert!(body.contains("Hostname:"), "got: {body}");
        // The failed rule did not disturb the other one.
        assert_eq!(manager.status(clash.id), Some(status.clone()));
        assert_eq!(manager.status(ok.id), Some(ForwardStatus::Active { bound }));

        let seen = drain_events(&mut events);
        assert_eq!(
            statuses_for(&seen, clash.id),
            vec![ForwardStatus::Starting, status]
        );
        assert_eq!(
            statuses_for(&seen, ok.id),
            vec![ForwardStatus::Starting, ForwardStatus::Active { bound }]
        );
        drop(taken);
        manager.stop(ok.id).await;
    }

    #[tokio::test]
    async fn stop_releases_port() {
        if !fixture_enabled() {
            return;
        }
        let conn = connect().await;
        let (manager, _events) = ForwardManager::new();
        let r = rule(ForwardKind::Local, 0, Some(("web", 80)));
        let mut handle = manager.start(&r, conn.clone());
        let bound = active(&mut handle).await;

        // A connection in flight: the request is incomplete, so whoami waits
        // for more and the copy stays open.
        let mut in_flight = TcpStream::connect(bound).await.expect("connect");
        in_flight
            .write_all(b"GET / HTTP/1.1\r\nHost: web\r\n")
            .await
            .expect("partial request");
        // Still open before the stop (and gives the channel time to open).
        let mut buf = [0u8; 64];
        assert!(
            timeout(Duration::from_millis(500), in_flight.read(&mut buf))
                .await
                .is_err(),
            "whoami answered or closed an incomplete request"
        );

        assert!(manager.stop(r.id).await);
        assert_eq!(handle.status(), ForwardStatus::Stopped);

        // The in-flight copy ended: the client sees EOF or a reset.
        match timeout(Duration::from_secs(5), in_flight.read(&mut buf)).await {
            Ok(Ok(0) | Err(_)) => {}
            Ok(Ok(n)) => panic!("unexpected {n} bytes after stop"),
            Err(_) => panic!("in-flight connection still open after stop"),
        }
        // The listener is gone: the same port binds again...
        assert_port_free(bound).await;
        // ...and the rule can start again on it.
        let mut fixed = rule(ForwardKind::Local, bound.port(), Some(("web", 80)));
        fixed.id = r.id;
        let mut handle = manager.start(&fixed, conn);
        assert_eq!(active(&mut handle).await, bound);
        let body = http_get(bound).await;
        assert!(body.contains("Hostname:"), "got: {body}");
        manager.stop(r.id).await;
        assert_port_free(bound).await;
    }

    /// The forward's port can be bound again (so its listener is gone).
    async fn assert_port_free(bound: SocketAddr) {
        if let Err(e) = TcpListener::bind(bound).await {
            panic!("{bound} is still taken: {e}");
        }
    }

    /// An active rule with a connection in flight, on a port the test knows.
    async fn busy_local_rule(
        manager: &ForwardManager,
        conn: SshConnection,
    ) -> (ForwardRule, SocketAddr, TcpStream) {
        let r = rule(ForwardKind::Local, 0, Some(("web", 80)));
        let mut handle = manager.start(&r, conn);
        let bound = active(&mut handle).await;
        let mut in_flight = TcpStream::connect(bound).await.expect("connect");
        in_flight
            .write_all(b"GET / HTTP/1.1\r\nHost: web\r\n")
            .await
            .expect("partial request");
        let mut fixed = r;
        fixed.bind_port = bound.port();
        (fixed, bound, in_flight)
    }

    #[tokio::test]
    async fn concurrent_stops_return_after_release() {
        if !fixture_enabled() {
            return;
        }
        let (manager, mut events) = ForwardManager::new();
        let (r, bound, _in_flight) = busy_local_rule(&manager, connect().await).await;

        let a = manager.stop(r.id);
        let b = manager.stop(r.id);
        tokio::pin!(a, b);
        let a_first = tokio::select! {
            stopped = &mut a => { assert!(stopped); true }
            stopped = &mut b => { assert!(stopped); false }
        };
        // Whichever returned first: the listener is already gone.
        assert_port_free(bound).await;
        assert_eq!(manager.status(r.id), Some(ForwardStatus::Stopped));
        let other = if a_first {
            timeout(WAIT, b).await
        } else {
            timeout(WAIT, a).await
        };
        assert!(other.expect("second stop returns"));
        let stopped = statuses_for(&drain_events(&mut events), r.id)
            .into_iter()
            .filter(|s| *s == ForwardStatus::Stopped)
            .count();
        assert_eq!(stopped, 1, "Stopped is reported once");
    }

    #[tokio::test]
    async fn start_during_stop_runs_after_release() {
        if !fixture_enabled() {
            return;
        }
        let conn = connect().await;
        let (manager, mut events) = ForwardManager::new();
        let (r, bound, _in_flight) = busy_local_rule(&manager, conn.clone()).await;

        // Restart on the same fixed port while the stop is in progress: it
        // waits for the old listener, so it cannot hit "address in use".
        let stopping = manager.stop(r.id);
        let mut handle = manager.start(&r, conn.clone());
        assert!(timeout(WAIT, stopping).await.expect("stop returns"));
        assert_eq!(active(&mut handle).await, bound);
        let body = http_get(bound).await;
        assert!(body.contains("Hostname:"), "got: {body}");
        assert_eq!(
            statuses_for(&drain_events(&mut events), r.id),
            vec![
                ForwardStatus::Starting,
                ForwardStatus::Active { bound },
                ForwardStatus::Stopped,
                ForwardStatus::Starting,
                ForwardStatus::Active { bound },
            ]
        );

        // A stop after the queued start wins: the rule ends up stopped.
        let first = manager.stop(r.id);
        let _ = manager.start(&r, conn);
        let second = manager.stop(r.id);
        assert!(timeout(WAIT, first).await.expect("first stop returns"));
        assert!(timeout(WAIT, second).await.expect("second stop returns"));
        assert_eq!(manager.status(r.id), Some(ForwardStatus::Stopped));
        assert_port_free(bound).await;
    }
}
