//! `SftpClient` and `TransferQueue` against the docker sshd fixture
//! (127.0.0.1:2222). Run with `TETHER_SSH_FIXTURE=1`; otherwise every test
//! prints a skip line and returns early. Every test lives in `mod transfers`
//! so `cargo test -p core-api transfers` selects them all.

mod transfers {
    use std::io::Read;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use core_api::{
        ConflictPolicy, Direction, JobId, Prompt, PromptBridge, PromptResponse, TransferEvent,
        TransferJob, TransferQueue,
    };
    use core_ssh::{AcceptAll, AuthMethod, ConnectParams, NoPrompter, SftpClient, SshConnection};
    use secrecy::SecretString;
    use sha2::{Digest, Sha256};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::sync::mpsc;
    use tokio::time::timeout;

    const CONTAINER: &str = "tether-sshd";
    const MIB: u64 = 1024 * 1024;

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

    /// Runs a command in the fixture container with `args` passed verbatim
    /// (no shell); returns stdout.
    fn docker(args: &[&str]) -> String {
        let out = Command::new("docker")
            .arg("exec")
            .arg(CONTAINER)
            .args(args)
            .output()
            .expect("run docker exec");
        assert!(
            out.status.success(),
            "docker exec {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).expect("utf-8 output")
    }

    fn remote_sha256(path: &str) -> String {
        // Hashes stdin: `sha256sum FILE` escapes odd names in its output.
        // The path is a positional argument, never parsed by the shell.
        docker(&["sh", "-c", "sha256sum < \"$1\"", "sh", path])
            .split_whitespace()
            .next()
            .expect("sha256sum output")
            .to_owned()
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn local_sha256(path: &Path) -> String {
        let mut f = std::fs::File::open(path).expect("open local file");
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = f.read(&mut buf).expect("read local file");
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        hex(&hasher.finalize())
    }

    /// Writes `len` pseudo-random bytes to `path` (xorshift64* with a random
    /// seed; Windows has no `/dev/urandom`).
    fn random_file(path: &Path, len: u64) {
        use std::hash::{BuildHasher, RandomState};
        use std::io::Write;
        let mut state = RandomState::new().hash_one(path) | 1;
        let mut dst = std::fs::File::create(path).expect("create local file");
        let mut buf = vec![0u8; 64 * 1024];
        let mut left = len;
        while left > 0 {
            for word in buf.as_chunks_mut::<8>().0 {
                state ^= state >> 12;
                state ^= state << 25;
                state ^= state >> 27;
                *word = state.wrapping_mul(0x2545_F491_4F6C_DD1D).to_le_bytes();
            }
            let n = left.min(buf.len() as u64) as usize;
            dst.write_all(&buf[..n]).expect("fill local file");
            left -= n as u64;
        }
    }

    /// A unique remote directory, removed (as root, whatever is inside) on
    /// drop.
    struct RemoteDir(String);

    impl RemoteDir {
        async fn create(sftp: &SftpClient) -> Self {
            let path = format!("/tmp/tether-test-{}", uuid::Uuid::now_v7());
            sftp.mkdir(&path).await.expect("mkdir remote temp dir");
            Self(path)
        }

        fn join(&self, name: &str) -> String {
            format!("{}/{name}", self.0)
        }
    }

    impl Drop for RemoteDir {
        fn drop(&mut self) {
            let _ = Command::new("docker")
                .args(["exec", CONTAINER, "rm", "-rf", "--", &self.0])
                .output();
        }
    }

    /// Prompts emitted by the queue's bridge, for tests to answer.
    type PromptRx = mpsc::UnboundedReceiver<(String, Prompt)>;

    struct Harness {
        sftp: SftpClient,
        queue: TransferQueue,
        events: mpsc::UnboundedReceiver<TransferEvent>,
        bridge: Arc<PromptBridge>,
        prompts: PromptRx,
        _conn: SshConnection,
    }

    async fn harness() -> Harness {
        let conn = connect().await;
        let sftp = SftpClient::open(&conn).await.expect("open sftp");
        let (ptx, prompts) = mpsc::unbounded_channel();
        let bridge = Arc::new(PromptBridge::new(Arc::new(move |id, p| {
            ptx.send((id, p)).map_err(|e| e.to_string())
        })));
        let (queue, events) = TransferQueue::new(sftp.clone(), bridge.clone());
        Harness {
            sftp,
            queue,
            events,
            bridge,
            prompts,
            _conn: conn,
        }
    }

    fn job_of(e: &TransferEvent) -> &JobId {
        match e {
            TransferEvent::Progress { job, .. }
            | TransferEvent::Done { job, .. }
            | TransferEvent::Skipped { job }
            | TransferEvent::Cancelled { job }
            | TransferEvent::Failed { job, .. } => job,
        }
    }

    fn is_terminal(e: &TransferEvent) -> bool {
        !matches!(e, TransferEvent::Progress { .. })
    }

    impl Harness {
        /// Collects `job`'s events up to and including its terminal one.
        async fn finish(&mut self, job: &JobId, within: Duration) -> Vec<TransferEvent> {
            let deadline = Instant::now() + within;
            let mut seen = Vec::new();
            loop {
                let left = deadline.saturating_duration_since(Instant::now());
                let e = timeout(left, self.events.recv())
                    .await
                    .unwrap_or_else(|_| panic!("job {job} did not finish; events {seen:?}"))
                    .expect("event stream open");
                if job_of(&e) != job {
                    continue;
                }
                let end = is_terminal(&e);
                seen.push(e);
                if end {
                    return seen;
                }
            }
        }

        async fn run(&mut self, job: TransferJob) -> Vec<TransferEvent> {
            let id = self.queue.enqueue(job);
            self.finish(&id, Duration::from_secs(120)).await
        }

        async fn put(
            &mut self,
            local: &Path,
            remote: &str,
            policy: ConflictPolicy,
        ) -> TransferEvent {
            let evs = self
                .run(TransferJob {
                    direction: Direction::Upload,
                    local: local.to_path_buf(),
                    remote: remote.to_owned(),
                    policy,
                })
                .await;
            evs.last().cloned().expect("terminal event")
        }

        async fn get(
            &mut self,
            remote: &str,
            local: &Path,
            policy: ConflictPolicy,
        ) -> TransferEvent {
            let evs = self
                .run(TransferJob {
                    direction: Direction::Download,
                    local: local.to_path_buf(),
                    remote: remote.to_owned(),
                    policy,
                })
                .await;
            evs.last().cloned().expect("terminal event")
        }

        async fn read_remote(&self, path: &str) -> Vec<u8> {
            let mut f = self.sftp.open_read(path, 0).await.expect("open_read");
            let mut out = Vec::new();
            f.read_to_end(&mut out).await.expect("read remote");
            f.close().await.expect("close remote");
            out
        }

        async fn write_remote(&self, path: &str, data: &[u8]) {
            let mut f = self
                .sftp
                .open_write(path, 0, true)
                .await
                .expect("open_write");
            f.write_all(data).await.expect("write remote");
            f.close().await.expect("close remote");
        }
    }

    fn assert_done(e: &TransferEvent, dest: &str) {
        match e {
            TransferEvent::Done { dest: d, .. } => assert_eq!(d, dest),
            other => panic!("expected Done({dest:?}), got {other:?}"),
        }
    }

    /// Progress is monotonic, throttled, and ends at the total.
    fn check_progress(evs: &[TransferEvent], total: u64, min_secs: f64) {
        let progress: Vec<(u64, u64)> = evs
            .iter()
            .filter_map(|e| match e {
                TransferEvent::Progress { bytes, total, .. } => Some((*bytes, *total)),
                _ => None,
            })
            .collect();
        assert!(!progress.is_empty(), "no progress events: {evs:?}");
        assert!(
            progress.windows(2).all(|w| w[0].0 <= w[1].0),
            "progress not monotonic: {progress:?}"
        );
        assert_eq!(*progress.last().unwrap(), (total, total));
        // <= 10 per second plus the first and the final one.
        let bound = (min_secs * 10.0).ceil() as usize + 3;
        assert!(
            progress.len() <= bound,
            "{} progress events in {min_secs:.1}s",
            progress.len()
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn list_mkdir_rename_remove() {
        if !fixture_enabled() {
            return;
        }
        let h = harness().await;
        let dir = RemoteDir::create(&h.sftp).await;

        let sub = dir.join("sub");
        h.sftp.mkdir(&sub).await.expect("mkdir sub");
        let file = dir.join("a.txt");
        h.write_remote(&file, b"hello sftp").await;
        docker(&["ln", "-s", "a.txt", &dir.join("link")]);

        let mut entries = h.sftp.list_dir(&dir.0).await.expect("list_dir");
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["a.txt", "link", "sub"]);
        let a = &entries[0];
        assert_eq!(a.path, file);
        assert!(!a.is_dir && !a.is_symlink);
        assert_eq!(a.size, 10);
        assert!(a.mtime > 1_700_000_000, "mtime {}", a.mtime);
        assert_eq!(
            a.mode & 0o170000,
            0o100000,
            "regular file mode {:o}",
            a.mode
        );
        assert!(entries[1].is_symlink, "{:?}", entries[1]);
        assert!(entries[2].is_dir && !entries[2].is_symlink);

        // stat follows the link for its target's data but reports the link.
        let link = h.sftp.stat(&dir.join("link")).await.expect("stat link");
        assert!(link.is_symlink);
        assert_eq!(link.size, 10);
        assert_eq!(link.name, "link");

        h.sftp.chmod(&file, 0o600).await.expect("chmod");
        assert_eq!(h.sftp.stat(&file).await.unwrap().mode & 0o7777, 0o600);

        let moved = format!("{sub}/b.txt");
        h.sftp.rename(&file, &moved).await.expect("rename");
        assert!(h.sftp.try_stat(&file).await.unwrap().is_none());
        assert_eq!(h.read_remote(&moved).await, b"hello sftp");

        // A clear error, not a panic, for a missing path.
        let err = h.sftp.stat(&dir.join("missing")).await.unwrap_err();
        assert!(err.to_string().contains("missing"), "{err}");
        assert!(h.sftp.list_dir(&dir.join("missing")).await.is_err());
        // Removing a non-empty directory fails and leaves it.
        assert!(h.sftp.remove_dir(&sub).await.is_err());

        h.sftp.remove_file(&moved).await.expect("remove_file");
        h.sftp.remove_dir(&sub).await.expect("remove_dir");
        h.sftp
            .remove_file(&dir.join("link"))
            .await
            .expect("remove link");
        assert!(h.sftp.list_dir(&dir.0).await.unwrap().is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn upload_download_50mb_checksum() {
        if !fixture_enabled() {
            return;
        }
        let mut h = harness().await;
        let dir = RemoteDir::create(&h.sftp).await;
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src.bin");
        let len = 50 * MIB;
        random_file(&src, len);
        let want = local_sha256(&src);

        let remote = dir.join("big.bin");
        let started = Instant::now();
        let evs = h
            .run(TransferJob {
                direction: Direction::Upload,
                local: src.clone(),
                remote: remote.clone(),
                policy: ConflictPolicy::Ask,
            })
            .await;
        let secs = started.elapsed().as_secs_f64();
        assert_done(evs.last().unwrap(), &remote);
        check_progress(&evs, len, secs);
        assert!(h.prompts.try_recv().is_err(), "no conflict, so no prompt");
        assert_eq!(remote_sha256(&remote), want);

        let back = tmp.path().join("back.bin");
        let started = Instant::now();
        let evs = h
            .run(TransferJob {
                direction: Direction::Download,
                local: back.clone(),
                remote: remote.clone(),
                policy: ConflictPolicy::Ask,
            })
            .await;
        let secs = started.elapsed().as_secs_f64();
        assert_done(evs.last().unwrap(), &back.to_string_lossy());
        check_progress(&evs, len, secs);
        assert_eq!(local_sha256(&back), want);
    }

    /// Waits for a mid-transfer progress event, cancels, and returns the
    /// bytes it reported.
    async fn cancel_midway(h: &mut Harness, job: TransferJob, total: u64) -> u64 {
        let id = h.queue.enqueue(job);
        let deadline = Instant::now() + Duration::from_secs(60);
        let reported = loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let e = timeout(left, h.events.recv()).await.unwrap().unwrap();
            match e {
                TransferEvent::Progress { bytes, .. } if bytes > 0 && bytes < total => break bytes,
                TransferEvent::Progress { .. } => {}
                other => panic!("transfer ended before it could be cancelled: {other:?}"),
            }
        };
        assert!(h.queue.cancel(&id));
        let evs = h.finish(&id, Duration::from_secs(10)).await;
        assert!(
            matches!(evs.last(), Some(TransferEvent::Cancelled { .. })),
            "{evs:?}"
        );
        assert!(!h.queue.cancel(&id), "a finished job cannot be cancelled");
        assert!(h.queue.active_jobs().is_empty());
        reported
    }

    /// The first progress event of a resumed transfer.
    fn first_progress(evs: &[TransferEvent]) -> u64 {
        evs.iter()
            .find_map(|e| match e {
                TransferEvent::Progress { bytes, .. } => Some(*bytes),
                _ => None,
            })
            .expect("a progress event")
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn resume_after_cancel_completes_with_same_checksum() {
        if !fixture_enabled() {
            return;
        }
        let mut h = harness().await;
        let dir = RemoteDir::create(&h.sftp).await;
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src.bin");
        let len = 192 * MIB;
        random_file(&src, len);
        let want = local_sha256(&src);

        // Upload: cancel, then resume.
        let remote = dir.join("resume.bin");
        let up = |policy| TransferJob {
            direction: Direction::Upload,
            local: src.clone(),
            remote: remote.clone(),
            policy,
        };
        let reported = cancel_midway(&mut h, up(ConflictPolicy::Overwrite), len).await;
        let partial = h.sftp.stat(&remote).await.unwrap().size;
        assert!(
            partial >= reported && partial < len,
            "partial upload {partial}, reported {reported}, total {len}"
        );
        let window = h.sftp.write_window();
        let evs = h.run(up(ConflictPolicy::Resume)).await;
        assert_done(evs.last().unwrap(), &remote);
        assert_eq!(
            first_progress(&evs),
            partial.saturating_sub(window),
            "resumed one write window below the partial size"
        );
        assert_eq!(remote_sha256(&remote), want);
        // Resuming a complete destination re-sends its tail and changes
        // nothing.
        let evs = h.run(up(ConflictPolicy::Resume)).await;
        assert_done(evs.last().unwrap(), &remote);
        assert_eq!(first_progress(&evs), len - window);
        assert_eq!(remote_sha256(&remote), want);

        // Download: cancel, then resume.
        let back = tmp.path().join("back.bin");
        let down = |policy| TransferJob {
            direction: Direction::Download,
            local: back.clone(),
            remote: remote.clone(),
            policy,
        };
        let reported = cancel_midway(&mut h, down(ConflictPolicy::Ask), len).await;
        let partial = std::fs::metadata(&back).unwrap().len();
        assert!(
            partial >= reported && partial < len,
            "partial download {partial}, reported {reported}, total {len}"
        );
        let evs = h.run(down(ConflictPolicy::Resume)).await;
        assert_done(evs.last().unwrap(), &back.to_string_lossy());
        assert_eq!(first_progress(&evs), partial, "local partials are exact");
        assert_eq!(local_sha256(&back), want);

        // A lost in-flight write followed by a landed one leaves a hole below
        // the remote size; a resumed upload re-sends over it.
        let small = tmp.path().join("small.bin");
        let small_len = 8 * MIB;
        random_file(&small, small_len);
        let data = std::fs::read(&small).unwrap();
        let small_want = local_sha256(&small);
        let hole = |mut v: Vec<u8>, at: u64| {
            let at = at as usize;
            v[at..at + 512 * 1024].fill(0);
            v
        };
        let cases = [
            // A partial file with a hole just below its end.
            hole(data[..6 * MIB as usize].to_vec(), 5 * MIB),
            // A full-size file with a hole in its last window.
            hole(data.clone(), small_len - MIB),
        ];
        for (i, content) in cases.into_iter().enumerate() {
            let path = dir.join(&format!("hole-{i}.bin"));
            h.write_remote(&path, &content).await;
            assert_ne!(remote_sha256(&path), small_want);
            let e = h.put(&small, &path, ConflictPolicy::Resume).await;
            assert_done(&e, &path);
            assert_eq!(remote_sha256(&path), small_want, "hole case {i}");
        }
        // A remote file larger than the source is not a prefix of it: left
        // alone.
        let larger = dir.join("larger.bin");
        let mut bigger = data.clone();
        bigger.push(0x55);
        h.write_remote(&larger, &bigger).await;
        let before = remote_sha256(&larger);
        assert_done(
            &h.put(&small, &larger, ConflictPolicy::Resume).await,
            &larger,
        );
        assert_eq!(remote_sha256(&larger), before);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn conflict_skip_and_rename() {
        if !fixture_enabled() {
            return;
        }
        let mut h = harness().await;
        let dir = RemoteDir::create(&h.sftp).await;
        let tmp = tempfile::tempdir().unwrap();
        let first = tmp.path().join("first.txt");
        let second = tmp.path().join("second.txt");
        std::fs::write(&first, b"first version, longer").unwrap();
        std::fs::write(&second, b"second").unwrap();
        let target = dir.join("file.txt");

        assert_done(&h.put(&first, &target, ConflictPolicy::Ask).await, &target);

        // Skip leaves the destination alone.
        let e = h.put(&second, &target, ConflictPolicy::Skip).await;
        assert!(matches!(e, TransferEvent::Skipped { .. }), "{e:?}");
        assert_eq!(h.read_remote(&target).await, b"first version, longer");

        // Rename picks the first free "name (n).ext".
        let one = dir.join("file (1).txt");
        let two = dir.join("file (2).txt");
        assert_done(&h.put(&second, &target, ConflictPolicy::Rename).await, &one);
        assert_done(&h.put(&second, &target, ConflictPolicy::Rename).await, &two);
        assert_eq!(h.read_remote(&one).await, b"second");
        assert_eq!(h.read_remote(&target).await, b"first version, longer");

        // Dotfiles and names without an extension.
        let rc = dir.join(".bashrc");
        let plain = dir.join("README");
        for p in [&rc, &plain] {
            assert_done(&h.put(&first, p, ConflictPolicy::Ask).await, p);
            let e = h.put(&second, p, ConflictPolicy::Rename).await;
            assert_done(&e, &format!("{p} (1)"));
        }

        // Overwrite truncates.
        assert_done(
            &h.put(&second, &target, ConflictPolicy::Overwrite).await,
            &target,
        );
        assert_eq!(h.read_remote(&target).await, b"second");

        // Ask goes through the prompt bridge.
        let id = h.queue.enqueue(TransferJob {
            direction: Direction::Upload,
            local: first.clone(),
            remote: target.clone(),
            policy: ConflictPolicy::Ask,
        });
        let (pid, prompt) = timeout(Duration::from_secs(10), h.prompts.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            prompt,
            Prompt::TransferConflict {
                job_id: id.to_string(),
                path: target.clone(),
                existing_size: 6,
                incoming_size: 21,
            }
        );
        h.bridge
            .respond(&pid, PromptResponse::Conflict(ConflictPolicy::Rename));
        let evs = h.finish(&id, Duration::from_secs(10)).await;
        assert_done(evs.last().unwrap(), &dir.join("file (3).txt"));

        // A rejected prompt (or an "ask" answer) skips.
        for answer in [
            PromptResponse::Reject,
            PromptResponse::Conflict(ConflictPolicy::Ask),
        ] {
            let id = h.queue.enqueue(TransferJob {
                direction: Direction::Upload,
                local: first.clone(),
                remote: target.clone(),
                policy: ConflictPolicy::Ask,
            });
            let (pid, _) = timeout(Duration::from_secs(10), h.prompts.recv())
                .await
                .unwrap()
                .unwrap();
            h.bridge.respond(&pid, answer);
            let evs = h.finish(&id, Duration::from_secs(10)).await;
            assert!(
                matches!(evs.last(), Some(TransferEvent::Skipped { .. })),
                "{evs:?}"
            );
        }
        assert_eq!(h.read_remote(&target).await, b"second");

        // Download conflicts on the local side.
        let local = tmp.path().join("file.txt");
        std::fs::write(&local, b"local").unwrap();
        let e = h.get(&target, &local, ConflictPolicy::Skip).await;
        assert!(matches!(e, TransferEvent::Skipped { .. }), "{e:?}");
        assert_eq!(std::fs::read(&local).unwrap(), b"local");
        let renamed = tmp.path().join("file (1).txt");
        assert_done(
            &h.get(&target, &local, ConflictPolicy::Rename).await,
            &renamed.to_string_lossy(),
        );
        assert_eq!(std::fs::read(&renamed).unwrap(), b"second");
        assert_eq!(std::fs::read(&local).unwrap(), b"local");

        // Errors are reported, not panicked on.
        let e = h
            .get(
                &dir.join("missing"),
                &tmp.path().join("m"),
                ConflictPolicy::Ask,
            )
            .await;
        assert!(matches!(e, TransferEvent::Failed { .. }), "{e:?}");
        assert!(
            !tmp.path().join("m").exists(),
            "no destination for a missing source"
        );
        let e = h
            .put(
                &tmp.path().join("nope"),
                &dir.join("m"),
                ConflictPolicy::Ask,
            )
            .await;
        assert!(matches!(e, TransferEvent::Failed { .. }), "{e:?}");
        assert!(h.sftp.try_stat(&dir.join("m")).await.unwrap().is_none());
        let e = h.put(&first, &dir.0, ConflictPolicy::Overwrite).await;
        assert!(
            matches!(e, TransferEvent::Failed { .. }),
            "dest is a dir: {e:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn sftp_realpath_dot_is_the_login_directory() {
        if !fixture_enabled() {
            return;
        }
        let h = harness().await;
        let home = docker(&["sh", "-c", "getent passwd tether | cut -d: -f6"])
            .trim()
            .to_owned();
        assert!(home.starts_with('/'), "fixture home: {home:?}");
        assert_eq!(h.sftp.realpath(".").await.unwrap(), home);
        assert_eq!(h.sftp.realpath("/tmp/../tmp/").await.unwrap(), "/tmp");
        // Relative paths resolve against the login directory.
        assert_eq!(h.sftp.realpath("./").await.unwrap(), home);
    }

    /// `name` as a local file name: verbatim on Unix; on Windows, which
    /// forbids `<>:"/\|?*` and control characters in file names, those become
    /// `_` (the remote side keeps the odd name on every platform).
    fn local_name(name: &str) -> String {
        if cfg!(windows) {
            name.chars()
                .map(|c| {
                    if c.is_control() || "<>:\"/\\|?*".contains(c) {
                        '_'
                    } else {
                        c
                    }
                })
                .collect()
        } else {
            name.to_owned()
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn sftp_paths_with_spaces_and_unicode() {
        if !fixture_enabled() {
            return;
        }
        let mut h = harness().await;
        let dir = RemoteDir::create(&h.sftp).await;
        let tmp = tempfile::tempdir().unwrap();

        let spaced = dir.join("dir with space");
        h.sftp.mkdir(&spaced).await.expect("mkdir with space");
        let names = [
            "日本語 'q'.txt",
            "line1\nline2 \"dq\" $HOME `x` \\ *.txt",
            "emoji 🚀 ; rm -rf ~.txt",
        ];
        for (i, name) in names.iter().enumerate() {
            let remote = format!("{spaced}/{name}");
            let local_src = tmp.path().join(format!("src-{i}"));
            std::fs::write(&local_src, name.as_bytes()).unwrap();
            assert_done(
                &h.put(&local_src, &remote, ConflictPolicy::Ask).await,
                &remote,
            );
            // The server sees exactly this name.
            assert_eq!(remote_sha256(&remote), local_sha256(&local_src));

            let local_dst: PathBuf = tmp.path().join(local_name(name));
            assert_done(
                &h.get(&remote, &local_dst, ConflictPolicy::Ask).await,
                &local_dst.to_string_lossy(),
            );
            assert_eq!(std::fs::read(&local_dst).unwrap(), name.as_bytes());

            let st = h.sftp.stat(&remote).await.expect("stat odd name");
            assert_eq!(st.name, *name);
            assert_eq!(st.path, remote);
        }

        let mut listed: Vec<String> = h
            .sftp
            .list_dir(&spaced)
            .await
            .unwrap()
            .into_iter()
            .map(|e| {
                assert_eq!(e.path, format!("{spaced}/{}", e.name));
                e.name
            })
            .collect();
        listed.sort();
        let mut want: Vec<String> = names.iter().map(|s| s.to_string()).collect();
        want.sort();
        assert_eq!(listed, want);

        // Rename onto another odd name and remove it.
        let from = format!("{spaced}/{}", names[1]);
        let to = format!("{spaced}/renamed\n'again'");
        h.sftp.rename(&from, &to).await.expect("rename odd");
        assert_eq!(h.read_remote(&to).await, names[1].as_bytes());
        h.sftp.remove_file(&to).await.expect("remove odd");
        assert!(h.sftp.try_stat(&to).await.unwrap().is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "1 GiB transfer; run manually for the phase gate (--include-ignored)"]
    async fn upload_1gb_checksum() {
        if !fixture_enabled() {
            return;
        }
        let mut h = harness().await;
        let dir = RemoteDir::create(&h.sftp).await;
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("1g.bin");
        let len = 1024 * MIB;
        random_file(&src, len);
        let want = local_sha256(&src);

        let remote = dir.join("1g.bin");
        let id = h.queue.enqueue(TransferJob {
            direction: Direction::Upload,
            local: src.clone(),
            remote: remote.clone(),
            policy: ConflictPolicy::Ask,
        });
        let started = Instant::now();
        let evs = h.finish(&id, Duration::from_secs(600)).await;
        let secs = started.elapsed().as_secs_f64();
        assert_done(evs.last().unwrap(), &remote);
        check_progress(&evs, len, secs);
        eprintln!(
            "upload 1 GiB: {secs:.2}s, {:.1} MB/s",
            len as f64 / 1e6 / secs
        );
        assert_eq!(remote_sha256(&remote), want);
        // Both sides are removed: the remote dir by RemoteDir, the local file
        // with the tempdir.
    }
}
