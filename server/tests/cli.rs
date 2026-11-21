//! The `fingerprint` and `invite` subcommands, run as the real binary.

mod common;

use common::*;
use std::process::Command;

#[tokio::test]
async fn fingerprint_prints_pin_and_names() {
    let srv = spawn_with(|c| {
        c.tls = tether_server::config::TlsMode::SelfSigned;
        c.names = vec!["sync.example.com".into()];
    })
    .await;
    let data = srv.state.cfg.data_dir.clone();
    let out = Command::new(env!("CARGO_BIN_EXE_termlyx-server"))
        .arg("fingerprint")
        .env_clear()
        .env("TETHER_DATA", &data)
        .env("TETHER_NAMES", "sync.example.com")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).unwrap();
    let hex: String = srv.pin.iter().map(|b| format!("{b:02x}")).collect();
    assert!(text.contains(&format!("sha256:{hex}")), "{text}");
    assert!(text.contains("sync.example.com"), "{text}");
}

#[test]
fn fingerprint_with_tls_off_is_an_error() {
    let dir = TempDir::new().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_termlyx-server"))
        .arg("fingerprint")
        .env_clear()
        .env("TETHER_DATA", dir.path())
        .env("TETHER_TLS", "off")
        .output()
        .unwrap();
    assert!(!out.status.success());
}

fn invite_cmd(data: &std::path::Path, tls: &str, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_termlyx-server"))
        .arg("invite")
        .args(args)
        .env_clear()
        .env("TETHER_DATA", data)
        .env("TETHER_TLS", tls)
        .env("TETHER_BIND", "127.0.0.1:8443")
        .env("TETHER_NAMES", "localhost")
        .env("TETHER_URL", "https://tether.example.com")
        .output()
        .unwrap()
}

#[tokio::test]
async fn invite_prints_short_then_full_code_and_both_register() {
    use core_proto::{CodeKind, JoinCode};
    let srv = spawn().await;
    let out = invite_cmd(
        &srv.state.cfg.data_dir,
        "off",
        &["--note", "for Sam", "--expires", "12h"],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).unwrap();
    // The short code, then the full code for the same invite.
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "{text:?}");
    assert!(lines[0].starts_with("TLX-"), "{text:?}");
    let full = lines[1];
    let code = JoinCode::parse(full).unwrap();
    assert_eq!(
        code.token,
        core_proto::ShortCode::parse(lines[0]).unwrap().token()
    );
    assert_eq!(code.kind, CodeKind::Invite);
    assert_eq!(code.url, "https://tether.example.com");
    assert_eq!(code.pin, None);

    // The row carries the note and the 12 hour expiry; only a hash is stored.
    let (note, created, expires, hash): (String, i64, i64, Vec<u8>) =
        sqlx::query_as("SELECT note, created_at, expires_at, token_hash FROM invites")
            .fetch_one(&srv.state.db)
            .await
            .unwrap();
    assert_eq!(note, "for Sam");
    assert_eq!(expires - created, 12 * 60 * 60 * 1000);
    assert_ne!(hash, code.token.to_vec());

    // The running server accepts it, once.
    let req = register_req(full, "sam", 1, 1);
    assert_eq!(srv.post_json("/v1/register", &req).await.status, 200);
    let again = register_req(full, "sam2", 2, 1);
    assert_eq!(srv.post_json("/v1/register", &again).await.status, 403);
}

#[tokio::test]
async fn invite_carries_the_pin_in_self_signed_mode() {
    use core_proto::JoinCode;
    let srv = spawn_tls().await;
    let out = invite_cmd(
        &srv.state.cfg.data_dir,
        "self-signed",
        &["--expires", "30m"],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).unwrap();
    let code = JoinCode::parse(text.lines().nth(1).unwrap()).unwrap();
    assert_eq!(code.pin, Some(srv.pin));
    let (created, expires): (i64, i64) =
        sqlx::query_as("SELECT created_at, expires_at FROM invites")
            .fetch_one(&srv.state.db)
            .await
            .unwrap();
    assert_eq!(expires - created, 30 * 60 * 1000);
}

#[tokio::test]
async fn invite_prints_short_code() {
    let srv = spawn().await;
    let text = ok_stdout(&admin(&srv.state.cfg.data_dir, &["invite"]));
    let line = text.trim_end();
    assert_eq!(text.lines().count(), 1, "{text:?}");
    assert!(
        line.len() == 23
            && line.starts_with("TLX-")
            && line.split('-').skip(1).all(|g| {
                g.len() == 4
                    && g.bytes()
                        .all(|b| b.is_ascii_digit() || b.is_ascii_uppercase())
            }),
        "{line}"
    );
}

#[tokio::test]
async fn setup_code_command_after_account_fails() {
    let srv = spawn().await;
    let data = &srv.state.cfg.data_dir;
    // Before any account it prints the banner, the same code each time.
    let first = ok_stdout(&admin(data, &["setup-code"]));
    assert!(first.contains("Setup code: TLX-"), "{first}");
    assert!(
        first.contains("then enter this\ncode and this computer's address."),
        "{first}"
    );
    assert_eq!(first, ok_stdout(&admin(data, &["setup-code"])));
    // A database that cannot be opened is a plain failure (status 1), not "has an account".
    let empty = TempDir::new().unwrap();
    assert_eq!(admin(empty.path(), &["setup-code"]).status.code(), Some(1));
    register(&srv, "alice").await;
    let out = admin(data, &["setup-code"]);
    // The installers print "Server updated." on exactly this status.
    assert_eq!(out.status.code(), Some(tether_server::cli::HAS_ACCOUNT_EXIT));
    assert_eq!(tether_server::cli::HAS_ACCOUNT_EXIT, 3);
    fails_with(
        &out,
        "this server already has an account; use `termlyx-server invite` for more people",
    );
}

#[tokio::test]
async fn setup_code_gone_after_first_account() {
    let srv = spawn().await;
    let db = &srv.state.db;
    assert!(
        tether_server::setup::ensure(db, now_ms())
            .await
            .unwrap()
            .is_some()
    );
    register(&srv, "alice").await;
    assert!(
        tether_server::setup::ensure(db, now_ms())
            .await
            .unwrap()
            .is_none()
    );
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM setup_code")
        .fetch_one(db)
        .await
        .unwrap();
    assert_eq!(n, 0);
    let open: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM invites WHERE used_at IS NULL")
        .fetch_one(db)
        .await
        .unwrap();
    assert_eq!(open, 0);
}

#[tokio::test]
async fn setup_code_dies_when_first_account_uses_other_invite() {
    let srv = spawn().await;
    let db = &srv.state.db;
    let code = tether_server::setup::ensure(db, now_ms())
        .await
        .unwrap()
        .unwrap();
    register(&srv, "alice").await; // a different full-token invite
    let req = register_req(
        &core_proto::JoinCode {
            kind: core_proto::CodeKind::Invite,
            url: "https://tether.example.com".into(),
            pin: None,
            token: code.token(),
        }
        .encode(),
        "bob",
        2,
        1,
    );
    assert_err(
        &srv.post_json("/v1/register", &req).await,
        403,
        core_proto::ErrorCode::InviteInvalid,
    );
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM setup_code")
        .fetch_one(db)
        .await
        .unwrap();
    assert_eq!(n, 0);
}

#[test]
fn invite_rejects_bad_durations_and_prints_nothing() {
    let dir = TempDir::new().unwrap();
    for bad in ["5x", "0d", "", "d", "-1h", "99999999999999999999d", "1.5h"] {
        let out = invite_cmd(dir.path(), "off", &["--expires", bad]);
        assert!(!out.status.success(), "{bad}");
        assert!(out.stdout.is_empty(), "{bad}");
    }
}

#[tokio::test]
async fn cli_link_code_adds_device() {
    use core_proto::{CodeKind, JoinCode};
    let srv = spawn().await;
    let acct = register(&srv, "lena").await;
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_termlyx-server"))
            .arg("link")
            .args(args)
            .env_clear()
            .env("TETHER_DATA", &srv.state.cfg.data_dir)
            .env("TETHER_TLS", "off")
            .env("TETHER_BIND", "127.0.0.1:8443")
            .env("TETHER_URL", "https://tether.example.com")
            .output()
            .unwrap()
    };
    let out = run(&["Lena"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).unwrap();
    assert_eq!(text.trim_end().lines().count(), 1, "{text:?}");
    let code = JoinCode::parse(text.trim()).unwrap();
    assert_eq!(code.kind, CodeKind::Link);

    // It adds a device with the password, once, and the device gets a secret.
    let res = login_link(&srv, "lena", &acct.auth_key, code.token.to_vec(), "Rescued").await;
    assert_eq!(res.status, 200, "{}", String::from_utf8_lossy(&res.body));
    let t: core_proto::TokenResp = res.json();
    assert_ne!(t.device_id, acct.tokens.device_id);
    assert!(!t.device_secret.is_empty());
    let res = login_link(&srv, "lena", &acct.auth_key, code.token.to_vec(), "Twice").await;
    assert_eq!(res.status, 403);

    // Unknown accounts are an error and print nothing.
    let out = run(&["nobody"]);
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
}

// ------------------------------------------------------- users / devices

/// Run `tether-server ARGS...` the way `docker compose exec` does: only the
/// `TETHER_*` environment, pointing at `data`.
fn admin(data: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_termlyx-server"))
        .args(args)
        .env_clear()
        .env("TETHER_DATA", data)
        .env("TETHER_TLS", "off")
        .env("TETHER_BIND", "127.0.0.1:8443")
        .output()
        .unwrap()
}

fn ok_stdout(out: &std::process::Output) -> String {
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout.clone()).unwrap()
}

/// A failed command: non-zero exit, a message on stderr, nothing on stdout.
fn fails_with(out: &std::process::Output, needle: &str) {
    assert!(!out.status.success(), "should have failed");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains(needle), "stderr {err:?} should say {needle:?}");
    assert!(out.stdout.is_empty());
}

#[tokio::test]
async fn users_list_prints_a_table_and_no_secrets() {
    let srv = spawn().await;
    let alice = register_with(&srv, "alice", 1, 1).await;
    add_device(&srv, &alice, "Laptop").await;
    let bob = register_with(&srv, "bob", 2, 1).await;
    let data = &srv.state.cfg.data_dir;

    let text = ok_stdout(&admin(data, &["users", "list"]));
    let lines: Vec<&str> = text.lines().collect();
    let cols = |l: &str| l.split_whitespace().map(String::from).collect::<Vec<_>>();
    let header = cols(lines[0]).join(" ");
    for h in ["username", "created", "disabled", "devices"] {
        assert!(header.contains(h), "{header}");
    }
    let alice_row = lines.iter().find(|l| l.starts_with("alice")).unwrap();
    let bob_row = lines.iter().find(|l| l.starts_with("bob")).unwrap();
    assert_eq!(cols(alice_row).last().unwrap(), "2", "{alice_row}");
    assert_eq!(cols(bob_row).last().unwrap(), "1", "{bob_row}");
    assert!(alice_row.contains(" no "), "{alice_row}");
    assert_eq!(lines.len(), 3, "{text}");

    // Disabling shows up; the list never prints verifiers, tokens or keys.
    ok_stdout(&admin(data, &["users", "disable", "bob"]));
    let text = ok_stdout(&admin(data, &["users", "list"]));
    let bob_row = text.lines().find(|l| l.starts_with("bob")).unwrap();
    assert!(bob_row.contains("yes"), "{bob_row}");
    let verifier: Vec<u8> =
        sqlx::query_scalar("SELECT verifier FROM accounts WHERE username = 'bob'")
            .fetch_one(&srv.state.db)
            .await
            .unwrap();
    Needles::default()
        .secret("access token", &bob.tokens.access_token)
        .secret("refresh token", &bob.tokens.refresh_token)
        .secret("device secret", &bob.tokens.device_secret)
        .assert_absent("users list", text.as_bytes());
    assert!(!contains(
        text.as_bytes(),
        data_encoding::HEXLOWER.encode(&verifier).as_bytes()
    ));
    assert!(!contains(
        text.as_bytes(),
        data_encoding::BASE64.encode(&verifier).as_bytes()
    ));
}

#[tokio::test]
async fn unknown_user_is_a_clear_error() {
    let srv = spawn().await;
    register(&srv, "alice").await;
    let data = &srv.state.cfg.data_dir;
    for args in [
        vec!["users", "disable", "nobody"],
        vec!["users", "enable", "nobody"],
        vec!["users", "reset-schema", "nobody"],
        vec!["devices", "list", "nobody"],
    ] {
        fails_with(&admin(data, &args), "no account named nobody");
    }
    fails_with(
        &admin(
            data,
            &["devices", "revoke", "00000000-0000-0000-0000-000000000000"],
        ),
        "no device",
    );
}

#[test]
fn admin_commands_do_not_invent_a_database() {
    let dir = TempDir::new().unwrap();
    for args in [
        vec!["users", "list"],
        vec!["devices", "list", "alice"],
        vec!["users", "disable", "alice"],
    ] {
        fails_with(&admin(dir.path(), &args), "no database");
    }
    assert!(!dir.path().join("tether.db").exists());
}

#[tokio::test]
async fn disabled_user_cannot_login() {
    use core_proto::{ErrorCode, RefreshReq};
    let srv = spawn().await;
    let a = register(&srv, "ruth").await;
    let other = register_with(&srv, "sam", 2, 1).await;
    let data = srv.state.cfg.data_dir.clone();

    let out = ok_stdout(&admin(&data, &["users", "disable", "Ruth"]));
    assert!(out.contains("ruth"), "{out}");
    assert_err(
        &login_device(&srv, &a, &a.tokens.device_id, &a.auth_key).await,
        403,
        ErrorCode::Forbidden,
    );
    assert_err(
        &srv.post_json(
            "/v1/refresh",
            &RefreshReq {
                refresh_token: a.tokens.refresh_token.clone(),
            },
        )
        .await,
        403,
        ErrorCode::Forbidden,
    );
    assert_err(
        &get_auth(&srv, "/v1/devices", &a.tokens.access_token).await,
        403,
        ErrorCode::Forbidden,
    );
    // Only that account.
    assert_eq!(
        get_auth(&srv, "/v1/devices", &other.tokens.access_token)
            .await
            .status,
        200
    );

    // Enabling restores everything, and doing it twice is harmless.
    ok_stdout(&admin(&data, &["users", "enable", "ruth"]));
    ok_stdout(&admin(&data, &["users", "enable", "ruth"]));
    let res = login_device(&srv, &a, &a.tokens.device_id, &a.auth_key).await;
    assert_eq!(res.status, 200);
    let fresh: core_proto::TokenResp = res.json();
    assert_eq!(
        get_auth(&srv, "/v1/devices", &fresh.access_token)
            .await
            .status,
        200
    );
}

#[tokio::test]
async fn reset_schema_lets_outdated_clients_write_again() {
    use core_proto::ErrorCode;
    let srv = spawn().await;
    let a = register(&srv, "ivy").await;
    let tok = &a.tokens.access_token;
    let newer = |schema: &'static str| {
        let tok = tok.clone();
        let srv = &srv;
        async move {
            srv.request(
                "GET",
                "/v1/account",
                &[
                    ("Authorization", &format!("Bearer {tok}")),
                    ("X-Tether-Schema", schema),
                ],
                None,
            )
            .await
        }
    };
    let write = || async {
        push(
            &srv,
            tok,
            vec![item(row(&new_id(), "host", now_ms(), &[1, 2, 3]), None)],
        )
        .await
    };
    let max_schema = || async {
        sqlx::query_scalar::<_, i64>("SELECT max_schema FROM accounts WHERE username = 'ivy'")
            .fetch_one(&srv.state.db)
            .await
            .unwrap()
    };

    // A newer client was seen; the old one can no longer write.
    assert_eq!(newer("3").await.status, 200);
    assert_eq!(max_schema().await, 3);
    assert_err(&write().await, 400, ErrorCode::UpdateClient);

    // The admin resets the mark; the old client writes again.
    let out = ok_stdout(&admin(
        &srv.state.cfg.data_dir,
        &["users", "reset-schema", "ivy"],
    ));
    assert!(out.contains("ivy"), "{out}");
    assert_eq!(max_schema().await, 0);
    assert_eq!(write().await.status, 200);
    // (Its own request recorded its schema again, as designed.)
    assert_eq!(max_schema().await, 1);
    // Another account is untouched.
    let b = register_with(&srv, "joe", 2, 1).await;
    assert_eq!(
        srv.request(
            "GET",
            "/v1/account",
            &[
                (
                    "Authorization",
                    &format!("Bearer {}", b.tokens.access_token)
                ),
                ("X-Tether-Schema", "3"),
            ],
            None,
        )
        .await
        .status,
        200
    );
    ok_stdout(&admin(
        &srv.state.cfg.data_dir,
        &["users", "reset-schema", "ivy"],
    ));
    let joe: i64 = sqlx::query_scalar("SELECT max_schema FROM accounts WHERE username = 'joe'")
        .fetch_one(&srv.state.db)
        .await
        .unwrap();
    assert_eq!(joe, 3);
}

#[tokio::test]
async fn devices_list_shows_devices_and_cleans_names() {
    let srv = spawn().await;
    let a = register(&srv, "alice").await;
    let second = add_device(&srv, &a, "Laptop").await;
    // The API refuses control characters in names; a row from elsewhere (an
    // older version, a hand edit) must still not reach the terminal raw.
    sqlx::query("UPDATE devices SET name = ? WHERE id = ?")
        .bind("Lap\u{1b}[31mtop\ntwo")
        .bind(&second.device_id)
        .execute(&srv.state.db)
        .await
        .unwrap();
    register_with(&srv, "bob", 2, 1).await;
    let data = &srv.state.cfg.data_dir;

    ok_stdout(&admin(data, &["devices", "revoke", &second.device_id]));
    let text = ok_stdout(&admin(data, &["devices", "list", "ALICE"]));
    let lines: Vec<&str> = text.lines().collect();
    for h in ["id", "name", "platform", "last seen", "revoked"] {
        assert!(lines[0].contains(h), "{}", lines[0]);
    }
    assert_eq!(
        lines.len(),
        3,
        "one line per device, no raw newlines: {text:?}"
    );
    assert!(!text.contains('\u{1b}'), "terminal escapes are not printed");
    let first = lines
        .iter()
        .find(|l| l.contains(&a.tokens.device_id))
        .unwrap();
    let rev = lines
        .iter()
        .find(|l| l.contains(&second.device_id))
        .unwrap();
    assert!(first.contains("First") && first.contains("macos") && first.trim_end().ends_with("no"));
    assert!(rev.trim_end().ends_with("yes"), "{rev}");
    // Never another account's devices.
    assert!(!text.contains("bob"));
}

#[tokio::test]
async fn devices_revoke_from_cli() {
    use core_proto::ErrorCode;
    let srv = spawn().await;
    let a = register(&srv, "alice").await;
    let second = add_device(&srv, &a, "Laptop").await;
    let data = srv.state.cfg.data_dir.clone();

    let out = ok_stdout(&admin(&data, &["devices", "revoke", &second.device_id]));
    assert!(
        out.contains(&second.device_id) && out.contains("alice"),
        "{out}"
    );
    // The revoked device is cut off; the other one is not.
    assert_err(
        &get_auth(&srv, "/v1/devices", &second.access_token).await,
        401,
        ErrorCode::Revoked,
    );
    assert_eq!(
        get_auth(&srv, "/v1/devices", &a.tokens.access_token)
            .await
            .status,
        200
    );
    // Again is fine and says so; a malformed id is an error.
    let again = ok_stdout(&admin(&data, &["devices", "revoke", &second.device_id]));
    assert!(again.contains("already"), "{again}");
    fails_with(
        &admin(&data, &["devices", "revoke", "not-a-device"]),
        "no device",
    );
}

fn health_cmd(
    data: &std::path::Path,
    tls: &str,
    bind: std::net::SocketAddr,
) -> std::process::Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_termlyx-server"));
    cmd.arg("health").env_clear();
    // Winsock fails to initialise (os error 10106) without SystemRoot.
    #[cfg(windows)]
    cmd.env(
        "SystemRoot",
        std::env::var_os("SystemRoot").unwrap_or_default(),
    );
    cmd.env("TETHER_DATA", data)
        .env("TETHER_TLS", tls)
        .env("TETHER_BIND", bind.to_string())
        .output()
        .unwrap()
}

// Multi-threaded: the child process blocks this thread while the in-process
// server must answer it.
#[tokio::test(flavor = "multi_thread")]
async fn health_pins_the_local_certificate() {
    let srv = spawn_tls().await;
    let data = srv.state.cfg.data_dir.clone();
    let out = health_cmd(&data, "self-signed", srv.addr());
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "ok");

    // A different certificate on that port (another server's key) is refused:
    // there is no fallback to WebPKI or to "trust anything".
    let other = spawn_tls().await;
    let out = health_cmd(&other.state.cfg.data_dir, "self-signed", srv.addr());
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
}

// Multi-threaded: the child process blocks this thread while the in-process
// server must answer it.
#[tokio::test(flavor = "multi_thread")]
async fn health_speaks_plain_http_when_tls_is_off() {
    let srv = spawn().await;
    let dir = TempDir::new().unwrap();
    let out = health_cmd(dir.path(), "off", srv.addr());
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn health_fails_when_nothing_listens() {
    let dir = TempDir::new().unwrap();
    // Reserve a port, then free it so connecting is refused.
    let addr = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap()
    };
    assert!(!health_cmd(dir.path(), "off", addr).status.success());
    // Self-signed with no certificate yet (server never started): also unhealthy,
    // and the probe must not create TLS material.
    assert!(!health_cmd(dir.path(), "self-signed", addr).status.success());
    assert!(!dir.path().join("tls").exists());
}

// ------------------------------------------------------------ users delete

/// Rows that belong to one account, per table.
#[derive(Debug, PartialEq, Eq)]
struct Footprint {
    accounts: i64,
    devices: i64,
    refresh_tokens: i64,
    access_tokens: i64,
    link_tokens: i64,
    records: i64,
    vault_seq: i64,
}

async fn footprint(db: &sqlx::SqlitePool, account_id: &str, vault_id: &str) -> Footprint {
    let n = |sql: &'static str, key: String| async move {
        sqlx::query_scalar::<_, i64>(sql)
            .bind(key)
            .fetch_one(db)
            .await
            .unwrap()
    };
    let a = account_id.to_owned();
    let v = vault_id.to_owned();
    Footprint {
        accounts: n("SELECT count(*) FROM accounts WHERE id = ?", a.clone()).await,
        devices: n(
            "SELECT count(*) FROM devices WHERE account_id = ?",
            a.clone(),
        )
        .await,
        refresh_tokens: n(
            "SELECT count(*) FROM refresh_tokens WHERE device_id IN \
             (SELECT id FROM devices WHERE account_id = ?)",
            a.clone(),
        )
        .await,
        access_tokens: n(
            "SELECT count(*) FROM access_tokens WHERE account_id = ?",
            a.clone(),
        )
        .await,
        link_tokens: n("SELECT count(*) FROM link_tokens WHERE account_id = ?", a).await,
        records: n("SELECT count(*) FROM records WHERE vault_id = ?", v.clone()).await,
        vault_seq: n("SELECT count(*) FROM vault_seq WHERE vault_id = ?", v).await,
    }
}

async fn id_of(db: &sqlx::SqlitePool, username: &str) -> String {
    sqlx::query_scalar("SELECT id FROM accounts WHERE username = ?")
        .bind(username)
        .fetch_one(db)
        .await
        .unwrap()
}

/// An account with something in every per-account table: two devices (one
/// revoked), records and a tombstone, an unused add-device code and the
/// limited recovery token.
async fn busy_account(srv: &TestServer, username: &str, k: u8) -> (Account, String) {
    let a = register_with(srv, username, k, 1).await;
    let second = add_device(srv, &a, "Laptop").await;
    let third = add_device(srv, &a, "Old phone").await;
    ok_stdout(&admin(
        &srv.state.cfg.data_dir,
        &["devices", "revoke", &third.device_id],
    ));
    let gone = new_id();
    push_ok(
        srv,
        &a.tokens.access_token,
        vec![
            item(row(&new_id(), "host", now_ms(), &[1, 2, 3]), None),
            item(row(&gone, "host", now_ms(), &[4, 5, 6]), None),
        ],
    )
    .await;
    push_ok(
        srv,
        &second.access_token,
        vec![item(tombstone(&gone, "host", now_ms() + 1), Some(1))],
    )
    .await;
    make_link(srv, &a.tokens.access_token).await;
    recover_token(srv, &a).await;
    let id = id_of(&srv.state.db, username).await;
    (a, id)
}

#[tokio::test]
async fn users_delete_refuses_an_enabled_account() {
    let srv = spawn().await;
    let (a, id) = busy_account(&srv, "zed", 1).await;
    let before = footprint(&srv.state.db, &id, &a.vault_id).await;
    let data = &srv.state.cfg.data_dir;

    for args in [
        vec!["users", "delete", "zed", "--yes"],
        vec!["users", "delete", "zed"],
    ] {
        fails_with(&admin(data, &args), "users disable zed");
    }
    assert_eq!(footprint(&srv.state.db, &id, &a.vault_id).await, before);
    assert_eq!(
        get_auth(&srv, "/v1/devices", &a.tokens.access_token)
            .await
            .status,
        200
    );
}

#[tokio::test]
async fn users_delete_without_yes_only_says_what_it_would_delete() {
    let srv = spawn().await;
    let (a, id) = busy_account(&srv, "zed", 1).await;
    let data = &srv.state.cfg.data_dir;
    ok_stdout(&admin(data, &["users", "disable", "zed"]));
    let before = footprint(&srv.state.db, &id, &a.vault_id).await;
    assert!(before.records == 2 && before.devices == 3, "{before:?}");

    let out = admin(data, &["users", "delete", "ZED"]);
    fails_with(&out, "--yes");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("zed") && err.contains("3 devices") && err.contains("2 records"),
        "{err}"
    );
    Needles::default()
        .secret("access token", &a.tokens.access_token)
        .secret("refresh token", &a.tokens.refresh_token)
        .secret("device secret", &a.tokens.device_secret)
        .assert_absent("users delete preview", &out.stderr);
    assert_eq!(footprint(&srv.state.db, &id, &a.vault_id).await, before);
}

#[tokio::test]
async fn users_delete_removes_every_row_of_the_account_and_nothing_else() {
    use core_proto::{ErrorCode, RefreshReq};
    let srv = spawn().await;
    let (zed, zed_id) = busy_account(&srv, "zed", 1).await;
    let (bob, bob_id) = busy_account(&srv, "bob", 2).await;
    srv.make_invite().await; // an unused invite: shared, must survive
    let data = srv.state.cfg.data_dir.clone();
    let bob_before = footprint(&srv.state.db, &bob_id, &bob.vault_id).await;
    let count = |sql: &'static str| {
        let db = srv.state.db.clone();
        async move {
            sqlx::query_scalar::<_, i64>(sql)
                .fetch_one(&db)
                .await
                .unwrap()
        }
    };
    let invites = count("SELECT count(*) FROM invites").await;
    let meta = count("SELECT count(*) FROM meta").await;

    ok_stdout(&admin(&data, &["users", "disable", "zed"]));
    let out = ok_stdout(&admin(&data, &["users", "delete", "Zed", "--yes"]));
    assert_eq!(out, "deleted zed: 3 devices, 2 records\n");
    Needles::default()
        .secret("access token", &zed.tokens.access_token)
        .secret("refresh token", &zed.tokens.refresh_token)
        .secret("device secret", &zed.tokens.device_secret)
        .assert_absent("users delete", out.as_bytes());

    assert_eq!(
        footprint(&srv.state.db, &zed_id, &zed.vault_id).await,
        Footprint {
            accounts: 0,
            devices: 0,
            refresh_tokens: 0,
            access_tokens: 0,
            link_tokens: 0,
            records: 0,
            vault_seq: 0,
        }
    );
    // Nothing points at the deleted account or its devices any more.
    let orphans = count(
        "SELECT (SELECT count(*) FROM devices WHERE account_id NOT IN (SELECT id FROM accounts)) \
              + (SELECT count(*) FROM access_tokens WHERE account_id NOT IN (SELECT id FROM accounts)) \
              + (SELECT count(*) FROM link_tokens WHERE account_id NOT IN (SELECT id FROM accounts)) \
              + (SELECT count(*) FROM refresh_tokens WHERE device_id NOT IN (SELECT id FROM devices)) \
              + (SELECT count(*) FROM records WHERE vault_id NOT IN (SELECT vault_id FROM accounts)) \
              + (SELECT count(*) FROM vault_seq WHERE vault_id NOT IN (SELECT vault_id FROM accounts))",
    )
    .await;
    assert_eq!(orphans, 0);
    // Other accounts and shared rows are untouched.
    assert_eq!(
        footprint(&srv.state.db, &bob_id, &bob.vault_id).await,
        bob_before
    );
    assert_eq!(count("SELECT count(*) FROM invites").await, invites);
    assert_eq!(count("SELECT count(*) FROM meta").await, meta);

    // The running server no longer knows the deleted account's sessions.
    assert_err(
        &get_auth(&srv, "/v1/devices", &zed.tokens.access_token).await,
        401,
        ErrorCode::Unauthorized,
    );
    assert_err(
        &srv.post_json(
            "/v1/refresh",
            &RefreshReq {
                refresh_token: zed.tokens.refresh_token.clone(),
            },
        )
        .await,
        401,
        ErrorCode::Unauthorized,
    );
    assert_eq!(
        login_device(&srv, &zed, &zed.tokens.device_id, &zed.auth_key)
            .await
            .status,
        401
    );
    assert_eq!(
        get_auth(&srv, "/v1/devices", &bob.tokens.access_token)
            .await
            .status,
        200
    );
    // Gone is gone: a second delete is an unknown account.
    fails_with(
        &admin(&data, &["users", "delete", "zed", "--yes"]),
        "no account named zed",
    );

    // The name is free again for a new invite.
    let fresh = register_with(&srv, "zed", 9, 1).await;
    assert_eq!(
        get_auth(&srv, "/v1/devices", &fresh.tokens.access_token)
            .await
            .status,
        200
    );
    let users = ok_stdout(&admin(&data, &["users", "list"]));
    assert_eq!(users.lines().filter(|l| l.starts_with("zed")).count(), 1);
}

#[tokio::test]
async fn users_delete_is_all_or_nothing() {
    let srv = spawn().await;
    let (a, id) = busy_account(&srv, "zed", 1).await;
    let data = srv.state.cfg.data_dir.clone();
    ok_stdout(&admin(&data, &["users", "disable", "zed"]));
    let before = footprint(&srv.state.db, &id, &a.vault_id).await;
    // The last step fails; every earlier delete must be rolled back.
    sqlx::query(
        "CREATE TRIGGER no_account_delete BEFORE DELETE ON accounts \
         BEGIN SELECT RAISE(ABORT, 'refused by test'); END",
    )
    .execute(&srv.state.db)
    .await
    .unwrap();

    fails_with(
        &admin(&data, &["users", "delete", "zed", "--yes"]),
        "refused by test",
    );
    assert_eq!(footprint(&srv.state.db, &id, &a.vault_id).await, before);
}

#[tokio::test]
async fn users_delete_of_an_unknown_account_is_an_error() {
    let srv = spawn().await;
    register(&srv, "alice").await;
    fails_with(
        &admin(
            &srv.state.cfg.data_dir,
            &["users", "delete", "nobody", "--yes"],
        ),
        "no account named nobody",
    );
}
