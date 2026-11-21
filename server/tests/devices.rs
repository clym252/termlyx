//! `GET/PATCH/DELETE /v1/devices`.

mod common;

use common::*;
use core_proto::{DeviceView, ErrorCode, RefreshReq, RenameDeviceReq};

async fn list(srv: &TestServer, token: &str) -> Vec<DeviceView> {
    let res = get_auth(srv, "/v1/devices", token).await;
    assert_eq!(res.status, 200, "{}", String::from_utf8_lossy(&res.body));
    res.json()
}

#[tokio::test]
async fn devices_list_marks_current() {
    let srv = spawn().await;
    let acct = register(&srv, "alice").await;
    let second = add_device(&srv, &acct, "Laptop").await;
    // Another account's devices never show up.
    register_with(&srv, "mallory", 2, 1).await;

    let from_first = list(&srv, &acct.tokens.access_token).await;
    assert_eq!(from_first.len(), 2);
    let cur: Vec<_> = from_first.iter().filter(|d| d.current).collect();
    assert_eq!(cur.len(), 1);
    assert_eq!(cur[0].id, acct.tokens.device_id);
    assert_eq!(
        (cur[0].name.as_str(), cur[0].platform.as_str()),
        ("First", "macos")
    );
    assert!(cur[0].created_at > 0 && cur[0].last_seen >= cur[0].created_at);

    let from_second = list(&srv, &second.access_token).await;
    let cur: Vec<_> = from_second.iter().filter(|d| d.current).collect();
    assert_eq!(cur.len(), 1);
    assert_eq!(cur[0].id, second.device_id);
    assert_eq!(cur[0].name, "Laptop");
}

#[tokio::test]
async fn rename_device() {
    let srv = spawn().await;
    let acct = register(&srv, "alice").await;
    let second = add_device(&srv, &acct, "Laptop").await;
    let tok = &acct.tokens.access_token;

    // Any of the account's devices can rename any other (or itself).
    let body = RenameDeviceReq {
        name: "  Work laptop ".into(),
    };
    let path = format!("/v1/devices/{}", second.device_id);
    let res = call(&srv, "PATCH", &path, Some(tok), None, Some(&body)).await;
    assert_eq!(res.status, 204, "{}", String::from_utf8_lossy(&res.body));
    let names: Vec<_> = list(&srv, tok)
        .await
        .into_iter()
        .map(|d| (d.id, d.name))
        .collect();
    assert!(names.contains(&(second.device_id.clone(), "Work laptop".into())));

    for bad in ["", "   ", &"x".repeat(101), "tab\there"] {
        let res = call(
            &srv,
            "PATCH",
            &path,
            Some(tok),
            None,
            Some(&RenameDeviceReq { name: bad.into() }),
        )
        .await;
        assert_err(&res, 400, ErrorCode::BadRequest);
    }
    // Needs a bearer; unknown ids are 404.
    let res = call(&srv, "PATCH", &path, None, None, Some(&body)).await;
    assert_err(&res, 401, ErrorCode::Unauthorized);
    let res = call(
        &srv,
        "PATCH",
        "/v1/devices/nope",
        Some(tok),
        None,
        Some(&body),
    )
    .await;
    assert_err(&res, 404, ErrorCode::NotFound);
}

#[tokio::test]
async fn revoke_kills_refresh_and_access() {
    let srv = spawn().await;
    let acct = register(&srv, "alice").await;
    let second = add_device(&srv, &acct, "Laptop").await;

    let res = delete_auth(
        &srv,
        &format!("/v1/devices/{}", second.device_id),
        &acct.tokens.access_token,
    )
    .await;
    assert_eq!(res.status, 204, "{}", String::from_utf8_lossy(&res.body));

    // The revoked device's next call of any kind is 401 "device revoked".
    assert_err(
        &get_auth(&srv, "/v1/devices", &second.access_token).await,
        401,
        ErrorCode::Revoked,
    );
    let refresh = srv
        .post_json(
            "/v1/refresh",
            &RefreshReq {
                refresh_token: second.refresh_token.clone(),
            },
        )
        .await;
    assert_err(&refresh, 401, ErrorCode::Revoked);
    assert_eq!(error_of(&refresh).message, "device revoked");
    assert_err(
        &post_auth::<()>(&srv, "/v1/link", &second.access_token, None).await,
        401,
        ErrorCode::Revoked,
    );
    // Even the AuthKey does not bring it back.
    assert_err(
        &login_device_with(
            &srv,
            "alice",
            &second.device_id,
            Some(&second.device_secret),
            &acct.auth_key,
        )
        .await,
        401,
        ErrorCode::Revoked,
    );

    // The other device is unaffected and no longer lists it.
    let rest = list(&srv, &acct.tokens.access_token).await;
    assert_eq!(rest.len(), 1);
    assert_eq!(rest[0].id, acct.tokens.device_id);
    // Revoking again is harmless.
    let res = delete_auth(
        &srv,
        &format!("/v1/devices/{}", second.device_id),
        &acct.tokens.access_token,
    )
    .await;
    assert_eq!(res.status, 204);
}

#[tokio::test]
async fn a_device_can_revoke_itself() {
    let srv = spawn().await;
    let acct = register(&srv, "alice").await;
    let tok = &acct.tokens;
    let res = delete_auth(
        &srv,
        &format!("/v1/devices/{}", tok.device_id),
        &tok.access_token,
    )
    .await;
    assert_eq!(res.status, 204);
    assert_err(
        &get_auth(&srv, "/v1/devices", &tok.access_token).await,
        401,
        ErrorCode::Revoked,
    );
}

#[tokio::test]
async fn cannot_revoke_other_accounts_device() {
    let srv = spawn().await;
    let alice = register(&srv, "alice").await;
    let mallory = register_with(&srv, "mallory", 2, 1).await;

    let path = format!("/v1/devices/{}", alice.tokens.device_id);
    let res = delete_auth(&srv, &path, &mallory.tokens.access_token).await;
    assert_err(&res, 404, ErrorCode::NotFound);
    let res = call(
        &srv,
        "PATCH",
        &path,
        Some(&mallory.tokens.access_token),
        None,
        Some(&RenameDeviceReq {
            name: "pwned".into(),
        }),
    )
    .await;
    assert_err(&res, 404, ErrorCode::NotFound);

    // Alice's device is untouched.
    let mine = list(&srv, &alice.tokens.access_token).await;
    assert_eq!(mine.len(), 1);
    assert_eq!(mine[0].name, "First");
}
