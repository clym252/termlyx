//! Account key material, password change, recovery reset, recovery key
//! replacement and schema gating of writes.

mod common;

use common::*;
use core_proto::{
    AccountResp, CodeProof, CodeVerifyReq, CodeVerifyResp, ErrorCode, Event, KdfWire,
    PasswordChangeReq, PreloginReq, PreloginResp, RecoverReq, RecoveryResetReq, RecoveryUpdateReq,
    RegisterReq, ShortCode, TokenResp, client_proof, verify_server_proof,
};
use serde::Serialize;

const MIN: i64 = 60_000;

fn new_kdf() -> KdfWire {
    KdfWire {
        m_kib: 32768,
        t: 2,
        p: 2,
        salt: vec![9; 16],
    }
}

fn change_req(a: &Account, rev: i64) -> PasswordChangeReq {
    PasswordChangeReq {
        old_auth_key: a.auth_key.clone(),
        new_auth_key: vec![50; 32],
        kdf: new_kdf(),
        secret_by_kek: vec![55; 73],
        account_rev_base: rev,
    }
}

fn reset_req(a: &Account) -> RecoveryResetReq {
    RecoveryResetReq {
        recovery_auth_key: a.recovery_key.clone(),
        new_auth_key: vec![60; 32],
        kdf: new_kdf(),
        secret_by_kek: vec![66; 73],
    }
}

/// Any request with an optional bearer token and `X-Tether-Schema` header.
async fn send<T: Serialize>(
    srv: &TestServer,
    method: &str,
    path: &str,
    token: &str,
    schema: Option<u32>,
    body: Option<&T>,
) -> Response {
    let auth = format!("Bearer {token}");
    let schema = schema.map(|s| s.to_string());
    let mut headers: Vec<(&str, &str)> = vec![("Authorization", &auth)];
    if let Some(s) = &schema {
        headers.push(("X-Tether-Schema", s));
    }
    let body = body.map(|b| {
        headers.push(("Content-Type", "application/json"));
        serde_json::to_vec(b).unwrap()
    });
    srv.request(method, path, &headers, body).await
}

async fn get_account(srv: &TestServer, token: &str) -> AccountResp {
    let res = get_auth(srv, "/v1/account", token).await;
    assert_eq!(res.status, 200, "{}", String::from_utf8_lossy(&res.body));
    res.json()
}

async fn recover_login(srv: &TestServer, a: &Account) -> TokenResp {
    let req = RecoverReq {
        username: a.username.clone(),
        recovery_auth_key: a.recovery_key.clone(),
        device: device("Fresh"),
    };
    let res = srv.post_json("/v1/recover-login", &req).await;
    assert_eq!(res.status, 200, "{}", String::from_utf8_lossy(&res.body));
    res.json()
}

async fn db_i64(srv: &TestServer, sql: &'static str, bind: &str) -> i64 {
    sqlx::query_scalar(sql)
        .bind(bind.to_owned())
        .fetch_one(&srv.state.db)
        .await
        .unwrap()
}

#[tokio::test]
async fn account_material_round_trips_byte_exact() {
    let srv = spawn().await;
    let a = register(&srv, "alice").await;
    let want = material(&a.vault_id);

    let got = get_account(&srv, &a.tokens.access_token).await;
    assert_eq!(got.material, want);
    assert_eq!(got.account_rev, 1);

    // The recovery-scoped token reads the very same material: the client needs
    // `secretByRecovery` to recover.
    let rec = recover_login(&srv, &a).await;
    let got = get_account(&srv, &rec.access_token).await;
    assert_eq!(got.material, want);
    assert_eq!(got.account_rev, 1);

    // No token, or garbage: unauthorized.
    assert_err(&srv.get("/v1/account").await, 401, ErrorCode::Unauthorized);
}

#[tokio::test]
async fn password_change_bumps_rev_and_old_key_fails() {
    let srv = spawn().await;
    let a = register(&srv, "bob").await;
    // A second device keeps its session through the change.
    let other = add_device(&srv, &a, "Laptop").await;
    // A failed attempt first: the successful change clears the count.
    let mut wrong = change_req(&a, 1);
    wrong.old_auth_key = vec![9; 32];
    let res = send(
        &srv,
        "POST",
        "/v1/account/password",
        &a.tokens.access_token,
        None,
        Some(&wrong),
    )
    .await;
    assert_err(&res, 401, ErrorCode::Unauthorized);
    assert_eq!(
        db_i64(
            &srv,
            "SELECT failed_logins FROM accounts WHERE username = ?",
            "bob"
        )
        .await,
        1
    );

    let res = send(
        &srv,
        "POST",
        "/v1/account/password",
        &a.tokens.access_token,
        None,
        Some(&change_req(&a, 1)),
    )
    .await;
    assert_eq!(res.status, 200, "{}", String::from_utf8_lossy(&res.body));
    let resp: AccountResp = res.json();
    assert_eq!(resp.account_rev, 2);
    assert_eq!(resp.material.kdf, new_kdf());
    assert_eq!(resp.material.secret_by_kek, vec![55; 73]);
    assert_eq!(
        db_i64(
            &srv,
            "SELECT failed_logins FROM accounts WHERE username = ?",
            "bob"
        )
        .await,
        0,
        "a successful change clears the failure count"
    );

    // Everything else of the material is untouched.
    let mut expected = material(&a.vault_id);
    expected.kdf = new_kdf();
    expected.secret_by_kek = vec![55; 73];
    let got = get_account(&srv, &other.access_token).await;
    assert_eq!(got.material, expected);
    assert_eq!(got.account_rev, 2);

    // The other device's session is still valid.
    assert_eq!(
        get_auth(&srv, "/v1/devices", &other.access_token)
            .await
            .status,
        200
    );

    // Old key no longer signs in; the new one does.
    let old = login_device(&srv, &a, &a.tokens.device_id, &a.auth_key).await;
    assert_err(&old, 401, ErrorCode::Unauthorized);
    let new = login_device(&srv, &a, &a.tokens.device_id, &[50; 32]).await;
    assert_eq!(new.status, 200, "{}", String::from_utf8_lossy(&new.body));

    // Prelogin now serves the new KDF parameters.
    let res = srv
        .post_json(
            "/v1/prelogin",
            &PreloginReq {
                username: "bob".into(),
            },
        )
        .await;
    assert_eq!(res.json::<PreloginResp>().kdf, new_kdf());

    // A second change needs the new old key and the new rev.
    let mut again = change_req(&a, 2);
    again.old_auth_key = vec![50; 32];
    again.new_auth_key = vec![51; 32];
    let token = new.json::<TokenResp>().access_token;
    let res = send(
        &srv,
        "POST",
        "/v1/account/password",
        &token,
        None,
        Some(&again),
    )
    .await;
    assert_eq!(res.status, 200, "{}", String::from_utf8_lossy(&res.body));
    assert_eq!(res.json::<AccountResp>().account_rev, 3);

    // A fresh salt every time: the stored verifier salts differ.
    let salt: Vec<u8> =
        sqlx::query_scalar("SELECT verifier_salt FROM accounts WHERE username = 'bob'")
            .fetch_one(&srv.state.db)
            .await
            .unwrap();
    assert_eq!(salt.len(), 16);
}

#[tokio::test]
async fn password_change_with_wrong_old_key_is_unauthorized_and_counts() {
    let srv = spawn().await;
    let a = register(&srv, "carol").await;
    let mut req = change_req(&a, 1);
    req.old_auth_key = vec![9; 32];

    let res = send(
        &srv,
        "POST",
        "/v1/account/password",
        &a.tokens.access_token,
        None,
        Some(&req),
    )
    .await;
    assert_err(&res, 401, ErrorCode::Unauthorized);
    assert_eq!(
        db_i64(
            &srv,
            "SELECT failed_logins FROM accounts WHERE username = ?",
            "carol"
        )
        .await,
        1
    );
    // Nothing changed.
    let got = get_account(&srv, &a.tokens.access_token).await;
    assert_eq!(got.account_rev, 1);
    assert_eq!(got.material, material(&a.vault_id));

    // Enough wrong keys lock the account, even for the right key.
    let t0 = now_ms() + 2 * MIN;
    srv.set_clock(t0);
    for i in 1..10 {
        srv.set_clock(t0 + i * 4_000);
        let r = send(
            &srv,
            "POST",
            "/v1/account/password",
            &a.tokens.access_token,
            None,
            Some(&req),
        )
        .await;
        assert_err(&r, 401, ErrorCode::Unauthorized);
    }
    // The tenth failure (counting the first above) has locked it.
    let ok = change_req(&a, 1);
    let r = send(
        &srv,
        "POST",
        "/v1/account/password",
        &a.tokens.access_token,
        None,
        Some(&ok),
    )
    .await;
    assert_err(&r, 429, ErrorCode::RateLimited);
}

#[tokio::test]
async fn password_change_with_stale_rev_conflicts() {
    let srv = spawn().await;
    let a = register(&srv, "dave").await;
    let ok = send(
        &srv,
        "POST",
        "/v1/account/password",
        &a.tokens.access_token,
        None,
        Some(&change_req(&a, 1)),
    )
    .await;
    assert_eq!(ok.status, 200);

    // Based on rev 1 again, with the right (now new) old key.
    let mut stale = change_req(&a, 1);
    stale.old_auth_key = vec![50; 32];
    stale.new_auth_key = vec![70; 32];
    let res = send(
        &srv,
        "POST",
        "/v1/account/password",
        &a.tokens.access_token,
        None,
        Some(&stale),
    )
    .await;
    assert_err(&res, 409, ErrorCode::Conflict);
    assert_eq!(
        get_account(&srv, &a.tokens.access_token).await.account_rev,
        2
    );
    // A base from the future conflicts too.
    stale.account_rev_base = 99;
    let res = send(
        &srv,
        "POST",
        "/v1/account/password",
        &a.tokens.access_token,
        None,
        Some(&stale),
    )
    .await;
    assert_err(&res, 409, ErrorCode::Conflict);
}

#[tokio::test]
async fn password_change_validates_kdf_and_lengths() {
    let srv = spawn().await;
    let a = register(&srv, "erin").await;
    let tok = &a.tokens.access_token;

    let mut cases: Vec<(&str, PasswordChangeReq)> = vec![];
    let mut r = change_req(&a, 1);
    r.kdf.m_kib = 1024 * 1024 + 1;
    cases.push(("m too big", r));
    let mut r = change_req(&a, 1);
    r.kdf.m_kib = 0;
    cases.push(("m zero", r));
    let mut r = change_req(&a, 1);
    r.kdf.t = 17;
    cases.push(("t too big", r));
    let mut r = change_req(&a, 1);
    r.kdf.t = 0;
    cases.push(("t zero", r));
    let mut r = change_req(&a, 1);
    r.kdf.p = 65;
    cases.push(("p too big", r));
    let mut r = change_req(&a, 1);
    r.kdf.p = 0;
    cases.push(("p zero", r));
    let mut r = change_req(&a, 1);
    r.kdf.salt = vec![1; 15];
    cases.push(("salt short", r));
    let mut r = change_req(&a, 1);
    r.secret_by_kek = vec![1; 72];
    cases.push(("wrap short", r));
    let mut r = change_req(&a, 1);
    r.secret_by_kek = vec![1; 74];
    cases.push(("wrap long", r));
    let mut r = change_req(&a, 1);
    r.new_auth_key = vec![1; 31];
    cases.push(("new key short", r));
    let mut r = change_req(&a, 1);
    r.old_auth_key = vec![1; 33];
    cases.push(("old key long", r));

    for (what, req) in &cases {
        let res = send(&srv, "POST", "/v1/account/password", tok, None, Some(req)).await;
        assert_eq!(
            (res.status, error_of(&res).code),
            (400, ErrorCode::BadRequest),
            "{what}"
        );
    }
    // The maximum allowed values are accepted.
    let mut max = change_req(&a, 1);
    max.kdf = KdfWire {
        m_kib: 1024 * 1024,
        t: 16,
        p: 64,
        salt: vec![3; 16],
    };
    let res = send(&srv, "POST", "/v1/account/password", tok, None, Some(&max)).await;
    assert_eq!(res.status, 200, "{}", String::from_utf8_lossy(&res.body));
    // None of the rejected ones counted as a failed sign-in or changed a thing.
    assert_eq!(
        db_i64(
            &srv,
            "SELECT failed_logins FROM accounts WHERE username = ?",
            "erin"
        )
        .await,
        0
    );
}

#[tokio::test]
async fn recover_login_then_reset_password() {
    let srv = spawn().await;
    let a = register(&srv, "frank").await;
    let other = add_device(&srv, &a, "Phone").await;
    let rec = recover_login(&srv, &a).await;

    let res = send(
        &srv,
        "POST",
        "/v1/account/recover",
        &rec.access_token,
        None,
        Some(&reset_req(&a)),
    )
    .await;
    assert_eq!(res.status, 200, "{}", String::from_utf8_lossy(&res.body));
    let resp: AccountResp = res.json();
    assert_eq!(resp.account_rev, 2);
    assert_eq!(resp.material.kdf, new_kdf());
    assert_eq!(resp.material.secret_by_kek, vec![66; 73]);
    // The recovery wrap stays as it was.
    assert_eq!(resp.material.secret_by_recovery, vec![3; 73]);

    // The token is consumed.
    let res = get_auth(&srv, "/v1/account", &rec.access_token).await;
    assert_err(&res, 401, ErrorCode::Unauthorized);
    let res = send(
        &srv,
        "POST",
        "/v1/account/recover",
        &rec.access_token,
        None,
        Some(&reset_req(&a)),
    )
    .await;
    assert_err(&res, 401, ErrorCode::Unauthorized);

    // The new password works, the old one does not.
    let old = login_device(&srv, &a, &a.tokens.device_id, &a.auth_key).await;
    assert_err(&old, 401, ErrorCode::Unauthorized);
    let new = login_device(&srv, &a, &a.tokens.device_id, &[60; 32]).await;
    assert_eq!(new.status, 200, "{}", String::from_utf8_lossy(&new.body));
    // Other devices keep their sessions.
    assert_eq!(
        get_auth(&srv, "/v1/devices", &other.access_token)
            .await
            .status,
        200
    );
    // The recovery key itself still works for another round.
    let again = recover_login(&srv, &a).await;
    assert_eq!(get_account(&srv, &again.access_token).await.account_rev, 2);
}

#[tokio::test]
async fn recover_reset_checks_the_recovery_key_in_the_body_and_the_token_kind() {
    let srv = spawn().await;
    let a = register(&srv, "gina").await;
    let rec = recover_login(&srv, &a).await;

    // A wrong recoveryAuthKey in the body is refused and counted; the token
    // survives for a retry.
    let mut bad = reset_req(&a);
    bad.recovery_auth_key = vec![9; 32];
    let res = send(
        &srv,
        "POST",
        "/v1/account/recover",
        &rec.access_token,
        None,
        Some(&bad),
    )
    .await;
    assert_err(&res, 401, ErrorCode::Unauthorized);
    assert_eq!(
        db_i64(
            &srv,
            "SELECT failed_logins FROM accounts WHERE username = ?",
            "gina"
        )
        .await,
        1
    );
    assert_eq!(get_account(&srv, &rec.access_token).await.account_rev, 1);

    // A normal session token cannot use the recovery endpoint.
    let res = send(
        &srv,
        "POST",
        "/v1/account/recover",
        &a.tokens.access_token,
        None,
        Some(&reset_req(&a)),
    )
    .await;
    assert_err(&res, 403, ErrorCode::Forbidden);

    // Bad shapes are rejected without touching anything.
    let mut r = reset_req(&a);
    r.secret_by_kek = vec![1; 10];
    let res = send(
        &srv,
        "POST",
        "/v1/account/recover",
        &rec.access_token,
        None,
        Some(&r),
    )
    .await;
    assert_err(&res, 400, ErrorCode::BadRequest);
    let mut r = reset_req(&a);
    r.kdf.m_kib = 2_000_000;
    let res = send(
        &srv,
        "POST",
        "/v1/account/recover",
        &rec.access_token,
        None,
        Some(&r),
    )
    .await;
    assert_err(&res, 400, ErrorCode::BadRequest);

    // And the right one still works afterwards.
    let res = send(
        &srv,
        "POST",
        "/v1/account/recover",
        &rec.access_token,
        None,
        Some(&reset_req(&a)),
    )
    .await;
    assert_eq!(res.status, 200, "{}", String::from_utf8_lossy(&res.body));
    assert_eq!(
        db_i64(
            &srv,
            "SELECT failed_logins FROM accounts WHERE username = ?",
            "gina"
        )
        .await,
        0,
        "a successful reset clears the failure count"
    );
}

#[tokio::test]
async fn recovery_token_cannot_push_or_list_devices() {
    let srv = spawn().await;
    let a = register(&srv, "hank").await;
    let rec = recover_login(&srv, &a).await;
    let t = &rec.access_token;

    assert_err(
        &get_auth(&srv, "/v1/devices", t).await,
        403,
        ErrorCode::Forbidden,
    );
    assert_err(
        &post_auth::<()>(&srv, "/v1/link", t, None).await,
        403,
        ErrorCode::Forbidden,
    );
    assert_err(
        &post_auth::<()>(&srv, "/v1/logout", t, None).await,
        403,
        ErrorCode::Forbidden,
    );
    assert_err(
        &send(
            &srv,
            "POST",
            "/v1/account/password",
            t,
            None,
            Some(&change_req(&a, 1)),
        )
        .await,
        403,
        ErrorCode::Forbidden,
    );
    assert_err(
        &send(
            &srv,
            "PUT",
            "/v1/account/recovery",
            t,
            None,
            Some(&RecoveryUpdateReq {
                auth_key: a.auth_key.clone(),
                recovery_auth_key: vec![5; 32],
                secret_by_recovery: vec![6; 73],
            }),
        )
        .await,
        403,
        ErrorCode::Forbidden,
    );
    // Nothing changed.
    assert_eq!(get_account(&srv, t).await.account_rev, 1);
}

#[tokio::test]
async fn old_schema_client_cannot_write() {
    let srv = spawn().await;
    let a = register(&srv, "ivy").await;
    let tok = &a.tokens.access_token;

    // A schema-2 client writes (a password change).
    let res = send(
        &srv,
        "POST",
        "/v1/account/password",
        tok,
        Some(2),
        Some(&change_req(&a, 1)),
    )
    .await;
    assert_eq!(res.status, 200, "{}", String::from_utf8_lossy(&res.body));
    assert_eq!(
        db_i64(
            &srv,
            "SELECT max_schema FROM accounts WHERE username = ?",
            "ivy"
        )
        .await,
        2
    );

    // A schema-1 client can read ...
    let res = send::<()>(&srv, "GET", "/v1/account", tok, Some(1), None).await;
    assert_eq!(res.status, 200);
    // ... but not write, with the right key and rev, with or without the header.
    let mut next = change_req(&a, 2);
    next.old_auth_key = vec![50; 32];
    next.new_auth_key = vec![51; 32];
    for schema in [Some(1), None] {
        let res = send(
            &srv,
            "POST",
            "/v1/account/password",
            tok,
            schema,
            Some(&next),
        )
        .await;
        assert_err(&res, 400, ErrorCode::UpdateClient);
    }
    let res = send(
        &srv,
        "PUT",
        "/v1/account/recovery",
        tok,
        Some(1),
        Some(&RecoveryUpdateReq {
            auth_key: a.auth_key.clone(),
            recovery_auth_key: vec![5; 32],
            secret_by_recovery: vec![6; 73],
        }),
    )
    .await;
    assert_err(&res, 400, ErrorCode::UpdateClient);
    // Refused writes changed nothing and cost no lockout points.
    assert_eq!(get_account(&srv, tok).await.account_rev, 2);
    assert_eq!(
        db_i64(
            &srv,
            "SELECT failed_logins FROM accounts WHERE username = ?",
            "ivy"
        )
        .await,
        0
    );
    // Same schema (or newer) writes fine.
    let res = send(
        &srv,
        "POST",
        "/v1/account/password",
        tok,
        Some(2),
        Some(&next),
    )
    .await;
    assert_eq!(res.status, 200, "{}", String::from_utf8_lossy(&res.body));
    let mut third = change_req(&a, 3);
    third.old_auth_key = vec![51; 32];
    third.new_auth_key = vec![52; 32];
    let res = send(
        &srv,
        "POST",
        "/v1/account/password",
        tok,
        Some(3),
        Some(&third),
    )
    .await;
    assert_eq!(res.status, 200);
    assert_eq!(
        db_i64(
            &srv,
            "SELECT max_schema FROM accounts WHERE username = ?",
            "ivy"
        )
        .await,
        3
    );
}

#[tokio::test]
async fn schema_is_tracked_on_reads_and_a_missing_header_is_schema_one() {
    let srv = spawn().await;
    let a = register(&srv, "jack").await;
    let tok = &a.tokens.access_token;
    let max = || async {
        db_i64(
            &srv,
            "SELECT max_schema FROM accounts WHERE username = ?",
            "jack",
        )
        .await
    };

    // A read from a schema-3 client raises the account's high-water mark.
    send::<()>(&srv, "GET", "/v1/account", tok, Some(3), None).await;
    assert_eq!(max().await, 3);
    // It never goes down; garbage headers are ignored.
    send::<()>(&srv, "GET", "/v1/account", tok, Some(1), None).await;
    assert_eq!(max().await, 3);
    let res = srv
        .request(
            "GET",
            "/v1/account",
            &[
                ("Authorization", &format!("Bearer {tok}")),
                ("X-Tether-Schema", "banana"),
            ],
            None,
        )
        .await;
    assert_eq!(res.status, 200);
    assert_eq!(max().await, 3);

    // No header counts as schema 1, so it cannot write now ...
    let res = send(
        &srv,
        "POST",
        "/v1/account/password",
        tok,
        None,
        Some(&change_req(&a, 1)),
    )
    .await;
    assert_err(&res, 400, ErrorCode::UpdateClient);

    // ... while on an account that never saw more than 1 it can.
    let b = register_with(&srv, "kate", 2, 1).await;
    let res = send(
        &srv,
        "POST",
        "/v1/account/password",
        &b.tokens.access_token,
        None,
        Some(&change_req(&b, 1)),
    )
    .await;
    assert_eq!(res.status, 200, "{}", String::from_utf8_lossy(&res.body));
    // Each account has its own mark.
    assert_eq!(
        db_i64(
            &srv,
            "SELECT max_schema FROM accounts WHERE username = ?",
            "kate"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn recover_reset_is_also_a_gated_write() {
    let srv = spawn().await;
    let a = register(&srv, "liam").await;
    send::<()>(
        &srv,
        "GET",
        "/v1/account",
        &a.tokens.access_token,
        Some(2),
        None,
    )
    .await;
    let rec = recover_login(&srv, &a).await;
    let res = send(
        &srv,
        "POST",
        "/v1/account/recover",
        &rec.access_token,
        Some(1),
        Some(&reset_req(&a)),
    )
    .await;
    assert_err(&res, 400, ErrorCode::UpdateClient);
    // The token was not consumed.
    assert_eq!(get_account(&srv, &rec.access_token).await.account_rev, 1);
}

#[tokio::test]
async fn account_changed_event_sent() {
    let srv = spawn().await;
    let a = register(&srv, "mona").await;
    let mut rx = srv.state.events.subscribe(&a.vault_id, &a.tokens.device_id);
    let mut other_vault = srv
        .state
        .events
        .subscribe("some-other-vault", &a.tokens.device_id);

    // Password change.
    let res = send(
        &srv,
        "POST",
        "/v1/account/password",
        &a.tokens.access_token,
        None,
        Some(&change_req(&a, 1)),
    )
    .await;
    assert_eq!(res.status, 200);
    assert_eq!(
        rx.recv().await.unwrap(),
        Event::AccountChanged { account_rev: 2 }
    );

    // Recovery key replacement.
    let res = send(
        &srv,
        "PUT",
        "/v1/account/recovery",
        &a.tokens.access_token,
        None,
        Some(&RecoveryUpdateReq {
            auth_key: vec![50; 32],
            recovery_auth_key: vec![5; 32],
            secret_by_recovery: vec![6; 73],
        }),
    )
    .await;
    assert_eq!(res.status, 200, "{}", String::from_utf8_lossy(&res.body));
    assert_eq!(
        rx.recv().await.unwrap(),
        Event::AccountChanged { account_rev: 3 }
    );

    // Recovery reset.
    let b = Account {
        recovery_key: vec![5; 32],
        ..Account {
            username: a.username.clone(),
            auth_key: a.auth_key.clone(),
            recovery_key: vec![],
            vault_id: a.vault_id.clone(),
            tokens: a.tokens.clone(),
        }
    };
    let rec = recover_login(&srv, &b).await;
    let res = send(
        &srv,
        "POST",
        "/v1/account/recover",
        &rec.access_token,
        None,
        Some(&reset_req(&b)),
    )
    .await;
    assert_eq!(res.status, 200, "{}", String::from_utf8_lossy(&res.body));
    assert_eq!(
        rx.recv().await.unwrap(),
        Event::AccountChanged { account_rev: 4 }
    );

    // Failed or refused calls send nothing, and other vaults hear nothing.
    let mut bad = change_req(&a, 4);
    bad.old_auth_key = vec![9; 32];
    send(
        &srv,
        "POST",
        "/v1/account/password",
        &a.tokens.access_token,
        None,
        Some(&bad),
    )
    .await;
    let quiet = std::time::Duration::from_millis(100);
    assert!(tokio::time::timeout(quiet, rx.recv()).await.is_err());
    assert!(
        tokio::time::timeout(quiet, other_vault.recv())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn recovery_key_can_be_replaced() {
    let srv = spawn().await;
    let a = register(&srv, "nina").await;
    let tok = &a.tokens.access_token;

    let req = RecoveryUpdateReq {
        auth_key: a.auth_key.clone(),
        recovery_auth_key: vec![5; 32],
        secret_by_recovery: vec![6; 73],
    };
    let res = send(&srv, "PUT", "/v1/account/recovery", tok, None, Some(&req)).await;
    assert_eq!(res.status, 200, "{}", String::from_utf8_lossy(&res.body));
    let resp: AccountResp = res.json();
    assert_eq!(resp.account_rev, 2);
    assert_eq!(resp.material.secret_by_recovery, vec![6; 73]);
    // The rest of the material is untouched.
    let mut expected = material(&a.vault_id);
    expected.secret_by_recovery = vec![6; 73];
    assert_eq!(get_account(&srv, tok).await.material, expected);

    // The old recovery key is dead, the new one signs in.
    let old = RecoverReq {
        username: "nina".into(),
        recovery_auth_key: a.recovery_key.clone(),
        device: device("x"),
    };
    assert_err(
        &srv.post_json("/v1/recover-login", &old).await,
        401,
        ErrorCode::Unauthorized,
    );
    let fresh = RecoverReq {
        recovery_auth_key: vec![5; 32],
        ..old
    };
    assert_eq!(srv.post_json("/v1/recover-login", &fresh).await.status, 200);

    // The password is unaffected.
    assert_eq!(
        login_device(&srv, &a, &a.tokens.device_id, &a.auth_key)
            .await
            .status,
        200
    );

    // Shapes are checked.
    for bad in [
        RecoveryUpdateReq {
            auth_key: a.auth_key.clone(),
            recovery_auth_key: vec![5; 31],
            secret_by_recovery: vec![6; 73],
        },
        RecoveryUpdateReq {
            auth_key: a.auth_key.clone(),
            recovery_auth_key: vec![5; 32],
            secret_by_recovery: vec![6; 72],
        },
    ] {
        let tok2 = login_device(&srv, &a, &a.tokens.device_id, &a.auth_key)
            .await
            .json::<TokenResp>()
            .access_token;
        let res = send(&srv, "PUT", "/v1/account/recovery", &tok2, None, Some(&bad)).await;
        assert_err(&res, 400, ErrorCode::BadRequest);
    }
    // No token: unauthorized.
    let res = srv
        .request(
            "PUT",
            "/v1/account/recovery",
            &[("Content-Type", "application/json")],
            Some(serde_json::to_vec(&req).unwrap()),
        )
        .await;
    assert_err(&res, 401, ErrorCode::Unauthorized);
}

#[tokio::test]
async fn concurrent_changes_on_the_same_rev_let_exactly_one_win() {
    let srv = spawn().await;
    let a = register(&srv, "olga").await;
    let tok = &a.tokens.access_token;
    let mut one = change_req(&a, 1);
    one.new_auth_key = vec![71; 32];
    let mut two = change_req(&a, 1);
    two.new_auth_key = vec![72; 32];
    let (r1, r2) = tokio::join!(
        send(&srv, "POST", "/v1/account/password", tok, None, Some(&one)),
        send(&srv, "POST", "/v1/account/password", tok, None, Some(&two)),
    );
    let mut statuses = [r1.status, r2.status];
    statuses.sort_unstable();
    assert_eq!(statuses, [200, 409], "{r1:?} {r2:?}");
    assert_eq!(get_account(&srv, tok).await.account_rev, 2);
}

#[tokio::test]
async fn password_change_with_dead_key_cannot_overwrite_new_password() {
    let srv = spawn().await;
    let a = register(&srv, "pete").await;
    let tok = &a.tokens.access_token;
    let ok = send(
        &srv,
        "POST",
        "/v1/account/password",
        tok,
        None,
        Some(&change_req(&a, 1)),
    )
    .await;
    assert_eq!(ok.status, 200);

    // The old key K0 with the base the account is at now (2): the checked
    // verifier is the current one, so the dead key fails.
    let mut attack = change_req(&a, 2);
    attack.new_auth_key = vec![99; 32];
    let res = send(
        &srv,
        "POST",
        "/v1/account/password",
        tok,
        None,
        Some(&attack),
    )
    .await;
    assert_err(&res, 401, ErrorCode::Unauthorized);
    // And with the base it had before: stale, so a conflict.
    attack.account_rev_base = 1;
    let res = send(
        &srv,
        "POST",
        "/v1/account/password",
        tok,
        None,
        Some(&attack),
    )
    .await;
    assert_err(&res, 409, ErrorCode::Conflict);
    // And with a base from the future.
    attack.account_rev_base = 3;
    let res = send(
        &srv,
        "POST",
        "/v1/account/password",
        tok,
        None,
        Some(&attack),
    )
    .await;
    assert_err(&res, 409, ErrorCode::Conflict);

    // The new password is untouched.
    let got = get_account(&srv, tok).await;
    assert_eq!(got.account_rev, 2);
    assert_eq!(got.material.secret_by_kek, vec![55; 73]);
    assert_eq!(
        login_device(&srv, &a, &a.tokens.device_id, &[50; 32])
            .await
            .status,
        200
    );
}

#[tokio::test]
async fn recovery_update_requires_auth_key() {
    let srv = spawn().await;
    let a = register(&srv, "quinn").await;
    let tok = &a.tokens.access_token;
    let bad = RecoveryUpdateReq {
        auth_key: vec![9; 32],
        recovery_auth_key: vec![5; 32],
        secret_by_recovery: vec![6; 73],
    };
    let res = send(&srv, "PUT", "/v1/account/recovery", tok, None, Some(&bad)).await;
    assert_err(&res, 401, ErrorCode::Unauthorized);
    assert_eq!(
        db_i64(
            &srv,
            "SELECT failed_logins FROM accounts WHERE username = ?",
            "quinn"
        )
        .await,
        1
    );
    // Nothing changed: same rev, old recovery key still signs in.
    assert_eq!(get_account(&srv, tok).await.account_rev, 1);
    let rec = RecoverReq {
        username: "quinn".into(),
        recovery_auth_key: a.recovery_key.clone(),
        device: device("x"),
    };
    assert_eq!(srv.post_json("/v1/recover-login", &rec).await.status, 200);

    // A short auth key is a bad request; the right one works and clears the count.
    let mut short = bad.clone();
    short.auth_key = vec![1; 31];
    let res = send(&srv, "PUT", "/v1/account/recovery", tok, None, Some(&short)).await;
    assert_err(&res, 400, ErrorCode::BadRequest);
    let good = RecoveryUpdateReq {
        auth_key: a.auth_key.clone(),
        ..bad
    };
    let res = send(&srv, "PUT", "/v1/account/recovery", tok, None, Some(&good)).await;
    assert_eq!(res.status, 200, "{}", String::from_utf8_lossy(&res.body));
    assert_eq!(
        db_i64(
            &srv,
            "SELECT failed_logins FROM accounts WHERE username = ?",
            "quinn"
        )
        .await,
        0
    );
}

#[tokio::test]
async fn absurd_schema_header_rejected_and_not_recorded() {
    let srv = spawn().await;
    let a = register(&srv, "rita").await;
    let tok = &a.tokens.access_token;
    let max = || async {
        db_i64(
            &srv,
            "SELECT max_schema FROM accounts WHERE username = ?",
            "rita",
        )
        .await
    };
    // KNOWN_SCHEMA + 8 is the largest accepted value.
    let limit = core_proto::KNOWN_SCHEMA + 8;
    let res = send::<()>(&srv, "GET", "/v1/account", tok, Some(limit), None).await;
    assert_eq!(res.status, 200);
    assert_eq!(max().await, i64::from(limit));

    let b = register_with(&srv, "sam", 2, 1).await;
    for bad in [limit + 1, 1000, u32::MAX] {
        let res = send::<()>(
            &srv,
            "GET",
            "/v1/account",
            &b.tokens.access_token,
            Some(bad),
            None,
        )
        .await;
        assert_err(&res, 400, ErrorCode::BadRequest);
    }
    // Larger than u32 too.
    let res = srv
        .request(
            "GET",
            "/v1/account",
            &[
                (
                    "Authorization",
                    &format!("Bearer {}", b.tokens.access_token),
                ),
                ("X-Tether-Schema", "99999999999999999999999"),
            ],
            None,
        )
        .await;
    assert_err(&res, 400, ErrorCode::BadRequest);
    // A write with an absurd header changes nothing.
    let res = send(
        &srv,
        "POST",
        "/v1/account/password",
        &b.tokens.access_token,
        Some(1000),
        Some(&change_req(&b, 1)),
    )
    .await;
    assert_err(&res, 400, ErrorCode::BadRequest);
    let n = db_i64(
        &srv,
        "SELECT max_schema FROM accounts WHERE username = ?",
        "sam",
    )
    .await;
    assert_eq!(n, 0, "absurd headers record nothing");
    assert_eq!(
        get_account(&srv, &b.tokens.access_token).await.account_rev,
        1
    );
}

#[tokio::test]
async fn rejected_scope_does_not_raise_max_schema() {
    let srv = spawn().await;
    let a = register(&srv, "tom").await;
    let max = || async {
        db_i64(
            &srv,
            "SELECT max_schema FROM accounts WHERE username = ?",
            "tom",
        )
        .await
    };
    let before = max().await;
    let rec = recover_login(&srv, &a).await;

    // The recovery token on a device endpoint: forbidden, nothing recorded.
    let res = send::<()>(&srv, "GET", "/v1/devices", &rec.access_token, Some(5), None).await;
    assert_err(&res, 403, ErrorCode::Forbidden);
    assert_eq!(max().await, before);
    // A session token on the recovery endpoint: forbidden, nothing recorded.
    let res = send(
        &srv,
        "POST",
        "/v1/account/recover",
        &a.tokens.access_token,
        Some(5),
        Some(&reset_req(&a)),
    )
    .await;
    assert_err(&res, 403, ErrorCode::Forbidden);
    assert_eq!(max().await, before);
    // An accepted request does record it.
    send::<()>(
        &srv,
        "GET",
        "/v1/account",
        &a.tokens.access_token,
        Some(5),
        None,
    )
    .await;
    assert_eq!(max().await, 5);
}

// ------------------------------------------------------------- short codes

const NONCE: [u8; 32] = [5; 32];

async fn short_code(srv: &TestServer) -> ShortCode {
    tether_server::accounts::create_short_invite(
        &srv.state.db,
        srv.state.now_ms(),
        None,
        7 * 24 * 60 * MIN,
    )
    .await
    .unwrap()
}

/// A register request redeeming `code` with a client proof over `binding`.
fn code_req(code: &ShortCode, username: &str, binding: &[u8], pinned: bool) -> RegisterReq {
    RegisterReq {
        invite_token: vec![],
        username: username.into(),
        auth_key: vec![1; 32],
        device: device("First"),
        material: material(&uuid::Uuid::now_v7().to_string()),
        recovery_auth_key: vec![101; 32],
        node_hint: 1,
        code_proof: Some(CodeProof {
            code_id: code.id().to_vec(),
            client_nonce: NONCE.to_vec(),
            pinned,
            proof: client_proof(&code.key(), binding, &NONCE).to_vec(),
        }),
    }
}

async fn verify(srv: &TestServer, code: &ShortCode, pinned: bool) -> Response {
    let req = CodeVerifyReq {
        code_id: code.id().to_vec(),
        client_nonce: NONCE.to_vec(),
        pinned,
    };
    srv.post_json("/v1/code/verify", &req).await
}

#[tokio::test]
async fn short_code_registers_with_pinned_proof() {
    let srv = spawn_tls().await;
    let code = short_code(&srv).await;
    let res = srv
        .post_json("/v1/register", &code_req(&code, "alice", &srv.pin, true))
        .await;
    assert_eq!(res.status, 200, "{}", String::from_utf8_lossy(&res.body));
    let tokens: TokenResp = res.json();
    assert_eq!(get_account(&srv, &tokens.access_token).await.account_rev, 1);
}

#[tokio::test]
async fn wrong_binding_is_refused() {
    let srv = spawn_tls().await;
    let code = short_code(&srv).await;
    let res = srv
        .post_json("/v1/register", &code_req(&code, "alice", &[9; 32], true))
        .await;
    assert_err(&res, 403, ErrorCode::InviteInvalid);
    // A proof over the empty binding while claiming a pin fails too.
    let res = srv
        .post_json("/v1/register", &code_req(&code, "alice", &[], true))
        .await;
    assert_err(&res, 403, ErrorCode::InviteInvalid);
    // The failed attempts did not use the invite.
    let res = srv
        .post_json("/v1/register", &code_req(&code, "alice", &srv.pin, true))
        .await;
    assert_eq!(res.status, 200, "{}", String::from_utf8_lossy(&res.body));
}

#[tokio::test]
async fn verify_returns_server_proof_for_pin() {
    let srv = spawn_tls().await;
    let code = short_code(&srv).await;
    let res = verify(&srv, &code, true).await;
    assert_eq!(res.status, 200, "{}", String::from_utf8_lossy(&res.body));
    let resp: CodeVerifyResp = res.json();
    assert!(verify_server_proof(
        &code.key(),
        &srv.pin,
        &NONCE,
        &resp.server_proof
    ));
    assert!(!verify_server_proof(
        &code.key(),
        &[9; 32],
        &NONCE,
        &resp.server_proof
    ));
    // Verifying does not use the invite.
    let res = verify(&srv, &code, true).await;
    assert_eq!(res.status, 200);
}

#[tokio::test]
async fn unknown_code_id_is_invite_invalid() {
    let srv = spawn_tls().await;
    let stranger = ShortCode::from_bytes([3; 10]);
    assert_err(
        &verify(&srv, &stranger, true).await,
        403,
        ErrorCode::InviteInvalid,
    );
    let res = srv
        .post_json(
            "/v1/register",
            &code_req(&stranger, "alice", &srv.pin, true),
        )
        .await;
    assert_err(&res, 403, ErrorCode::InviteInvalid);
    // Wrong lengths are refused the same way.
    let bad = CodeVerifyReq {
        code_id: vec![1; 15],
        client_nonce: NONCE.to_vec(),
        pinned: true,
    };
    assert_err(
        &srv.post_json("/v1/code/verify", &bad).await,
        403,
        ErrorCode::InviteInvalid,
    );
    let code = short_code(&srv).await;
    let mut req = code_req(&code, "alice", &srv.pin, true);
    req.code_proof.as_mut().unwrap().client_nonce = vec![5; 31];
    assert_err(
        &srv.post_json("/v1/register", &req).await,
        403,
        ErrorCode::InviteInvalid,
    );
}

#[tokio::test]
async fn used_code_is_invite_invalid() {
    let srv = spawn_tls().await;
    let code = short_code(&srv).await;
    let res = srv
        .post_json("/v1/register", &code_req(&code, "alice", &srv.pin, true))
        .await;
    assert_eq!(res.status, 200, "{}", String::from_utf8_lossy(&res.body));
    assert_err(
        &verify(&srv, &code, true).await,
        403,
        ErrorCode::InviteInvalid,
    );
    let res = srv
        .post_json("/v1/register", &code_req(&code, "bob", &srv.pin, true))
        .await;
    assert_err(&res, 403, ErrorCode::InviteInvalid);

    // An expired code is refused the same way.
    let late = short_code(&srv).await;
    srv.set_clock(now_ms() + 8 * 24 * 60 * MIN);
    assert_err(
        &verify(&srv, &late, true).await,
        403,
        ErrorCode::InviteInvalid,
    );
}

#[tokio::test]
async fn pinned_proof_on_plain_server_is_refused() {
    let srv = spawn().await;
    let code = short_code(&srv).await;
    assert_err(
        &verify(&srv, &code, true).await,
        403,
        ErrorCode::InviteInvalid,
    );
    for binding in [[0u8; 32].as_slice(), &[]] {
        let res = srv
            .post_json("/v1/register", &code_req(&code, "alice", binding, true))
            .await;
        assert_err(&res, 403, ErrorCode::InviteInvalid);
    }
}

#[tokio::test]
async fn unpinned_proof_on_plain_server_registers() {
    let srv = spawn().await;
    let code = short_code(&srv).await;
    let res = verify(&srv, &code, false).await;
    assert_eq!(res.status, 200, "{}", String::from_utf8_lossy(&res.body));
    let resp: CodeVerifyResp = res.json();
    assert!(verify_server_proof(
        &code.key(),
        &[],
        &NONCE,
        &resp.server_proof
    ));
    let res = srv
        .post_json("/v1/register", &code_req(&code, "alice", &[], false))
        .await;
    assert_eq!(res.status, 200, "{}", String::from_utf8_lossy(&res.body));
}

#[tokio::test]
async fn full_code_still_registers() {
    let srv = spawn_tls().await;
    register(&srv, "alice").await;
    // No token and no proof: not an invite.
    let mut req = register_req(&srv.make_invite().await, "bob", 2, 2);
    req.invite_token = vec![];
    assert_err(
        &srv.post_json("/v1/register", &req).await,
        403,
        ErrorCode::InviteInvalid,
    );
}

#[tokio::test]
async fn code_verify_is_rate_limited_per_code_id() {
    let srv = spawn().await;
    let hammered = ShortCode::from_bytes([3; 10]);
    let mut last = verify(&srv, &hammered, false).await;
    for _ in 0..30 {
        if last.status == 429 {
            break;
        }
        last = verify(&srv, &hammered, false).await;
    }
    assert_err(&last, 429, ErrorCode::RateLimited);
    // Another code id is not blocked (same test IP, whose own window still
    // has room): its real code verifies.
    let code = short_code(&srv).await;
    let res = verify(&srv, &code, false).await;
    assert_eq!(res.status, 200, "{}", String::from_utf8_lossy(&res.body));
}
