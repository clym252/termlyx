//! `backup` and `restore`: consistency while serving, the data-dir lock, the
//! new epoch, validation, TLS material, and that a backup holds no plaintext.

mod common;

use std::path::Path;
use std::process::{Command, Output};

use common::*;
use sqlx::Connection;
use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection};
use tether_server::backup;

fn bin(data: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_termlyx-server"))
        .args(args)
        .env_clear()
        .env("TETHER_DATA", data)
        .env("TETHER_TLS", "off")
        .env("TETHER_BIND", "127.0.0.1:8443")
        .output()
        .unwrap()
}

/// Open a backup file on its own (read-only), to look inside.
async fn open_file(path: &Path) -> SqliteConnection {
    SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(path).read_only(true))
        .await
        .unwrap()
}

async fn scalar(conn: &mut SqliteConnection, sql: &'static str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(conn).await.unwrap()
}

fn pushes(n: usize) -> Vec<core_proto::PushItem> {
    (0..n)
        .map(|_| item(row(&new_id(), "host", now_ms(), &[7; 64]), None))
        .collect()
}

#[tokio::test]
async fn backup_while_serving_is_consistent() {
    let srv = spawn().await;
    let a = register(&srv, "alice").await;
    let tok = a.tokens.access_token.clone();
    push_ok(&srv, &tok, pushes(5)).await;

    // Keep writing while the backup runs.
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let writer = {
        let (stop, url_srv_db, vault) = (stop.clone(), srv.state.db.clone(), a.vault_id.clone());
        // Writes go straight through the same transaction the push handler
        // uses: seq bump and row insert are one write transaction.
        tokio::spawn(async move {
            let mut n = 0u32;
            while !stop.load(std::sync::atomic::Ordering::SeqCst) {
                let mut tx = tether_server::db::begin_write(&url_srv_db).await.unwrap();
                let seq: i64 = sqlx::query_scalar(
                    "UPDATE vault_seq SET head = head + 1 WHERE vault_id = ? RETURNING head",
                )
                .bind(&vault)
                .fetch_one(&mut *tx)
                .await
                .unwrap();
                sqlx::query(
                    "INSERT INTO records (vault_id, id, kind, key_version, hlc, rev, seq, deleted, envelope, updated_at) \
                     VALUES (?, ?, 'host', 1, '0', 1, ?, 0, x'00', 0)",
                )
                .bind(&vault)
                .bind(format!("bg-{n}"))
                .bind(seq)
                .execute(&mut *tx)
                .await
                .unwrap();
                tx.commit().await.unwrap();
                n += 1;
                tokio::task::yield_now().await;
            }
            n
        })
    };

    for i in 0..5 {
        let path = srv.dir.path().join(format!("snap{i}.db"));
        srv.backup_to(&path).await.unwrap();
        let mut c = open_file(&path).await;
        let check: String = sqlx::query_scalar("PRAGMA integrity_check")
            .fetch_one(&mut c)
            .await
            .unwrap();
        assert_eq!(check, "ok");
        // One snapshot: the change counter and the rows agree.
        let head = scalar(&mut c, "SELECT head FROM vault_seq").await;
        let max = scalar(&mut c, "SELECT max(seq) FROM records").await;
        let count = scalar(&mut c, "SELECT count(*) FROM records").await;
        assert_eq!(head, max, "snapshot {i}");
        assert_eq!(head, count, "snapshot {i}");
        assert!(count >= 5);
        assert_eq!(scalar(&mut c, "SELECT count(*) FROM accounts").await, 1);
    }
    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    let written = writer.await.unwrap();
    assert!(
        written > 0,
        "the writer must have run alongside the backups"
    );
}

#[tokio::test]
async fn backup_refuses_to_overwrite_and_is_private() {
    let srv = spawn().await;
    register(&srv, "alice").await;
    let path = srv.dir.path().join("b.db");
    srv.backup_to(&path).await.unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
    let before = std::fs::read(&path).unwrap();
    let err = srv.backup_to(&path).await.unwrap_err();
    assert!(err.to_string().contains("already exists"), "{err}");
    assert_eq!(std::fs::read(&path).unwrap(), before, "left untouched");
    // A missing directory is an error, not a surprise mkdir.
    let err = srv
        .backup_to(&srv.dir.path().join("nope/b.db"))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("directory"), "{err}");
}

#[tokio::test]
async fn backup_from_the_cli_works_while_serving() {
    let srv = spawn().await;
    register(&srv, "alice").await;
    let out_dir = TempDir::new().unwrap();
    let path = out_dir.path().join("cli.db");
    let out = bin(srv.dir.path(), &["backup", path.to_str().unwrap()]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("cli.db"));
    let mut c = open_file(&path).await;
    assert_eq!(scalar(&mut c, "SELECT count(*) FROM accounts").await, 1);
    // No TLS tar unless asked.
    assert!(!out_dir.path().join("cli.db.tls.tar").exists());
    // Again to the same path: refused, non-zero exit.
    let out = bin(srv.dir.path(), &["backup", path.to_str().unwrap()]);
    assert!(!out.status.success());
}

#[tokio::test]
async fn restore_refused_while_running() {
    let srv = spawn().await;
    register(&srv, "alice").await;
    let snap = TempDir::new().unwrap();
    let path = snap.path().join("b.db");
    srv.backup_to(&path).await.unwrap();
    register_with(&srv, "bob", 2, 1).await;

    // The real binary refuses ...
    let out = bin(srv.dir.path(), &["restore", path.to_str().unwrap()]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("running"), "{err}");
    // ... and so does the library, and nothing changed.
    let err = backup::restore(srv.dir.path(), &path, false)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("running"), "{err}");
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM accounts")
        .fetch_one(&srv.state.db)
        .await
        .unwrap();
    assert_eq!(n, 2);
    assert_eq!(srv.get("/v1/health").await.status, 200);

    // A second server cannot start on the same data directory either.
    let err = tether_server::start((*srv.state.cfg).clone())
        .await
        .err()
        .expect("second server must not start");
    assert!(err.to_string().contains("already"), "{err}");

    // But `backup` is fine, as is any admin command.
    let again = snap.path().join("c.db");
    assert!(
        bin(srv.dir.path(), &["backup", again.to_str().unwrap()])
            .status
            .success()
    );
    assert!(bin(srv.dir.path(), &["users", "list"]).status.success());
}

#[tokio::test]
async fn a_stale_lock_file_does_not_block() {
    let srv = spawn().await;
    register(&srv, "alice").await;
    let snap = TempDir::new().unwrap();
    let path = snap.path().join("b.db");
    srv.backup_to(&path).await.unwrap();
    let dir = srv.dir.path().to_path_buf();
    assert!(dir.join("server.lock").exists());
    // Restart (stop + start) and restore-through-the-helper both reuse the
    // file the stopped server left behind.
    let srv = srv.restart().await;
    let srv = srv.restore_from(&path).await.unwrap();
    assert_eq!(srv.get("/v1/health").await.status, 200);
}

#[tokio::test]
async fn restore_changes_epoch() {
    let srv = spawn().await;
    let a = register(&srv, "alice").await;
    let old_epoch = srv.state.epoch.clone();
    let snap = TempDir::new().unwrap();
    let path = snap.path().join("b.db");
    srv.backup_to(&path).await.unwrap();
    // After the backup: more data, which the restore rewinds.
    register_with(&srv, "bob", 2, 1).await;
    push_ok(&srv, &a.tokens.access_token, pushes(2)).await;

    let srv = srv.restore_from(&path).await.unwrap();
    assert_ne!(srv.state.epoch, old_epoch, "a rewind must be detectable");
    assert_eq!(srv.state.epoch.len(), 32);
    let health: serde_json::Value = srv.get("/v1/health").await.json();
    assert_eq!(health["epoch"], srv.state.epoch.as_str());
    // The state is the backup's: alice yes, bob no, her pushes gone.
    let names: Vec<String> = sqlx::query_scalar("SELECT username FROM accounts")
        .fetch_all(&srv.state.db)
        .await
        .unwrap();
    assert_eq!(names, vec!["alice".to_string()]);
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM records")
        .fetch_one(&srv.state.db)
        .await
        .unwrap();
    assert_eq!(n, 0);
    // Her old session still works (it is in the backup), now with the new epoch.
    let page = pull(&srv, &a.tokens.access_token, 0, None).await;
    assert_eq!(page.epoch, srv.state.epoch);

    // The backup file itself is not changed by restoring from it, and a second
    // restore from it gives yet another epoch.
    let e1 = srv.state.epoch.clone();
    let srv = srv.restore_from(&path).await.unwrap();
    assert_ne!(srv.state.epoch, e1);
    assert_ne!(srv.state.epoch, old_epoch);
    let mut c = open_file(&path).await;
    let in_backup: Vec<u8> = sqlx::query_scalar("SELECT v FROM meta WHERE k = 'epoch'")
        .fetch_one(&mut c)
        .await
        .unwrap();
    assert_eq!(String::from_utf8(in_backup).unwrap(), old_epoch);
}

#[tokio::test]
async fn restore_from_the_cli_into_a_stopped_server() {
    let src = spawn().await;
    register(&src, "alice").await;
    let snap = TempDir::new().unwrap();
    let path = snap.path().join("b.db");
    src.backup_to(&path).await.unwrap();
    let old_epoch = src.state.epoch.clone();

    // A fresh data directory, no server: restore from the command line.
    let data = TempDir::new().unwrap();
    let out = bin(data.path(), &["restore", path.to_str().unwrap()]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("restored") && text.contains("epoch"),
        "{text}"
    );
    let cfg = {
        let mut c = tether_server::Config::new(data.path().to_path_buf());
        c.tls = tether_server::TlsMode::Off;
        c.bind = "127.0.0.1:0".parse().unwrap();
        c
    };
    let running = tether_server::start(cfg).await.unwrap();
    assert_ne!(running.state.epoch, old_epoch);
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM accounts")
        .fetch_one(&running.state.db)
        .await
        .unwrap();
    assert_eq!(n, 1);
    running.shutdown().await;
}

#[tokio::test]
async fn restore_rejects_bad_input_and_changes_nothing() {
    let srv = spawn().await;
    register(&srv, "alice").await;
    let dir = srv.dir.path().to_path_buf();
    let snap = TempDir::new().unwrap();

    // Not a database at all, an empty file, a database of something else, a
    // database from a newer server, and a missing file.
    let garbage = snap.path().join("garbage.db");
    std::fs::write(
        &garbage,
        b"this is not sqlite, it is just text. ".repeat(200),
    )
    .unwrap();
    let empty = snap.path().join("empty.db");
    std::fs::write(&empty, b"").unwrap();
    let foreign = snap.path().join("foreign.db");
    {
        let mut c = SqliteConnection::connect_with(
            &SqliteConnectOptions::new()
                .filename(&foreign)
                .create_if_missing(true),
        )
        .await
        .unwrap();
        sqlx::query("CREATE TABLE other (x)")
            .execute(&mut c)
            .await
            .unwrap();
    }
    let newer = snap.path().join("newer.db");
    srv.backup_to(&newer).await.unwrap();
    {
        let mut c = SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&newer))
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) \
             VALUES (9999, 'from the future', 1, x'00', 0)",
        )
        .execute(&mut c)
        .await
        .unwrap();
    }
    let corrupt = snap.path().join("corrupt.db");
    srv.backup_to(&corrupt).await.unwrap();
    {
        // Damage the middle of the file (a page of the records or index).
        let mut bytes = std::fs::read(&corrupt).unwrap();
        let len = bytes.len();
        assert!(len > 8192);
        for b in &mut bytes[4096..len] {
            *b = 0xA5;
        }
        std::fs::write(&corrupt, bytes).unwrap();
    }
    let missing = snap.path().join("missing.db");

    // The server must be stopped for restore to even start: use a copy of
    // the data directory's state through a stopped instance.
    let data = TempDir::new().unwrap();
    let ok_snap = snap.path().join("ok.db");
    srv.backup_to(&ok_snap).await.unwrap();
    backup::restore(data.path(), &ok_snap, false).await.unwrap();
    let before = std::fs::read(data.path().join("tether.db")).unwrap();

    for (what, path) in [
        ("garbage", &garbage),
        ("empty", &empty),
        ("foreign", &foreign),
        ("newer", &newer),
        ("corrupt", &corrupt),
        ("missing", &missing),
    ] {
        let res = backup::restore(data.path(), path, false).await;
        assert!(res.is_err(), "{what} should be refused");
        assert_eq!(
            std::fs::read(data.path().join("tether.db")).unwrap(),
            before,
            "{what}: the database must be untouched"
        );
        let leftovers: Vec<_> = std::fs::read_dir(data.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n != "tether.db" && n != "server.lock")
            .collect();
        assert!(leftovers.is_empty(), "{what}: left {leftovers:?}");
    }
    let _ = dir;
}

#[tokio::test]
async fn restore_removes_stale_wal_and_shm() {
    let src = spawn().await;
    register(&src, "alice").await;
    let snap = TempDir::new().unwrap();
    let path = snap.path().join("b.db");
    src.backup_to(&path).await.unwrap();

    let data = TempDir::new().unwrap();
    // An old database with leftover WAL/SHM files (a crashed server).
    {
        let pool = tether_server::db::open(data.path()).await.unwrap();
        pool.close().await;
    }
    std::fs::write(data.path().join("tether.db-wal"), b"stale wal frames").unwrap();
    std::fs::write(data.path().join("tether.db-shm"), b"stale shm").unwrap();
    backup::restore(data.path(), &path, false).await.unwrap();
    assert!(!data.path().join("tether.db-wal").exists());
    assert!(!data.path().join("tether.db-shm").exists());
    // And the result opens and holds the backup's data.
    let pool = tether_server::db::open(data.path()).await.unwrap();
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM accounts")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 1);
}

#[tokio::test]
async fn backup_with_tls_carries_the_key_and_restores_it() {
    let srv = spawn_tls().await;
    register(&srv, "alice").await;
    let snap = TempDir::new().unwrap();
    let path = snap.path().join("b.db");
    let tar = snap.path().join("b.db.tls.tar");
    backup::backup(&srv.state.db, srv.dir.path(), &path, true)
        .await
        .unwrap();
    assert!(tar.exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&tar).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    // A standard tar reads it.
    let listing = Command::new("tar").arg("-tf").arg(&tar).output();
    if let Ok(listing) = listing {
        assert!(listing.status.success());
        let names = String::from_utf8_lossy(&listing.stdout);
        let mut names: Vec<&str> = names.lines().collect();
        names.sort();
        assert_eq!(names, vec!["cert.pem", "key.pem"]);
    }
    // Refuses to overwrite either file.
    assert!(
        backup::backup(&srv.state.db, srv.dir.path(), &path, true)
            .await
            .is_err()
    );

    // Restore into an empty directory: same key, same pin.
    let data = TempDir::new().unwrap();
    // Without --with-tls the TLS material is not touched.
    backup::restore(data.path(), &path, false).await.unwrap();
    assert!(!data.path().join("tls").exists());
    backup::restore(data.path(), &path, true).await.unwrap();
    for f in ["key.pem", "cert.pem"] {
        assert_eq!(
            std::fs::read(data.path().join("tls").join(f)).unwrap(),
            std::fs::read(srv.dir.path().join("tls").join(f)).unwrap(),
            "{f}"
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let m = std::fs::metadata(data.path().join("tls/key.pem")).unwrap();
        assert_eq!(m.permissions().mode() & 0o777, 0o600);
    }
    let mut cfg = tether_server::Config::new(data.path().to_path_buf());
    cfg.names = vec!["localhost".into()];
    let material = tether_server::tls::load_or_create(&cfg).unwrap();
    assert_eq!(material.spki_pin(), srv.pin);
}

#[tokio::test]
async fn tls_flag_without_material_or_tar_is_an_error() {
    // Backup: plain-HTTP server has no TLS material.
    let srv = spawn().await;
    let snap = TempDir::new().unwrap();
    let path = snap.path().join("b.db");
    let err = backup::backup(&srv.state.db, srv.dir.path(), &path, true)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("TLS"), "{err}");
    assert!(!path.exists(), "no half-made backup");
    assert!(!snap.path().join("b.db.tls.tar").exists());

    // Restore: a backup made without --with-tls has no tar to restore from.
    srv.backup_to(&path).await.unwrap();
    let data = TempDir::new().unwrap();
    let err = backup::restore(data.path(), &path, true).await.unwrap_err();
    assert!(err.to_string().contains("tls.tar"), "{err}");
    assert!(!data.path().join("tether.db").exists(), "nothing was done");
}

#[tokio::test]
async fn backup_holds_no_plaintext() {
    const CANARY: &str = "CANARY-7f3a";
    let srv = spawn().await;
    let a = register(&srv, "alice").await;
    let tok = &a.tokens.access_token;
    let key = [7u8; 32];

    // Real sealed envelopes around the canary, as in the database test.
    let mut envelopes = vec![];
    let mut items = vec![];
    for i in 0..4 {
        let id = new_id();
        let plaintext = format!(
            "{{\"label\":\"{CANARY} host {i}\",\"address\":\"{CANARY}.example.com\",\"password\":\"{CANARY}\"}}"
        );
        let env = core_crypto::envelope::seal(
            &key,
            &core_crypto::envelope::Aad {
                id: &id,
                vault_id: &a.vault_id,
                kind: "host",
                key_version: 1,
            },
            plaintext.as_bytes(),
        );
        envelopes.push(env.clone());
        items.push(item(row(&id, "host", now_ms(), &env), None));
    }
    push_ok(&srv, tok, items).await;

    let snap = TempDir::new().unwrap();
    let path = snap.path().join("b.db");
    srv.backup_to(&path).await.unwrap();
    let bytes = std::fs::read(&path).unwrap();

    // Positive control: the sealed envelopes are in the backup (it is a
    // faithful copy), so the search can find things.
    let envs = Needles::default().envelopes(&envelopes);
    assert_eq!(
        envs.found_in(&bytes)
            .iter()
            .filter(|w| w.ends_with("(raw prefix)"))
            .count(),
        envelopes.len()
    );
    Needles::default()
        .canaries(&[CANARY, "example.com"])
        .assert_absent("the backup", &bytes);
    Needles::default()
        .secret("access token", &a.tokens.access_token)
        .secret("refresh token", &a.tokens.refresh_token)
        .secret("device secret", &a.tokens.device_secret)
        .assert_absent("the backup", &bytes);
}

#[tokio::test]
async fn the_serve_binary_holds_the_lock_until_it_dies() {
    use std::process::Stdio;
    let dir = TempDir::new().unwrap();
    let snap = TempDir::new().unwrap();
    // A backup to restore from.
    let src = spawn().await;
    register(&src, "alice").await;
    let path = snap.path().join("b.db");
    src.backup_to(&path).await.unwrap();

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_termlyx-server"));
    cmd.arg("serve").env_clear();
    // Winsock fails to initialise (os error 10106) without SystemRoot.
    #[cfg(windows)]
    cmd.env(
        "SystemRoot",
        std::env::var_os("SystemRoot").unwrap_or_default(),
    );
    let mut server = cmd
        .env("TETHER_DATA", dir.path())
        .env("TETHER_TLS", "off")
        .env("TETHER_BIND", "127.0.0.1:0")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    // Wait until it has the lock: restore starts refusing.
    let mut refused = false;
    for _ in 0..100 {
        let out = bin(dir.path(), &["restore", path.to_str().unwrap()]);
        if String::from_utf8_lossy(&out.stderr).contains("running") {
            refused = true;
            break;
        }
        // (A restore that got in first would have succeeded and released the
        // lock again; the server then starts and the next try is refused.)
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(refused, "restore never saw the running server");
    // Killed without a clean shutdown: the OS drops the lock, the file stays,
    // and restore works.
    server.kill().unwrap();
    server.wait().unwrap();
    assert!(dir.path().join("server.lock").exists());
    let out = bin(dir.path(), &["restore", path.to_str().unwrap()]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
