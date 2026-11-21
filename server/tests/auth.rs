//! Invites, registration, prelogin, login, recover-login, sessions, link
//! codes, node ids, rate limits and lockout.

mod common;

use std::sync::{Arc, Mutex, OnceLock};

use common::*;
use core_proto::{
    CodeKind, ErrorCode, JoinCode, KdfWire, PreloginReq, PreloginResp, RecoverReq, RefreshReq,
    TokenResp,
};
use serde_json::Value;

const MIN: i64 = 60_000;

fn kdf_of(res: &Response) -> KdfWire {
    res.json::<PreloginResp>().kdf
}

async fn prelogin(srv: &TestServer, username: &str) -> Response {
    srv.post_json(
        "/v1/prelogin",
        &PreloginReq {
            username: username.into(),
        },
    )
    .await
}

async fn db_i64(srv: &TestServer, sql: &'static str, bind: &str) -> i64 {
    sqlx::query_scalar(sql)
        .bind(bind.to_owned())
        .fetch_one(&srv.state.db)
        .await
        .unwrap()
}

#[tokio::test]
async fn register_with_invite_then_login() {
    let srv = spawn().await;
    let acct = register_with(&srv, "Alice", 1, 5).await;
    let t = &acct.tokens;
    // The first device keeps its node hint; tokens are 32 random bytes in
    // base64url without padding; the lifetime is fifteen minutes.
    assert_eq!(t.node, 5);
    assert_eq!(t.expires_in, 900);
    assert_eq!(t.epoch, srv.state.epoch);
    assert_eq!(token_bytes(&t.access_token).unwrap().len(), 32);
    assert_eq!(token_bytes(&t.refresh_token).unwrap().len(), 32);
    assert_ne!(t.access_token, t.refresh_token);
    // The access token works.
    assert_eq!(
        get_auth(&srv, "/v1/devices", &t.access_token).await.status,
        200
    );

    // Prelogin serves the registered parameters (usernames are case-insensitive).
    let res = prelogin(&srv, "  alice ").await;
    assert_eq!(res.status, 200);
    let kdf = kdf_of(&res);
    assert_eq!(
        (kdf.m_kib, kdf.t, kdf.p, kdf.salt),
        (65536, 3, 4, vec![7; 16])
    );

    // The same device logs in again with the AuthKey.
    let res = login_device(&srv, &acct, &t.device_id, &acct.auth_key).await;
    assert_eq!(res.status, 200, "{}", String::from_utf8_lossy(&res.body));
    let again: TokenResp = res.json();
    assert_eq!(again.device_id, t.device_id);
    assert_eq!(again.node, 5);
    assert_ne!(again.access_token, t.access_token);
    assert_eq!(
        get_auth(&srv, "/v1/devices", &again.access_token)
            .await
            .status,
        200
    );

    // Nothing sensitive is stored in the clear: the verifier is a hash.
    let (verifier, salt): (Vec<u8>, Vec<u8>) =
        sqlx::query_as("SELECT verifier, verifier_salt FROM accounts WHERE username = 'alice'")
            .fetch_one(&srv.state.db)
            .await
            .unwrap();
    assert_ne!(verifier, acct.auth_key);
    assert_eq!((verifier.len(), salt.len()), (32, 16));
}

#[tokio::test]
async fn invite_is_single_use() {
    let srv = spawn().await;
    let invite = srv.make_invite().await;
    let first = register_req(&invite, "one", 1, 1);
    assert_eq!(srv.post_json("/v1/register", &first).await.status, 200);
    let second = register_req(&invite, "two", 2, 1);
    assert_err(
        &srv.post_json("/v1/register", &second).await,
        403,
        ErrorCode::InviteInvalid,
    );
    // The failed attempt created nothing.
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM accounts")
        .fetch_one(&srv.state.db)
        .await
        .unwrap();
    assert_eq!(n, 1);

    // A token that was never issued, and one of the wrong length.
    let mut bogus = register_req(&invite, "three", 3, 1);
    bogus.invite_token = vec![9; 32];
    assert_err(
        &srv.post_json("/v1/register", &bogus).await,
        403,
        ErrorCode::InviteInvalid,
    );
    bogus.invite_token = vec![9; 5];
    assert_err(
        &srv.post_json("/v1/register", &bogus).await,
        403,
        ErrorCode::InviteInvalid,
    );
}

#[tokio::test]
async fn expired_invite_is_rejected() {
    let srv = spawn().await;
    let invite = srv.make_invite().await;
    srv.set_clock(now_ms() + 7 * 24 * 60 * MIN + 1000);
    assert_err(
        &srv.post_json("/v1/register", &register_req(&invite, "late", 1, 1))
            .await,
        403,
        ErrorCode::InviteInvalid,
    );
}

#[tokio::test]
async fn username_taken() {
    let srv = spawn().await;
    register(&srv, "alice").await;
    let invite = srv.make_invite().await;
    // Case-insensitive, and surrounding spaces do not make it different.
    let clash = register_req(&invite, " ALICE ", 2, 1);
    assert_err(
        &srv.post_json("/v1/register", &clash).await,
        409,
        ErrorCode::UsernameTaken,
    );
    // The invite was not consumed by the failed attempt.
    let ok = register_req(&invite, "bob", 3, 1);
    assert_eq!(srv.post_json("/v1/register", &ok).await.status, 200);
}

#[tokio::test]
async fn register_validates_input_without_spending_the_invite() {
    let srv = spawn().await;
    let invite = srv.make_invite().await;
    for bad in [
        "ab",
        "has space",
        "Üser",
        &"x".repeat(65),
        "semi;colon",
        "a/b",
    ] {
        let req = register_req(&invite, bad, 1, 1);
        assert_err(
            &srv.post_json("/v1/register", &req).await,
            400,
            ErrorCode::BadRequest,
        );
    }
    let mut req = register_req(&invite, "valid", 1, 1);
    req.auth_key = vec![1; 31];
    assert_err(
        &srv.post_json("/v1/register", &req).await,
        400,
        ErrorCode::BadRequest,
    );
    let mut req = register_req(&invite, "valid", 1, 1);
    req.recovery_auth_key = vec![];
    assert_err(
        &srv.post_json("/v1/register", &req).await,
        400,
        ErrorCode::BadRequest,
    );
    let mut req = register_req(&invite, "valid", 1, 1);
    req.material.account_public = vec![1; 31];
    assert_err(
        &srv.post_json("/v1/register", &req).await,
        400,
        ErrorCode::BadRequest,
    );
    let mut req = register_req(&invite, "valid", 1, 1);
    req.material.secret_by_kek = vec![2; 72];
    assert_err(
        &srv.post_json("/v1/register", &req).await,
        400,
        ErrorCode::BadRequest,
    );
    let mut req = register_req(&invite, "valid", 1, 1);
    req.material.secret_by_recovery = vec![];
    assert_err(
        &srv.post_json("/v1/register", &req).await,
        400,
        ErrorCode::BadRequest,
    );
    let mut req = register_req(&invite, "valid", 1, 1);
    req.material.sealed_vault_key = vec![4; 80];
    assert_err(
        &srv.post_json("/v1/register", &req).await,
        400,
        ErrorCode::BadRequest,
    );
    let mut req = register_req(&invite, "valid", 1, 1);
    req.material.vault_id = "not-a-uuid".into();
    assert_err(
        &srv.post_json("/v1/register", &req).await,
        400,
        ErrorCode::BadRequest,
    );
    let mut req = register_req(&invite, "valid", 1, 1);
    req.material.kdf.salt = vec![1; 15];
    assert_err(
        &srv.post_json("/v1/register", &req).await,
        400,
        ErrorCode::BadRequest,
    );
    let mut req = register_req(&invite, "valid", 1, 1);
    req.material.kdf.m_kib = u32::MAX;
    assert_err(
        &srv.post_json("/v1/register", &req).await,
        400,
        ErrorCode::BadRequest,
    );
    let mut req = register_req(&invite, "valid", 1, 1);
    req.device.name = "  ".into();
    assert_err(
        &srv.post_json("/v1/register", &req).await,
        400,
        ErrorCode::BadRequest,
    );

    // None of that used the invite up.
    let ok = register_req(&invite, "valid", 1, 1);
    assert_eq!(srv.post_json("/v1/register", &ok).await.status, 200);
    // The stored username is lowercase; the account holds the given material.
    let stored: String = sqlx::query_scalar("SELECT username FROM accounts")
        .fetch_one(&srv.state.db)
        .await
        .unwrap();
    assert_eq!(stored, "valid");
}

#[tokio::test]
async fn prelogin_unknown_user_is_indistinguishable() {
    let srv = spawn().await;
    register(&srv, "known").await;

    let real = prelogin(&srv, "known").await;
    let ghost = prelogin(&srv, "ghost").await;
    let ghost_again = prelogin(&srv, "ghost").await;
    let other_ghost = prelogin(&srv, "phantom").await;
    for r in [&real, &ghost, &other_ghost] {
        assert_eq!(r.status, 200);
    }

    // Same JSON shape (same keys at every level), production default params.
    fn shape(v: &Value) -> Value {
        match v {
            Value::Object(m) => {
                Value::Object(m.iter().map(|(k, v)| (k.clone(), shape(v))).collect())
            }
            Value::Number(_) => Value::from(0),
            Value::String(_) => Value::from(""),
            other => other.clone(),
        }
    }
    let rv: Value = real.json();
    let gv: Value = ghost.json();
    assert_eq!(shape(&rv), shape(&gv));
    let g = kdf_of(&ghost);
    assert_eq!((g.m_kib, g.t, g.p, g.salt.len()), (65536, 3, 4, 16));

    // Stable per name, different between names, and not a constant.
    assert_eq!(kdf_of(&ghost).salt, kdf_of(&ghost_again).salt);
    assert_ne!(kdf_of(&ghost).salt, kdf_of(&other_ghost).salt);
    // Case and padding do not matter.
    let loud = prelogin(&srv, " GHOST").await;
    assert_eq!(kdf_of(&loud).salt, kdf_of(&ghost).salt);
}

#[tokio::test]
async fn wrong_auth_key_is_unauthorized_and_counts_toward_lockout() {
    let srv = spawn().await;
    let acct = register(&srv, "carol").await;
    let dev = &acct.tokens.device_id;

    let res = login_device(&srv, &acct, dev, &[9; 32]).await;
    assert_err(&res, 401, ErrorCode::Unauthorized);
    // An unknown user gets the very same answer.
    let ghost = Account {
        username: "nobody".into(),
        ..register_stub(&acct)
    };
    let res_ghost = login_device(&srv, &ghost, dev, &[9; 32]).await;
    assert_eq!(res.status, res_ghost.status);
    assert_eq!(res.body, res_ghost.body);

    let n = db_i64(
        &srv,
        "SELECT failed_logins FROM accounts WHERE username = ?",
        "carol",
    )
    .await;
    assert_eq!(n, 1);
    login_device(&srv, &acct, dev, &[8; 32]).await;
    assert_eq!(
        db_i64(
            &srv,
            "SELECT failed_logins FROM accounts WHERE username = ?",
            "carol"
        )
        .await,
        2
    );
    // Success resets the count ("consecutive" failures).
    assert_eq!(
        login_device(&srv, &acct, dev, &acct.auth_key).await.status,
        200
    );
    assert_eq!(
        db_i64(
            &srv,
            "SELECT failed_logins FROM accounts WHERE username = ?",
            "carol"
        )
        .await,
        0
    );
}

/// A throwaway `Account` value to log in as somebody else.
fn register_stub(a: &Account) -> Account {
    Account {
        username: a.username.clone(),
        auth_key: a.auth_key.clone(),
        recovery_key: a.recovery_key.clone(),
        vault_id: a.vault_id.clone(),
        tokens: a.tokens.clone(),
    }
}

#[tokio::test]
async fn lockout_after_ten_failures() {
    let srv = spawn().await;
    let acct = register(&srv, "dave").await;
    let dev = acct.tokens.device_id.clone();
    // Registering counted as one attempt; start with an empty window.
    let t0 = now_ms() + 2 * MIN;
    srv.set_clock(t0);

    for i in 0..10 {
        let res = login_device(&srv, &acct, &dev, &[9; 32]).await;
        assert_err(&res, 401, ErrorCode::Unauthorized);
        srv.set_clock(t0 + (i + 1) * 4_000);
    }
    // Let the per-minute window clear so only the lockout can answer now.
    srv.set_clock(t0 + 2 * MIN);
    // Even the right key is refused while locked.
    let res = login_device(&srv, &acct, &dev, &acct.auth_key).await;
    assert_err(&res, 429, ErrorCode::RateLimited);
    // Recovery login shares the lock.
    let rec = RecoverReq {
        username: "dave".into(),
        recovery_auth_key: acct.recovery_key.clone(),
        device: device("x"),
    };
    assert_err(
        &srv.post_json("/v1/recover-login", &rec).await,
        429,
        ErrorCode::RateLimited,
    );
    // It lasts fifteen minutes from the tenth failure.
    srv.set_clock(t0 + 40_000 + 14 * MIN);
    assert_err(
        &login_device(&srv, &acct, &dev, &acct.auth_key).await,
        429,
        ErrorCode::RateLimited,
    );
    srv.set_clock(t0 + 40_000 + 15 * MIN + 1);
    assert_eq!(
        login_device(&srv, &acct, &dev, &acct.auth_key).await.status,
        200
    );
    // The lock survives in the database (a restart does not clear it).
    for _ in 0..10 {
        login_device(&srv, &acct, &dev, &[9; 32]).await;
        srv.set_clock(srv.state.now_ms() + 7_000);
    }
    let srv = srv.restart().await;
    let locked: i64 =
        sqlx::query_scalar("SELECT locked_until FROM accounts WHERE username = 'dave'")
            .fetch_one(&srv.state.db)
            .await
            .unwrap();
    assert!(locked > srv.state.now_ms());
}

#[tokio::test]
async fn lockout_looks_the_same_for_unknown_users() {
    let srv = spawn().await;
    let acct = register(&srv, "erin").await;
    let ghost = Account {
        username: "ghost".into(),
        ..register_stub(&acct)
    };
    let t0 = now_ms();
    for i in 0..10 {
        srv.set_clock(t0 + i * 4_000);
        assert_err(
            &login_device(&srv, &ghost, &acct.tokens.device_id, &[9; 32]).await,
            401,
            ErrorCode::Unauthorized,
        );
    }
    srv.set_clock(t0 + 3 * MIN);
    assert_err(
        &login_device(&srv, &ghost, &acct.tokens.device_id, &[9; 32]).await,
        429,
        ErrorCode::RateLimited,
    );
}

#[tokio::test]
async fn rate_limits_per_ip_and_per_username() {
    let srv = spawn().await;
    let t0 = now_ms();
    srv.set_clock(t0);
    // Per username: ten a minute, then 429.
    for _ in 0..10 {
        assert_eq!(prelogin(&srv, "someone").await.status, 200);
    }
    assert_err(
        &prelogin(&srv, "someone").await,
        429,
        ErrorCode::RateLimited,
    );
    // Other names are unaffected until the IP's twenty are used up.
    for i in 0..10 {
        assert_eq!(prelogin(&srv, &format!("name{i}")).await.status, 200, "{i}");
    }
    assert_err(
        &prelogin(&srv, "fresh-name").await,
        429,
        ErrorCode::RateLimited,
    );
    // The window slides.
    srv.set_clock(t0 + MIN + 1);
    assert_eq!(prelogin(&srv, "someone").await.status, 200);
}

#[tokio::test]
async fn refresh_rotates_and_reuse_revokes_device() {
    let srv = spawn().await;
    let acct = register(&srv, "frank").await;
    let r1 = acct.tokens.refresh_token.clone();

    let res = srv
        .post_json(
            "/v1/refresh",
            &RefreshReq {
                refresh_token: r1.clone(),
            },
        )
        .await;
    assert_eq!(res.status, 200, "{}", String::from_utf8_lossy(&res.body));
    let t2: TokenResp = res.json();
    assert_ne!(t2.refresh_token, r1);
    assert_ne!(t2.access_token, acct.tokens.access_token);
    assert_eq!(
        (t2.device_id.as_str(), t2.node),
        (acct.tokens.device_id.as_str(), acct.tokens.node)
    );
    assert_eq!(
        get_auth(&srv, "/v1/devices", &t2.access_token).await.status,
        200
    );

    // The new refresh token rotates again.
    let t3: TokenResp = srv
        .post_json(
            "/v1/refresh",
            &RefreshReq {
                refresh_token: t2.refresh_token.clone(),
            },
        )
        .await
        .json();

    // Presenting a rotated-out token revokes the device: everything fails
    // with `revoked` afterwards, including the newest tokens.
    let reuse = srv
        .post_json("/v1/refresh", &RefreshReq { refresh_token: r1 })
        .await;
    assert_err(&reuse, 401, ErrorCode::Revoked);
    assert_err(
        &srv.post_json(
            "/v1/refresh",
            &RefreshReq {
                refresh_token: t3.refresh_token.clone(),
            },
        )
        .await,
        401,
        ErrorCode::Revoked,
    );
    assert_err(
        &get_auth(&srv, "/v1/devices", &t3.access_token).await,
        401,
        ErrorCode::Revoked,
    );
    assert_err(
        &get_auth(&srv, "/v1/devices", &acct.tokens.access_token).await,
        401,
        ErrorCode::Revoked,
    );
    // Logging in again as the revoked device does not help.
    assert_err(
        &login_device(&srv, &acct, &acct.tokens.device_id, &acct.auth_key).await,
        401,
        ErrorCode::Revoked,
    );
}

#[tokio::test]
async fn refresh_rejects_garbage() {
    let srv = spawn().await;
    for bad in ["", "not a token", "AAAA", &"A".repeat(43), &"A".repeat(200)] {
        assert_err(
            &srv.post_json(
                "/v1/refresh",
                &RefreshReq {
                    refresh_token: bad.into(),
                },
            )
            .await,
            401,
            ErrorCode::Unauthorized,
        );
    }
}

#[tokio::test]
async fn access_token_expires() {
    let srv = spawn().await;
    let acct = register(&srv, "gina").await;
    let t0 = now_ms();
    srv.set_clock(t0);
    let tok = &acct.tokens.access_token;
    srv.set_clock(t0 + 14 * MIN);
    assert_eq!(get_auth(&srv, "/v1/devices", tok).await.status, 200);
    srv.set_clock(t0 + 16 * MIN);
    assert_err(
        &get_auth(&srv, "/v1/devices", tok).await,
        401,
        ErrorCode::Unauthorized,
    );
    // The refresh token outlives it and yields a working access token.
    let fresh: TokenResp = srv
        .post_json(
            "/v1/refresh",
            &RefreshReq {
                refresh_token: acct.tokens.refresh_token.clone(),
            },
        )
        .await
        .json();
    assert_eq!(
        get_auth(&srv, "/v1/devices", &fresh.access_token)
            .await
            .status,
        200
    );
    // Missing, malformed and unknown credentials.
    assert_err(&srv.get("/v1/devices").await, 401, ErrorCode::Unauthorized);
    assert_err(
        &get_auth(&srv, "/v1/devices", "garbage").await,
        401,
        ErrorCode::Unauthorized,
    );
    assert_err(
        &get_auth(&srv, "/v1/devices", &"A".repeat(43)).await,
        401,
        ErrorCode::Unauthorized,
    );
}

#[tokio::test]
async fn logout_ends_the_session() {
    let srv = spawn().await;
    let acct = register(&srv, "hank").await;
    let t = &acct.tokens;
    assert_eq!(
        post_auth::<()>(&srv, "/v1/logout", &t.access_token, None)
            .await
            .status,
        204
    );
    assert_err(
        &get_auth(&srv, "/v1/devices", &t.access_token).await,
        401,
        ErrorCode::Unauthorized,
    );
    assert_err(
        &srv.post_json(
            "/v1/refresh",
            &RefreshReq {
                refresh_token: t.refresh_token.clone(),
            },
        )
        .await,
        401,
        ErrorCode::Unauthorized,
    );
    // The device still exists and can sign in again.
    assert_eq!(
        login_device(&srv, &acct, &t.device_id, &acct.auth_key)
            .await
            .status,
        200
    );
}

#[tokio::test]
async fn login_without_link_token_cannot_add_device() {
    let srv = spawn().await;
    let acct = register(&srv, "ivy").await;
    let req = core_proto::LoginReq {
        username: "ivy".into(),
        auth_key: acct.auth_key.clone(),
        device: device("New laptop"),
        link_token: None,
    };
    // No device header: a password alone never creates a device.
    assert_err(
        &srv.post_json("/v1/login", &req).await,
        403,
        ErrorCode::Forbidden,
    );
    // A device id that is not this account's is no better.
    let other = register_with(&srv, "jack", 2, 1).await;
    for id in [other.tokens.device_id.as_str(), "no-such-device", ""] {
        let res = call(&srv, "POST", "/v1/login", None, Some(id), Some(&req)).await;
        assert_err(&res, 403, ErrorCode::Forbidden);
    }
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM devices WHERE account_id = (SELECT id FROM accounts WHERE username = 'ivy')")
        .fetch_one(&srv.state.db)
        .await
        .unwrap();
    assert_eq!(n, 1);
    // Wrong key with a good header is still plain unauthorized.
    let res = login_device(&srv, &acct, &acct.tokens.device_id, &[0; 32]).await;
    assert_err(&res, 401, ErrorCode::Unauthorized);
}

#[tokio::test]
async fn link_code_adds_device_once() {
    let srv = spawn().await;
    let acct = register_with(&srv, "kate", 1, 1).await;
    let link = make_link(&srv, &acct.tokens.access_token).await;
    let code = JoinCode::parse(&link.code).unwrap();
    assert_eq!(code.kind, CodeKind::Link);
    assert!(link.expires_at > now_ms() + 9 * MIN && link.expires_at <= now_ms() + 10 * MIN);

    // Bound to the account: somebody else's credentials cannot use it.
    let other = register_with(&srv, "liam", 2, 1).await;
    assert_err(
        &login_link(&srv, "liam", &other.auth_key, code.token.to_vec(), "x").await,
        403,
        ErrorCode::InviteInvalid,
    );
    // Wrong password does not spend it either.
    assert_err(
        &login_link(&srv, "kate", &[0; 32], code.token.to_vec(), "x").await,
        401,
        ErrorCode::Unauthorized,
    );

    let res = login_link(&srv, "kate", &acct.auth_key, code.token.to_vec(), "Laptop").await;
    assert_eq!(res.status, 200, "{}", String::from_utf8_lossy(&res.body));
    let t: TokenResp = res.json();
    assert_ne!(t.device_id, acct.tokens.device_id);
    assert_eq!(
        get_auth(&srv, "/v1/devices", &t.access_token).await.status,
        200
    );

    // Single use.
    assert_err(
        &login_link(&srv, "kate", &acct.auth_key, code.token.to_vec(), "Again").await,
        403,
        ErrorCode::InviteInvalid,
    );
    // The link table keeps only a hash.
    let stored: Vec<u8> = sqlx::query_scalar("SELECT hash FROM link_tokens")
        .fetch_one(&srv.state.db)
        .await
        .unwrap();
    assert_ne!(stored, code.token.to_vec());
    // A link token needs a bearer to be created.
    assert_err(
        &srv.post_json("/v1/link", &serde_json::json!({})).await,
        401,
        ErrorCode::Unauthorized,
    );
}

#[tokio::test]
async fn link_code_expires_after_ten_minutes() {
    let srv = spawn().await;
    let acct = register(&srv, "mia").await;
    let t0 = now_ms();
    srv.set_clock(t0);
    let early = make_link(&srv, &acct.tokens.access_token).await;
    let late = make_link(&srv, &acct.tokens.access_token).await;
    assert_eq!(early.expires_at, t0 + 10 * MIN);

    srv.set_clock(t0 + 10 * MIN - 1000);
    assert_eq!(
        login_link(&srv, "mia", &acct.auth_key, code_token(&early.code), "a")
            .await
            .status,
        200
    );
    srv.set_clock(t0 + 10 * MIN + 1000);
    assert_err(
        &login_link(&srv, "mia", &acct.auth_key, code_token(&late.code), "b").await,
        403,
        ErrorCode::InviteInvalid,
    );
}

#[tokio::test]
async fn node_ids_unique_per_account() {
    let srv = spawn().await;
    // The first device keeps its hint; the others get the lowest unused id >= 1.
    let acct = register_with(&srv, "nina", 1, 3).await;
    let d2 = add_device(&srv, &acct, "two").await;
    let d3 = add_device(&srv, &acct, "three").await;
    let d4 = add_device(&srv, &acct, "four").await;
    assert_eq!([acct.tokens.node, d2.node, d3.node, d4.node], [3, 1, 2, 4]);
    // A revoked device keeps its node reserved (its HLCs may still exist).
    assert_eq!(
        delete_auth(
            &srv,
            &format!("/v1/devices/{}", d2.device_id),
            &acct.tokens.access_token
        )
        .await
        .status,
        204
    );
    let d5 = add_device(&srv, &acct, "five").await;
    assert_eq!(d5.node, 5);

    // Another account may reuse the same ids; hint 0 is a legal node.
    let other = register_with(&srv, "omar", 2, 1).await;
    assert_eq!(other.tokens.node, 1);
    let zero = register_with(&srv, "pat", 3, 0).await;
    assert_eq!(zero.tokens.node, 0);
    assert_eq!(add_device(&srv, &zero, "z2").await.node, 1);
}

#[tokio::test]
async fn recover_login_issues_limited_token() {
    let srv = spawn().await;
    let acct = register(&srv, "quinn").await;
    let rec = RecoverReq {
        username: "quinn".into(),
        recovery_auth_key: acct.recovery_key.clone(),
        device: device("Recovering"),
    };
    let res = srv.post_json("/v1/recover-login", &rec).await;
    assert_eq!(res.status, 200, "{}", String::from_utf8_lossy(&res.body));
    let t: TokenResp = res.json();
    assert_eq!(t.expires_in, 600);
    assert_eq!(token_bytes(&t.access_token).unwrap().len(), 32);
    // No refresh token, no device.
    assert_eq!((t.refresh_token.as_str(), t.device_id.as_str()), ("", ""));

    // The limited token cannot use device endpoints.
    for (m, p) in [
        ("GET", "/v1/devices"),
        ("POST", "/v1/link"),
        ("POST", "/v1/logout"),
    ] {
        let res = call::<()>(&srv, m, p, Some(&t.access_token), None, None).await;
        assert_err(&res, 403, ErrorCode::Forbidden);
    }
    let devices: i64 = sqlx::query_scalar("SELECT count(*) FROM devices")
        .fetch_one(&srv.state.db)
        .await
        .unwrap();
    assert_eq!(devices, 1);

    // It expires after ten minutes.
    srv.set_clock(now_ms() + 11 * MIN);
    assert_err(
        &get_auth(&srv, "/v1/devices", &t.access_token).await,
        401,
        ErrorCode::Unauthorized,
    );

    // A wrong recovery key, and the account's own AuthKey, are refused.
    for key in [vec![0u8; 32], acct.auth_key.clone()] {
        let bad = RecoverReq {
            recovery_auth_key: key,
            ..rec.clone()
        };
        assert_err(
            &srv.post_json("/v1/recover-login", &bad).await,
            401,
            ErrorCode::Unauthorized,
        );
    }
    // The recovery key is not a password.
    assert_err(
        &login_device(&srv, &acct, &acct.tokens.device_id, &acct.recovery_key).await,
        401,
        ErrorCode::Unauthorized,
    );
}

#[tokio::test]
async fn disabled_account_cannot_sign_in_or_use_tokens() {
    let srv = spawn().await;
    let acct = register(&srv, "ruth").await;
    sqlx::query("UPDATE accounts SET disabled = 1")
        .execute(&srv.state.db)
        .await
        .unwrap();
    assert_err(
        &login_device(&srv, &acct, &acct.tokens.device_id, &acct.auth_key).await,
        403,
        ErrorCode::Forbidden,
    );
    assert_err(
        &get_auth(&srv, "/v1/devices", &acct.tokens.access_token).await,
        403,
        ErrorCode::Forbidden,
    );
    assert_err(
        &srv.post_json(
            "/v1/refresh",
            &RefreshReq {
                refresh_token: acct.tokens.refresh_token.clone(),
            },
        )
        .await,
        403,
        ErrorCode::Forbidden,
    );
}

#[tokio::test]
async fn relogin_requires_device_secret() {
    let srv = spawn().await;
    let acct = register(&srv, "sven").await;
    let first = &acct.tokens;
    // Device creation issued a secret (32 bytes, base64url); only its hash is stored.
    assert_eq!(token_bytes(&first.device_secret).unwrap().len(), 32);
    let stored: Vec<u8> = sqlx::query_scalar("SELECT secret_hash FROM devices")
        .fetch_one(&srv.state.db)
        .await
        .unwrap();
    assert_ne!(stored, token_bytes(&first.device_secret).unwrap());
    // A device added with a link code gets its own, different secret.
    let second = add_device(&srv, &acct, "Laptop").await;
    assert_eq!(token_bytes(&second.device_secret).unwrap().len(), 32);
    assert_ne!(second.device_secret, first.device_secret);

    let id = &first.device_id;
    // Missing, malformed, and another device's secret: unauthorized, counted.
    for secret in [
        None,
        Some(""),
        Some("junk"),
        Some(second.device_secret.as_str()),
    ] {
        let res = login_device_with(&srv, "sven", id, secret, &acct.auth_key).await;
        assert_err(&res, 401, ErrorCode::Unauthorized);
    }
    assert_eq!(
        db_i64(
            &srv,
            "SELECT failed_logins FROM accounts WHERE username = ?",
            "sven"
        )
        .await,
        4
    );
    // The right secret plus the AuthKey signs the device back in; no new secret.
    let res = login_device_with(&srv, "sven", id, Some(&first.device_secret), &acct.auth_key).await;
    assert_eq!(res.status, 200, "{}", String::from_utf8_lossy(&res.body));
    let again: TokenResp = res.json();
    assert_eq!(
        (again.device_id.as_str(), again.device_secret.as_str()),
        (id.as_str(), "")
    );
    assert_eq!(
        db_i64(
            &srv,
            "SELECT failed_logins FROM accounts WHERE username = ?",
            "sven"
        )
        .await,
        0
    );
    // The secret without the right AuthKey is useless too.
    let res = login_device_with(&srv, "sven", id, Some(&first.device_secret), &[3; 32]).await;
    assert_err(&res, 401, ErrorCode::Unauthorized);
    // Refresh and recover-login never carry a secret.
    let r: TokenResp = srv
        .post_json(
            "/v1/refresh",
            &RefreshReq {
                refresh_token: again.refresh_token,
            },
        )
        .await
        .json();
    assert_eq!(r.device_secret, "");
}

#[tokio::test]
async fn revoked_device_cannot_relogin_as_another_device() {
    let srv = spawn().await;
    let acct = register(&srv, "rita").await;
    let a = acct.tokens.clone();
    let b = add_device(&srv, &acct, "B").await;
    let res = delete_auth(
        &srv,
        &format!("/v1/devices/{}", a.device_id),
        &b.access_token,
    )
    .await;
    assert_eq!(res.status, 204);

    // With the password and the revoked device's own id and secret: revoked.
    let res = login_device_with(
        &srv,
        "rita",
        &a.device_id,
        Some(&a.device_secret),
        &acct.auth_key,
    )
    .await;
    assert_err(&res, 401, ErrorCode::Revoked);
    // Password and the revoked device's id alone: no way back in.
    let res = login_device_with(&srv, "rita", &a.device_id, None, &acct.auth_key).await;
    assert_err(&res, 401, ErrorCode::Unauthorized);
    // Presenting the revoked device's secret for a live device's id fails.
    let res = login_device_with(
        &srv,
        "rita",
        &b.device_id,
        Some(&a.device_secret),
        &acct.auth_key,
    )
    .await;
    assert_err(&res, 401, ErrorCode::Unauthorized);
    // Nothing was created, and the live device is unaffected.
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM devices")
        .fetch_one(&srv.state.db)
        .await
        .unwrap();
    assert_eq!(n, 2);
    assert_eq!(
        get_auth(&srv, "/v1/devices", &b.access_token).await.status,
        200
    );
    let res = login_device_with(
        &srv,
        "rita",
        &b.device_id,
        Some(&b.device_secret),
        &acct.auth_key,
    )
    .await;
    assert_eq!(res.status, 200);
}

#[tokio::test]
async fn failure_count_decays_after_window() {
    let srv = spawn().await;
    let acct = register(&srv, "dora").await;
    let dev = acct.tokens.device_id.clone();
    let t0 = now_ms() + 2 * MIN;
    for i in 0..9 {
        srv.set_clock(t0 + i * 4_000);
        assert_err(
            &login_device(&srv, &acct, &dev, &[9; 32]).await,
            401,
            ErrorCode::Unauthorized,
        );
    }
    assert_eq!(
        db_i64(
            &srv,
            "SELECT failed_logins FROM accounts WHERE username = ?",
            "dora"
        )
        .await,
        9
    );
    // Quiet for longer than the lockout window: the nine no longer count, so
    // this tenth failure is the first of a new run and does not lock.
    srv.set_clock(t0 + 16 * MIN);
    assert_err(
        &login_device(&srv, &acct, &dev, &[9; 32]).await,
        401,
        ErrorCode::Unauthorized,
    );
    assert_eq!(
        db_i64(
            &srv,
            "SELECT failed_logins FROM accounts WHERE username = ?",
            "dora"
        )
        .await,
        1
    );
    assert_eq!(
        login_device(&srv, &acct, &dev, &acct.auth_key).await.status,
        200
    );
}

#[tokio::test]
async fn rate_limit_keys_on_the_peer_address() {
    for srv in [spawn().await, spawn_tls().await] {
        assert_eq!(prelogin(&srv, "somebody").await.status, 200);
        let ips = srv.state.limiter.tracked_ips();
        assert_eq!(
            ips,
            vec!["127.0.0.1".parse::<std::net::IpAddr>().unwrap()],
            "{}",
            srv.url
        );
    }
}

/// The limited recovery token passes `RecoveryAuth` and nothing else; a full
/// session token is the other way round.
#[tokio::test]
async fn extractors_keep_the_two_scopes_apart() {
    use axum::extract::FromRequestParts;
    use axum::http::Request;
    use tether_server::auth::{AuthDevice, RecoveryAuth};

    let srv = spawn().await;
    let acct = register(&srv, "scoped").await;
    let rec: TokenResp = srv
        .post_json(
            "/v1/recover-login",
            &RecoverReq {
                username: "scoped".into(),
                recovery_auth_key: acct.recovery_key.clone(),
                device: device("d"),
            },
        )
        .await
        .json();

    fn parts(token: &str) -> axum::http::request::Parts {
        Request::builder()
            .header("authorization", format!("Bearer {token}"))
            .body(())
            .unwrap()
            .into_parts()
            .0
    }
    let st = &srv.state;
    let mut p = parts(&acct.tokens.access_token);
    let dev = AuthDevice::from_request_parts(&mut p, st).await.unwrap();
    assert_eq!(dev.device_id, acct.tokens.device_id);
    assert_eq!(dev.vault_id, acct.vault_id);
    assert_eq!(
        RecoveryAuth::from_request_parts(&mut p, st)
            .await
            .unwrap_err()
            .code(),
        ErrorCode::Forbidden
    );

    let mut p = parts(&rec.access_token);
    let who = RecoveryAuth::from_request_parts(&mut p, st).await.unwrap();
    assert_eq!(who.vault_id, acct.vault_id);
    assert_eq!(
        AuthDevice::from_request_parts(&mut p, st)
            .await
            .unwrap_err()
            .code(),
        ErrorCode::Forbidden
    );
    assert_eq!(
        RecoveryAuth::from_request_parts(&mut parts("garbage"), st)
            .await
            .unwrap_err()
            .code(),
        ErrorCode::Unauthorized
    );
}

// ---------------------------------------------------------------- logging

#[derive(Clone, Default)]
struct Sink(Arc<Mutex<Vec<u8>>>);
impl std::io::Write for Sink {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Sink {
    type Writer = Sink;
    fn make_writer(&'a self) -> Sink {
        self.clone()
    }
}

/// Install a process-wide subscriber that captures everything (the server runs
/// on other threads, so a thread-local default would miss it).
fn capture() -> Sink {
    static SINK: OnceLock<Sink> = OnceLock::new();
    SINK.get_or_init(|| {
        let sink = Sink::default();
        let sub = tracing_subscriber::fmt()
            .with_writer(sink.clone())
            .with_ansi(false)
            .with_max_level(tracing::Level::TRACE)
            .finish();
        tracing::subscriber::set_global_default(sub).expect("no other global subscriber");
        sink
    })
    .clone()
}

#[tokio::test]
async fn log_lines_hold_no_tokens_or_auth_keys() {
    use data_encoding::{BASE64, BASE64URL_NOPAD, HEXLOWER};
    let sink = capture();
    let srv = spawn().await;

    // Distinctive secrets so any rendering is easy to find.
    let invite = srv.make_invite().await;
    let mut req = register_req(&invite, "logcheck-user", 0xC4, 1);
    req.auth_key = vec![0xC4; 32];
    req.recovery_auth_key = vec![0xFE; 32];
    let res = srv.post_json("/v1/register", &req).await;
    assert_eq!(res.status, 200);
    let t: TokenResp = res.json();
    let login = login_device(
        &srv,
        &Account {
            username: "logcheck-user".into(),
            auth_key: req.auth_key.clone(),
            recovery_key: req.recovery_auth_key.clone(),
            vault_id: req.material.vault_id.clone(),
            tokens: t.clone(),
        },
        &t.device_id,
        &req.auth_key,
    )
    .await;
    assert_eq!(login.status, 200);
    let t2: TokenResp = login.json();
    // A failure, a refresh, a link, a lost-answer retry, a reuse revocation
    // and a recover-login.
    let bad = core_proto::LoginReq {
        username: "logcheck-user".into(),
        auth_key: vec![0xAB; 32],
        device: device("x"),
        link_token: None,
    };
    call(
        &srv,
        "POST",
        "/v1/login",
        None,
        Some(&t.device_id),
        Some(&bad),
    )
    .await;
    let link = make_link(&srv, &t2.access_token).await;
    let rec = RecoverReq {
        username: "logcheck-user".into(),
        recovery_auth_key: req.recovery_auth_key.clone(),
        device: device("r"),
    };
    let rt: TokenResp = srv.post_json("/v1/recover-login", &rec).await.json();
    let rotated: TokenResp = srv
        .post_json(
            "/v1/refresh",
            &RefreshReq {
                refresh_token: t2.refresh_token.clone(),
            },
        )
        .await
        .json();
    let retried: TokenResp = srv
        .post_json(
            "/v1/refresh",
            &RefreshReq {
                refresh_token: t2.refresh_token.clone(),
            },
        )
        .await
        .json();
    srv.post_json(
        "/v1/refresh",
        &RefreshReq {
            refresh_token: t2.refresh_token.clone(),
        },
    )
    .await;

    let log = String::from_utf8(sink.0.lock().unwrap().clone()).unwrap();
    assert!(!log.is_empty());
    let mut secrets: Vec<(&str, Vec<u8>)> = vec![
        ("auth key", req.auth_key.clone()),
        ("wrong auth key", vec![0xAB; 32]),
        ("recovery key", req.recovery_auth_key.clone()),
        ("invite token", req.invite_token.clone()),
        ("link token", code_token(&link.code)),
    ];
    for tok in [&t, &t2, &rt, &rotated, &retried] {
        for s in [&tok.access_token, &tok.refresh_token, &tok.device_secret] {
            if !s.is_empty() {
                secrets.push(("session token", token_bytes(s).unwrap()));
            }
        }
    }
    for (what, bytes) in secrets {
        for rendering in [
            BASE64.encode(&bytes),
            BASE64URL_NOPAD.encode(&bytes),
            HEXLOWER.encode(&bytes),
            format!("{bytes:?}"),
        ] {
            assert!(
                !log.contains(&rendering),
                "{what} leaked into the log as {rendering}"
            );
        }
    }
    assert!(!log.contains(&link.code), "link code leaked");
    assert!(!log.contains(&invite), "invite code leaked");
    // The username is not a secret, but there is no reason to log it either.
    assert!(!log.contains("logcheck-user"), "username logged");
    // The log is not empty of useful facts: account and device ids appear.
    assert!(log.contains(&t.device_id), "device id should be logged");
}

async fn refresh_with(srv: &TestServer, token: &str) -> Response {
    srv.post_json(
        "/v1/refresh",
        &RefreshReq {
            refresh_token: token.to_owned(),
        },
    )
    .await
}

#[tokio::test]
async fn a_lost_refresh_answer_is_retried_within_the_grace() {
    let srv = spawn().await;
    let acct = register(&srv, "grace").await;
    let t0 = now_ms();
    srv.set_clock(t0);
    let r1 = acct.tokens.refresh_token.clone();
    // The answer to this exchange never reaches the device...
    let lost: TokenResp = refresh_with(&srv, &r1).await.json();
    // ...so it presents the old token again a few minutes later.
    srv.set_clock(t0 + 9 * MIN);
    let res = refresh_with(&srv, &r1).await;
    assert_eq!(res.status, 200, "{}", String::from_utf8_lossy(&res.body));
    let again: TokenResp = res.json();
    assert_ne!(again.refresh_token, lost.refresh_token);
    assert_eq!(again.device_id, acct.tokens.device_id);
    // The new session works; the unused successor was invalidated (not a
    // revocation: it is simply unknown now).
    assert_eq!(
        get_auth(&srv, "/v1/devices", &again.access_token)
            .await
            .status,
        200
    );
    assert_err(
        &get_auth(&srv, "/v1/devices", &lost.access_token).await,
        401,
        ErrorCode::Unauthorized,
    );
    assert_err(
        &refresh_with(&srv, &lost.refresh_token).await,
        401,
        ErrorCode::Unauthorized,
    );
    // The newest token keeps rotating normally.
    let next = refresh_with(&srv, &again.refresh_token).await;
    assert_eq!(next.status, 200);
    // The grace is granted once: the old token a third time revokes.
    assert_err(&refresh_with(&srv, &r1).await, 401, ErrorCode::Revoked);
    assert_err(
        &get_auth(&srv, "/v1/devices", &next.json::<TokenResp>().access_token).await,
        401,
        ErrorCode::Revoked,
    );
}

#[tokio::test]
async fn the_refresh_grace_ends_after_ten_minutes() {
    let srv = spawn().await;
    let acct = register(&srv, "lategrace").await;
    let t0 = now_ms();
    srv.set_clock(t0);
    let r1 = acct.tokens.refresh_token.clone();
    let _lost: TokenResp = refresh_with(&srv, &r1).await.json();
    srv.set_clock(t0 + 10 * MIN);
    assert_err(&refresh_with(&srv, &r1).await, 401, ErrorCode::Revoked);
    assert_err(
        &login_device(&srv, &acct, &acct.tokens.device_id, &acct.auth_key).await,
        401,
        ErrorCode::Revoked,
    );
}

#[tokio::test]
async fn reuse_after_the_successor_was_used_revokes() {
    // Used by an access token...
    let srv = spawn().await;
    let acct = register(&srv, "usedaccess").await;
    let r1 = acct.tokens.refresh_token.clone();
    let t2: TokenResp = refresh_with(&srv, &r1).await.json();
    assert_eq!(
        get_auth(&srv, "/v1/devices", &t2.access_token).await.status,
        200
    );
    assert_err(&refresh_with(&srv, &r1).await, 401, ErrorCode::Revoked);
    assert_err(
        &refresh_with(&srv, &t2.refresh_token).await,
        401,
        ErrorCode::Revoked,
    );

    // ...or over the events socket...
    let acct = register_with(&srv, "usedws", 2, 2).await;
    let r1 = acct.tokens.refresh_token.clone();
    let t2: TokenResp = refresh_with(&srv, &r1).await.json();
    drop(ws_connect(&srv, &t2.access_token).await);
    assert_err(&refresh_with(&srv, &r1).await, 401, ErrorCode::Revoked);

    // ...or by refreshing with it.
    let acct = register_with(&srv, "usedrefresh", 3, 3).await;
    let r1 = acct.tokens.refresh_token.clone();
    let t2: TokenResp = refresh_with(&srv, &r1).await.json();
    let t3: TokenResp = refresh_with(&srv, &t2.refresh_token).await.json();
    assert_err(&refresh_with(&srv, &r1).await, 401, ErrorCode::Revoked);
    assert_err(
        &refresh_with(&srv, &t3.refresh_token).await,
        401,
        ErrorCode::Revoked,
    );
}

#[tokio::test]
async fn concurrent_refreshes_leave_one_live_session() {
    let srv = spawn().await;
    for (i, name) in ["race1", "race2", "race3", "race4"].into_iter().enumerate() {
        let k = u8::try_from(i + 1).unwrap();
        let acct = register_with(&srv, name, k, u16::from(k)).await;
        let r1 = acct.tokens.refresh_token.clone();
        let (a, b) = tokio::join!(refresh_with(&srv, &r1), refresh_with(&srv, &r1));
        assert_eq!((a.status, b.status), (200, 200));
        let (a, b): (TokenResp, TokenResp) = (a.json(), b.json());
        // Exactly one of the two answers holds a live session.
        let live_a = get_auth(&srv, "/v1/devices", &a.access_token).await.status == 200;
        let live_b = get_auth(&srv, "/v1/devices", &b.access_token).await.status == 200;
        assert!(live_a ^ live_b, "{name}: {live_a} {live_b}");
        let (live, dead) = if live_a { (&a, &b) } else { (&b, &a) };
        assert_err(
            &refresh_with(&srv, &dead.refresh_token).await,
            401,
            ErrorCode::Unauthorized,
        );
        assert_eq!(refresh_with(&srv, &live.refresh_token).await.status, 200);
        // A third presentation of the shared token is theft.
        assert_err(&refresh_with(&srv, &r1).await, 401, ErrorCode::Revoked);
    }
}
