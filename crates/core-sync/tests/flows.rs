//! Account flows against an in-process TLS server: enable sync on an
//! existing vault, join with an add-device code, password change, recovery
//! and turning sync off.

mod common;

use std::collections::BTreeSet;

use common::*;
use core_api::vault::VaultError;
use core_model::{Host, Identity};
use core_proto::{
    CodeKind, CodeVerifyReq, DeviceInfo, ErrorBody, ErrorCode, RecoverReq, ShortCode,
};
use core_sync::flows::{self, RecoveryChoice};
use core_sync::{Client, Pin, SyncConfig, SyncError, SyncNotice, TokenStore, parse_join_code, tls};
use secrecy::{ExposeSecret, SecretString};
use tether_server::testing::spawn_tls;

fn info() -> DeviceInfo {
    DeviceInfo {
        name: "probe".into(),
        platform: "test".into(),
    }
}

/// Whether the server accepts `recovery` for the account (recover-login).
async fn server_accepts_recovery(dev: &Dev, recovery: &str) -> bool {
    let cfg = dev.with(|v| SyncConfig::load(v).unwrap().unwrap());
    let key = core_crypto::parse_recovery_key(recovery).unwrap();
    let auth = core_crypto::recovery_auth_key(key.expose_secret());
    Client::new(&cfg.url, cfg.pin)
        .unwrap()
        .recover_login(&RecoverReq {
            username: USER.into(),
            recovery_auth_key: auth.expose_secret().to_vec(),
            device: info(),
        })
        .await
        .is_ok()
}

#[tokio::test(flavor = "multi_thread")]
async fn enable_sync_uploads_existing_vault() {
    let srv = spawn_tls().await;
    let dir = tempfile::tempdir().unwrap();
    let mut a = local_device(dir.path(), "a");
    a.with(|v| {
        for i in 0..3 {
            v.upsert(host(&format!("h{i}"))).unwrap();
        }
        v.upsert(identity("s3cret")).unwrap();
        v.upsert(identity("other")).unwrap();
    });
    let node = a.with(|v| v.node().unwrap());
    let local: BTreeSet<String> = a.with(|v| {
        let mut ids: Vec<String> = v
            .list::<Host>()
            .unwrap()
            .iter()
            .map(|h| h.id.to_string())
            .collect();
        ids.extend(
            v.list::<Identity>()
                .unwrap()
                .iter()
                .map(|i| i.id.to_string()),
        );
        ids.into_iter().collect()
    });

    enable(&srv, &mut a).await;

    let remote: BTreeSet<String> = server_rows(&a).await.into_iter().map(|r| r.0).collect();
    assert_eq!(remote, local, "same rows, same ids");
    assert_eq!(a.with(|v| v.outbox_len().unwrap()), 0);
    assert_eq!(
        a.with(|v| v.node().unwrap()),
        node,
        "first device keeps its node"
    );
    a.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn enable_with_existing_recovery_key() {
    let srv = spawn_tls().await;
    let dir = tempfile::tempdir().unwrap();
    let a = local_device(dir.path(), "a");
    let code = parse_join_code(&srv.make_invite().await).unwrap();

    // A wrong recovery key is caught locally; the invite is not used up.
    let (_, other) = core_crypto::new_recovery_key();
    let err = flows::enable_with_invite(
        &a.vault,
        a.tokens(),
        &code,
        USER,
        pw(PW),
        RecoveryChoice::Existing(SecretString::from(other)),
    )
    .await
    .unwrap_err();
    assert_eq!(err, SyncError::WrongRecoveryKey);
    let err = flows::enable_with_invite(
        &a.vault,
        a.tokens(),
        &code,
        USER,
        pw("not the password"),
        RecoveryChoice::Existing(SecretString::from(a.recovery.clone())),
    )
    .await
    .unwrap_err();
    assert_eq!(err, SyncError::WrongPassword);
    assert!(
        core_sync::start(a.vault.clone(), a.tokens()).is_none(),
        "still off"
    );

    let before = a.with(|v| v.account_material().unwrap());
    let result = flows::enable_with_invite(
        &a.vault,
        a.tokens(),
        &code,
        USER,
        pw(PW),
        RecoveryChoice::Existing(SecretString::from(a.recovery.clone())),
    )
    .await
    .unwrap();
    assert!(result.new_recovery_text.is_none());
    assert!(
        a.with(|v| v.account_material().unwrap()) == before,
        "nothing re-wrapped"
    );
    let recovery = a.recovery.clone();
    assert!(server_accepts_recovery(&a, &recovery).await);
}

#[tokio::test(flavor = "multi_thread")]
async fn enable_with_new_recovery_key_replaces_recovery_wrap() {
    let srv = spawn_tls().await;
    let dir = tempfile::tempdir().unwrap();
    let a = local_device(dir.path(), "a");
    let code = parse_join_code(&srv.make_invite().await).unwrap();
    let result = flows::enable_with_invite(
        &a.vault,
        a.tokens(),
        &code,
        USER,
        pw(PW),
        RecoveryChoice::New,
    )
    .await
    .unwrap();
    let debug = format!("{result:?}");
    let text = result
        .new_recovery_text
        .expect("a new recovery key is shown once")
        .expose_secret()
        .to_owned();
    assert!(!debug.contains(&text), "recovery key in Debug: {debug}");
    assert!(debug.contains("REDACTED"), "{debug}");
    assert_ne!(text, a.recovery);
    assert!(matches!(
        a.with(|v| v.recovery_auth(&a.recovery)),
        Err(VaultError::WrongRecoveryKey)
    ));
    a.with(|v| v.recovery_auth(&text)).unwrap();
    assert!(server_accepts_recovery(&a, &text).await);
    assert!(!server_accepts_recovery(&a, &a.recovery.clone()).await);
}

#[tokio::test(flavor = "multi_thread")]
async fn join_with_link_code_downloads_vault_and_opens_with_password() {
    let srv = spawn_tls().await;
    let dir = tempfile::tempdir().unwrap();
    let mut a = local_device(dir.path(), "a");
    a.with(|v| {
        v.upsert(host("web")).unwrap();
        v.upsert(identity("pw")).unwrap();
    });
    enable(&srv, &mut a).await;

    // A wrong password is "wrong password", not a server message.
    let (_, _, expires) = flows::create_link_code(a.handle(), None).await.unwrap();
    assert!(expires > 0);
    let code = link_code(&srv, &a).await;
    let b0 = blank_device(dir.path(), "b0");
    let err = flows::join_with_code(&b0.vault, b0.tokens(), &code, USER, pw("nope"), false)
        .await
        .unwrap_err();
    assert_eq!(err, SyncError::WrongPassword);
    assert!(!b0.with(|v| v.is_initialized()));

    let mut b = join(&srv, dir.path(), "b", &a).await;
    assert_eq!(b.hosts(), a.hosts());
    assert_eq!(
        b.with(|v| v.list::<Identity>().unwrap()[0]
            .password
            .as_ref()
            .unwrap()
            .expose_secret()
            .to_owned()),
        "pw"
    );
    assert_ne!(b.with(|v| v.node().unwrap()), a.with(|v| v.node().unwrap()));
    b.with(|v| {
        v.lock();
        assert!(matches!(
            v.unlock(pw("nope")),
            Err(VaultError::WrongPassword)
        ));
        v.unlock(pw(PW)).unwrap();
    });
    a.stop().await;
    b.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn join_rejects_invite_code() {
    let srv = spawn_tls().await;
    let dir = tempfile::tempdir().unwrap();
    let b = blank_device(dir.path(), "b");
    let code = parse_join_code(&srv.make_invite().await).unwrap();
    let err = flows::join_with_code(&b.vault, b.tokens(), &code, USER, pw(PW), false)
        .await
        .unwrap_err();
    assert!(matches!(err, SyncError::InvalidCode(_)), "{err:?}");
    assert!(!b.with(|v| v.is_initialized()));

    // And the other way round: enabling needs an invite, not a link code.
    let mut a = local_device(dir.path(), "a");
    enable(&srv, &mut a).await;
    let (link, _, _) = flows::create_link_code(a.handle(), None).await.unwrap();
    let c = local_device(dir.path(), "c");
    let err = flows::enable_with_invite(
        &c.vault,
        c.tokens(),
        &parse_join_code(&link).unwrap(),
        "carol",
        pw(PW),
        RecoveryChoice::New,
    )
    .await
    .unwrap_err();
    assert!(matches!(err.error, SyncError::InvalidCode(_)), "{err:?}");
    assert!(err.new_recovery_text.is_none());
    a.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn join_replace_existing_keeps_backup_file() {
    let srv = spawn_tls().await;
    let dir = tempfile::tempdir().unwrap();
    let mut a = local_device(dir.path(), "a");
    a.with(|v| v.upsert(host("synced")).unwrap());
    enable(&srv, &mut a).await;

    let mut b = local_device(dir.path(), "b");
    b.with(|v| v.upsert(host("only on b")).unwrap());
    let err = flows::join_with_code(
        &b.vault,
        b.tokens(),
        &link_code(&srv, &a).await,
        USER,
        pw(PW),
        false,
    )
    .await
    .unwrap_err();
    assert_eq!(err, SyncError::VaultExists);
    assert_eq!(b.hosts()[0].label, "only on b");
    // The refused join leaves no device behind on the server.
    assert_eq!(a.handle().client().devices().await.unwrap().len(), 1);

    let joined = flows::join_with_code(
        &b.vault,
        b.tokens(),
        &link_code(&srv, &a).await,
        USER,
        pw(PW),
        true,
    )
    .await
    .unwrap();
    assert!(!joined.reconnected);
    let backup = joined.backup.expect("old vault kept");
    assert!(backup.exists());
    assert!(
        backup
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("b.db.before-join-")
    );
    b.start();
    b.settle().await;
    assert_eq!(b.hosts(), a.hosts());

    let mut old = core_api::vault::VaultService::open_for_tests(
        &backup,
        Box::new(core_api::vault::MemoryKeyStore::default()),
    )
    .unwrap();
    old.unlock(pw(PW)).unwrap();
    assert_eq!(old.list::<Host>().unwrap()[0].label, "only on b");
    a.stop().await;
    b.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn password_change_propagates_and_old_password_fails() {
    let srv = spawn_tls().await;
    let dir = tempfile::tempdir().unwrap();
    let mut a = local_device(dir.path(), "a");
    enable(&srv, &mut a).await;
    let b = join(&srv, dir.path(), "b", &a).await;
    let c = join(&srv, dir.path(), "c", &a).await;
    c.with(|v| v.lock());
    let mut notices = b.handle().notices();
    let rev = a.with(|v| v.account_rev().unwrap().unwrap());

    flows::change_password_synced(&a.vault, a.handle(), pw("wrong"), pw(NEW_PW))
        .await
        .map(|_| ())
        .expect_err("wrong current password");
    flows::change_password_synced(&a.vault, a.handle(), pw(PW), pw(NEW_PW))
        .await
        .unwrap();
    assert!(
        a.with(|v| v.is_unlocked()),
        "the changing device stays unlocked"
    );
    assert_eq!(a.with(|v| v.account_rev().unwrap()), Some(rev + 1));

    // B was unlocked: it locks and says why.
    let notice = tokio::time::timeout(WAIT, notices.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(notice, SyncNotice::PasswordChangedElsewhere);
    assert_eq!(
        notice.to_string(),
        "Master password was changed on another device"
    );
    assert!(!b.with(|v| v.is_unlocked()));
    // C was locked: it installs the new material anyway.
    eventually("C installs the new material", || {
        c.with(|v| v.account_rev().unwrap()) == Some(rev + 1)
    })
    .await;

    // The new password opens every device; once it has, the old one fails
    // (until then the old wrap stays: nothing proved the server's).
    for dev in [&a, &b, &c] {
        dev.with(|v| {
            v.lock();
            v.unlock(pw(NEW_PW)).unwrap();
            v.lock();
            assert!(matches!(v.unlock(pw(PW)), Err(VaultError::WrongPassword)));
        });
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn stale_device_unlocks_with_new_password_via_refresh() {
    let srv = spawn_tls().await;
    let dir = tempfile::tempdir().unwrap();
    let mut a = local_device(dir.path(), "a");
    enable(&srv, &mut a).await;
    let mut c = join(&srv, dir.path(), "c", &a).await;
    c.stop().await; // offline during the change
    c.with(|v| v.lock());

    flows::change_password_synced(&a.vault, a.handle(), pw(PW), pw(NEW_PW))
        .await
        .unwrap();

    // Unlock is local first: the old password still opens the stale wrap
    // (documented limit), the new one does not yet.
    c.with(|v| {
        assert!(matches!(
            v.unlock(pw(NEW_PW)),
            Err(VaultError::WrongPassword)
        ));
        v.unlock(pw(PW)).unwrap();
        v.lock();
    });
    c.start();
    let err = flows::refresh_account_then_retry(&c.vault, c.handle(), pw("garbage"))
        .await
        .unwrap_err();
    assert_eq!(err, SyncError::WrongPassword);
    flows::refresh_account_then_retry(&c.vault, c.handle(), pw(NEW_PW))
        .await
        .unwrap();
    assert!(c.with(|v| v.is_unlocked()));
    a.stop().await;
    c.stop().await;
}

/// Calls through the handle work straight after `start`, before the engine's
/// first step has loaded the session (they used to get `Unauthorized`).
#[tokio::test(flavor = "multi_thread")]
async fn handle_client_is_signed_in_right_after_start() {
    let srv = spawn_tls().await;
    let dir = tempfile::tempdir().unwrap();
    let mut a = local_device(dir.path(), "a");
    enable(&srv, &mut a).await;
    for _ in 0..10 {
        a.stop().await;
        a.start();
        a.handle()
            .client()
            .account()
            .await
            .expect("account right after start");
    }
    a.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn recover_on_new_device_sets_new_password() {
    let srv = spawn_tls().await;
    let dir = tempfile::tempdir().unwrap();
    let mut a = local_device(dir.path(), "a");
    a.with(|v| v.upsert(host("keep me")).unwrap());
    enable(&srv, &mut a).await;
    let pin = pin_of(&srv);
    let d = blank_device(dir.path(), "d");

    let err =
        flows::recover_on_new_device(Some(&d.vault), &srv.url, pin, USER, "AAAA-BBBB", pw(NEW_PW))
            .await
            .unwrap_err();
    assert_eq!(err, SyncError::WrongRecoveryKey);
    let (_, other) = core_crypto::new_recovery_key();
    let err = flows::recover_on_new_device(Some(&d.vault), &srv.url, pin, USER, &other, pw(NEW_PW))
        .await
        .unwrap_err();
    assert_eq!(err, SyncError::WrongRecoveryKey);

    let rev = a.with(|v| v.account_rev().unwrap().unwrap());
    let done =
        flows::recover_on_new_device(Some(&d.vault), &srv.url, pin, USER, &a.recovery, pw(NEW_PW))
            .await
            .unwrap();
    assert_eq!(done.account_rev, rev + 1);

    // The other device hears about it and locks; the new password opens it.
    eventually("A locks", || !a.with(|v| v.is_unlocked())).await;
    a.with(|v| v.unlock(pw(NEW_PW))).unwrap();

    // The recovering device then joins with an add-device code and the new
    // password; the old one is refused.
    let code = link_code(&srv, &a).await;
    let err = flows::join_with_code(&d.vault, d.tokens(), &code, USER, pw(PW), false)
        .await
        .unwrap_err();
    assert_eq!(err, SyncError::WrongPassword);
    let mut d = d;
    flows::join_with_code(&d.vault, d.tokens(), &code, USER, pw(NEW_PW), false)
        .await
        .unwrap();
    d.start();
    d.settle().await;
    assert_eq!(d.hosts()[0].label, "keep me");
    a.stop().await;
    d.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn local_recovery_reset_with_sync_on_changes_password_everywhere() {
    let srv = spawn_tls().await;
    let dir = tempfile::tempdir().unwrap();
    let mut a = local_device(dir.path(), "a");
    enable(&srv, &mut a).await;
    let b = join(&srv, dir.path(), "b", &a).await;
    let mut notices = b.handle().notices();
    a.with(|v| v.lock()); // forgot the password

    let err = flows::recover_local_synced(&a.vault, a.handle(), "AAAA", pw(NEW_PW))
        .await
        .unwrap_err();
    assert_eq!(err, SyncError::WrongRecoveryKey);
    flows::recover_local_synced(&a.vault, a.handle(), &a.recovery, pw(NEW_PW))
        .await
        .unwrap();
    assert!(a.with(|v| v.is_unlocked()));

    let notice = tokio::time::timeout(WAIT, notices.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(notice, SyncNotice::PasswordChangedElsewhere);
    for dev in [&a, &b] {
        dev.with(|v| {
            v.lock();
            v.unlock(pw(NEW_PW)).unwrap();
            v.lock();
            assert!(matches!(v.unlock(pw(PW)), Err(VaultError::WrongPassword)));
        });
    }
    // The recovery key keeps working.
    assert!(server_accepts_recovery(&a, &a.recovery.clone()).await);
}

#[tokio::test(flavor = "multi_thread")]
async fn turn_off_keeps_local_records() {
    let srv = spawn_tls().await;
    let dir = tempfile::tempdir().unwrap();
    let mut a = local_device(dir.path(), "a");
    a.with(|v| v.upsert(host("stays")).unwrap());
    enable(&srv, &mut a).await;
    let device = a.device_id();
    assert!(a.tokens.load(device).unwrap().is_some());

    flows::turn_off(&a.vault, a.handle.take(), a.tokens())
        .await
        .unwrap();
    assert!(a.tokens.load(device).unwrap().is_none(), "token forgotten");
    assert!(a.with(|v| SyncConfig::load(v).unwrap()).is_none());
    assert!(core_sync::start(a.vault.clone(), a.tokens()).is_none());
    assert_eq!(a.hosts()[0].label, "stays");
    // Local edits keep working with sync off.
    a.with(|v| v.upsert(host("after")).unwrap());
    assert_eq!(a.hosts().len(), 2);
}

/// A credential store that always fails (no keychain, e.g. Linux without a
/// secret service).
struct BrokenKeychain;

impl TokenStore for BrokenKeychain {
    fn load(&self, _: core_model::Id) -> Result<Option<core_sync::DeviceCredentials>, SyncError> {
        Err(SyncError::Storage("no secret service".into()))
    }
    fn save(&self, _: core_model::Id, _: &core_sync::DeviceCredentials) -> Result<(), SyncError> {
        Err(SyncError::Storage("no secret service".into()))
    }
    fn delete(&self, _: core_model::Id) -> Result<(), SyncError> {
        Err(SyncError::Storage("no secret service".into()))
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn without_keychain_sync_uses_the_fallback_and_turns_off() {
    let srv = spawn_tls().await;
    let dir = tempfile::tempdir().unwrap();
    let a = local_device(dir.path(), "a");
    a.with(|v| v.upsert(host("kept")).unwrap());
    let broken: std::sync::Arc<dyn TokenStore> = std::sync::Arc::new(BrokenKeychain);
    let code = parse_join_code(&srv.make_invite().await).unwrap();
    flows::enable_with_invite(
        &a.vault,
        broken.clone(),
        &code,
        USER,
        pw(PW),
        RecoveryChoice::Existing(SecretString::from(a.recovery.clone())),
    )
    .await
    .unwrap();
    let handle = core_sync::start(a.vault.clone(), broken.clone()).expect("sync on");
    let status = handle.synced().await;
    assert!(
        matches!(status, core_sync::SyncStatus::Idle { .. }),
        "{status:?}"
    );

    flows::turn_off(&a.vault, Some(handle), broken.clone())
        .await
        .expect("turning off works without a keychain");
    assert!(a.with(|v| SyncConfig::load(v).unwrap()).is_none());
    assert_eq!(a.with(|v| v.sync_get("refresh_token").unwrap()), None);
    assert!(core_sync::start(a.vault.clone(), broken).is_none());
    assert_eq!(a.hosts()[0].label, "kept");
}

/// Points `dev`'s saved sync configuration at `url` (a restored test server
/// comes back on a new port).
fn retarget(dev: &Dev, url: &str) {
    dev.with(|v| {
        let mut cfg = SyncConfig::load(v).unwrap().unwrap();
        cfg.url = url.to_owned();
        cfg.save(v).unwrap();
    });
}

fn labels(dev: &Dev) -> Vec<String> {
    let mut labels: Vec<String> = dev.hosts().into_iter().map(|h| h.label).collect();
    labels.sort();
    labels
}

#[tokio::test(flavor = "multi_thread")]
async fn sign_in_again_after_a_restore_keeps_later_edits() {
    let srv = spawn_tls().await;
    let dir = tempfile::tempdir().unwrap();
    let mut a = local_device(dir.path(), "a");
    let h1 = host("h1");
    a.with(|v| v.upsert(h1.clone()).unwrap());
    enable(&srv, &mut a).await;
    let backup = dir.path().join("server.backup");
    srv.backup_to(&backup).await.unwrap();

    // After the backup the session rotates (it does every 15 minutes), and
    // A edits h1 and adds h2.
    a.handle().client().refresh().await.unwrap();
    let h2 = host("h2");
    a.with(|v| {
        let mut h = v.get::<Host>(h1.id).unwrap().unwrap();
        h.label = "h1 edited".into();
        v.upsert(h).unwrap();
        v.upsert(h2.clone()).unwrap();
    });
    a.settle().await;
    a.stop().await;

    // The restored server does not know A's current refresh token.
    let srv = srv.restore_from(&backup).await.unwrap();
    retarget(&a, &srv.url);
    a.start();
    let status = a.sync().await;
    assert!(
        matches!(
            &status,
            core_sync::SyncStatus::Error {
                kind: core_sync::SyncErrorKind::SignInAgain,
                ..
            }
        ),
        "{status:?}"
    );

    // A wrong password is refused and changes nothing.
    assert_eq!(
        flows::sign_in_again(&a.vault, a.handle(), pw("nope")).await,
        Err(SyncError::WrongPassword)
    );
    // The password signs the same device back in: no new device, no vault
    // replaced, and the later edits go up again.
    let device = a.device_id();
    flows::sign_in_again(&a.vault, a.handle(), pw(PW))
        .await
        .expect("sign in again");
    status_becomes(&a, |s| matches!(s, core_sync::SyncStatus::Idle { .. })).await;
    a.settle().await;
    assert_eq!(a.device_id(), device);
    assert_eq!(labels(&a), vec!["h1 edited", "h2"]);
    let backups = std::fs::read_dir(dir.path())
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains("before-join")
        })
        .count();
    assert_eq!(backups, 0);
    let on_server: BTreeSet<String> = server_rows(&a).await.into_iter().map(|r| r.0).collect();
    assert_eq!(
        on_server,
        BTreeSet::from([h1.id.to_string(), h2.id.to_string()])
    );
    let devices = a.handle().client().devices().await.unwrap();
    assert_eq!(devices.len(), 1);
    // A second device sees A's later edits.
    let b = join(&srv, dir.path(), "b", &a).await;
    assert_eq!(labels(&b), vec!["h1 edited", "h2"]);
    a.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn sign_in_again_refuses_a_revoked_device() {
    let srv = spawn_tls().await;
    let dir = tempfile::tempdir().unwrap();
    let mut a = local_device(dir.path(), "a");
    enable(&srv, &mut a).await;
    let b = join(&srv, dir.path(), "b", &a).await;
    b.handle()
        .client()
        .revoke_device(a.device_id())
        .await
        .unwrap();
    assert_eq!(
        flows::sign_in_again(&a.vault, a.handle(), pw(PW)).await,
        Err(SyncError::Revoked)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn sign_in_again_without_stored_credentials_needs_an_add_device_code() {
    let srv = spawn_tls().await;
    let dir = tempfile::tempdir().unwrap();
    let mut a = local_device(dir.path(), "a");
    enable(&srv, &mut a).await;
    a.stop().await;
    // The keychain lost the entry (refresh token and device secret).
    a.tokens.delete(a.device_id()).unwrap();
    a.start();
    let status = a.sync().await;
    assert!(
        matches!(
            &status,
            core_sync::SyncStatus::Error {
                kind: core_sync::SyncErrorKind::SignInAgain,
                ..
            }
        ),
        "{status:?}"
    );
    assert_eq!(
        flows::sign_in_again(&a.vault, a.handle(), pw(PW)).await,
        Err(SyncError::NeedsDeviceCode)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn rejoining_the_same_vault_keeps_offline_edits() {
    let srv = spawn_tls().await;
    let dir = tempfile::tempdir().unwrap();
    let mut a = local_device(dir.path(), "a");
    let shared = host("shared");
    a.with(|v| v.upsert(shared.clone()).unwrap());
    enable(&srv, &mut a).await;
    let mut b = join(&srv, dir.path(), "b", &a).await;

    // B turns sync off and edits while it is off.
    let handle = b.handle.take();
    flows::turn_off(&b.vault, handle, b.tokens()).await.unwrap();
    let offline = host("added while off");
    b.with(|v| {
        let mut h = v.get::<Host>(shared.id).unwrap().unwrap();
        h.label = "edited while off".into();
        v.upsert(h).unwrap();
        v.upsert(offline.clone()).unwrap();
    });

    // An add-device code for the same vault reconnects it: no replace asked
    // for, no backup, the records stay, and the edits sync.
    let joined = flows::join_with_code(
        &b.vault,
        b.tokens(),
        &link_code(&srv, &a).await,
        USER,
        pw(PW),
        false,
    )
    .await
    .expect("rejoin");
    assert!(joined.backup.is_none());
    assert!(joined.reconnected);
    assert_eq!(labels(&b), vec!["added while off", "edited while off"]);
    b.start();
    b.settle().await;
    a.settle().await;
    assert_eq!(labels(&a), vec!["added while off", "edited while off"]);
    assert_eq!(labels(&b), labels(&a));
    // A full resync cannot tell which server version an offline edit was
    // based on (R21: revisions are forgotten), so the edited record keeps
    // the server's older version as a conflict: the edit is current,
    // nothing is lost.
    for dev in [&a, &b] {
        let conflicts = dev.with(|v| v.list_conflicts().unwrap());
        assert_eq!(conflicts.len(), 1, "{conflicts:?}");
        assert_eq!(conflicts[0].record_id, shared.id);
        assert_eq!(
            conflicts[0].current.as_ref().unwrap()["label"],
            "edited while off"
        );
        assert_eq!(conflicts[0].other.as_ref().unwrap()["label"], "shared");
        assert_eq!(dev.with(|v| v.outbox_len().unwrap()), 0);
    }
    // An edit after the rejoin goes both ways.
    a.with(|v| v.upsert(host("after rejoin")).unwrap());
    a.settle().await;
    b.settle().await;
    assert_eq!(labels(&b).len(), 3);
    a.stop().await;
    b.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn rejoining_after_revoke_with_an_admin_code_keeps_edits() {
    let srv = spawn_tls().await;
    let dir = tempfile::tempdir().unwrap();
    let mut a = local_device(dir.path(), "a");
    enable(&srv, &mut a).await;
    let mut b = join(&srv, dir.path(), "b", &a).await;
    let a_id = a.device_id();

    // B revokes A; A's edit made meanwhile is still local only.
    b.handle().client().revoke_device(a_id).await.unwrap();
    let kept = host("unpushed at revoke");
    a.with(|v| v.upsert(kept.clone()).unwrap());
    status_becomes(&a, |s| {
        matches!(
            s,
            core_sync::SyncStatus::Error {
                kind: core_sync::SyncErrorKind::Revoked,
                ..
            }
        )
    })
    .await;
    // A turns sync off and comes back with a code from the server admin.
    let handle = a.handle.take();
    flows::turn_off(&a.vault, handle, a.tokens()).await.unwrap();
    let mut code = parse_join_code(
        &tether_server::cli::link_code(&srv.state.cfg, USER, Some("localhost"))
            .await
            .unwrap(),
    )
    .unwrap();
    code.url = srv.url.clone();
    let joined = flows::join_with_code(&a.vault, a.tokens(), &code, USER, pw(PW), false)
        .await
        .expect("rejoin");
    assert!(joined.reconnected && joined.backup.is_none());
    assert_ne!(a.device_id(), a_id, "a new device on the server");
    a.start();
    a.settle().await;
    b.settle().await;
    assert_eq!(labels(&b), vec!["unpushed at revoke"]);
    a.stop().await;
    b.stop().await;
}

/// A credential store that refuses every save with a non-storage error, so
/// not even the fallback is tried: a failure after `register`.
struct RefusingStore;

impl TokenStore for RefusingStore {
    fn load(&self, _: core_model::Id) -> Result<Option<core_sync::DeviceCredentials>, SyncError> {
        Ok(None)
    }
    fn save(&self, _: core_model::Id, _: &core_sync::DeviceCredentials) -> Result<(), SyncError> {
        Err(SyncError::Vault("injected failure".into()))
    }
    fn delete(&self, _: core_model::Id) -> Result<(), SyncError> {
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failure_after_register_still_hands_over_the_new_recovery_key() {
    let srv = spawn_tls().await;
    let dir = tempfile::tempdir().unwrap();
    let a = local_device(dir.path(), "a");
    let code = parse_join_code(&srv.make_invite().await).unwrap();
    let err = flows::enable_with_invite(
        &a.vault,
        std::sync::Arc::new(RefusingStore),
        &code,
        USER,
        pw(PW),
        RecoveryChoice::New,
    )
    .await
    .unwrap_err();
    assert_eq!(err.error, SyncError::Vault("injected failure".into()));
    let text = err
        .new_recovery_text
        .as_ref()
        .expect("the new key comes with the error")
        .expose_secret()
        .to_owned();
    let debug = format!("{err:?}");
    assert!(
        !debug.contains(&text) && debug.contains("REDACTED"),
        "{debug}"
    );
    // The server holds the new key, and so does the vault: the recovery wrap
    // and the enabled flag were committed together right after register.
    let key = core_crypto::parse_recovery_key(&text).unwrap();
    let probe = Client::new(&srv.url, pin_of(&srv)).unwrap();
    probe
        .recover_login(&RecoverReq {
            username: USER.into(),
            recovery_auth_key: core_crypto::recovery_auth_key(key.expose_secret())
                .expose_secret()
                .to_vec(),
            device: info(),
        })
        .await
        .expect("the server accepts the new key");
    a.with(|v| v.recovery_auth(&text)).unwrap();
    assert!(matches!(
        a.with(|v| v.recovery_auth(&a.recovery)),
        Err(VaultError::WrongRecoveryKey)
    ));
    assert!(a.with(|v| core_sync::is_enabled(v).unwrap()));
}

// -- connect with an address and a short code --------------------------------

fn address(url: &str) -> String {
    url.trim_start_matches("https://").to_owned()
}

#[tokio::test(flavor = "multi_thread")]
async fn short_code_enable_end_to_end() {
    let srv = spawn_tls().await;
    let code = tether_server::setup::ensure(&srv.state.db, srv.state.now_ms())
        .await
        .unwrap()
        .expect("a fresh server has a setup code");
    let dir = tempfile::tempdir().unwrap();
    let mut a = local_device(dir.path(), "a");
    a.with(|v| v.upsert(host("web")).unwrap());

    let addr = format!("127.0.0.1:{}", srv.addr().port());
    let result = flows::enable_with_short_code(
        &a.vault,
        a.tokens(),
        &addr,
        &code,
        USER,
        pw(PW),
        RecoveryChoice::Existing(SecretString::from(a.recovery.clone())),
    )
    .await
    .unwrap();
    assert!(result.new_recovery_text.is_none());
    let cfg = a.with(|v| SyncConfig::load(v).unwrap().unwrap());
    assert_eq!(cfg.url, srv.url);
    assert_eq!(cfg.pin, Some(Pin::new(srv.pin)), "self-signed: pinned");

    a.start();
    a.settle().await;
    assert_eq!(server_rows(&a).await.len(), 1);

    // Setup is closed: the setup code no longer verifies.
    let r = srv
        .post_json(
            "/v1/code/verify",
            &CodeVerifyReq {
                code_id: code.id().to_vec(),
                client_nonce: vec![7; 32],
                pinned: true,
            },
        )
        .await;
    assert_eq!(
        r.json::<ErrorBody>().code,
        ErrorCode::InviteInvalid,
        "{r:?}"
    );
    a.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn fake_server_gets_no_register_request() {
    let (fake, _) = net::stub_tls(|req| match req.route().as_str() {
        "GET /v1/health" => net::json(200, r#"{"status":"ok"}"#),
        "POST /v1/code/verify" => net::json(
            200,
            format!(
                r#"{{"serverProof":"{}"}}"#,
                data_encoding::BASE64.encode(&[0x5a; 32])
            ),
        ),
        _ => net::json(200, "{}"),
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let a = local_device(dir.path(), "a");
    let code = ShortCode::from_bytes([3; 10]);
    let err = flows::enable_with_short_code(
        &a.vault,
        a.tokens(),
        &address(&fake.url),
        &code,
        USER,
        pw(PW),
        RecoveryChoice::New,
    )
    .await
    .unwrap_err();
    assert_eq!(err, SyncError::ServerUnverified);
    assert!(err.new_recovery_text.is_none());
    assert_eq!(fake.count("POST /v1/code/verify"), 1);
    assert_eq!(fake.count("POST /v1/register"), 0, "nothing secret sent");
    assert!(
        core_sync::start(a.vault.clone(), a.tokens()).is_none(),
        "still off"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn relay_with_its_own_certificate_gets_no_register_request() {
    // A relay with its own self-signed certificate passing every request on
    // to the real server: the real server's proof is bound to the real pin,
    // the app pinned the relay's, so the proof must not check out.
    let srv = std::sync::Arc::new(spawn_tls().await);
    let code = tether_server::setup::ensure(&srv.state.db, srv.state.now_ms())
        .await
        .unwrap()
        .expect("a fresh server has a setup code");
    let real = srv.clone();
    let (relay, relay_pin) = net::stub_tls(move |req| {
        let body = (!req.body.is_empty()).then(|| req.body.clone());
        let headers: &[(&str, &str)] = if body.is_some() {
            &[("Content-Type", "application/json")]
        } else {
            &[]
        };
        let r = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(real.request(
                &req.method,
                &req.path,
                headers,
                body,
            ))
        });
        net::json(r.status, String::from_utf8(r.body).unwrap())
    })
    .await;
    assert_ne!(relay_pin, Pin::new(srv.pin));

    let dir = tempfile::tempdir().unwrap();
    let a = local_device(dir.path(), "a");
    let err = flows::enable_with_short_code(
        &a.vault,
        a.tokens(),
        &address(&relay.url),
        &code,
        USER,
        pw(PW),
        RecoveryChoice::New,
    )
    .await
    .unwrap_err();
    assert_eq!(err, SyncError::ServerUnverified);
    assert_eq!(
        relay.count("POST /v1/code/verify"),
        1,
        "the relay passed the proof request on"
    );
    assert_eq!(relay.count("POST /v1/register"), 0, "nothing secret sent");
    assert!(
        core_sync::start(a.vault.clone(), a.tokens()).is_none(),
        "still off"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn probe_pins_self_signed() {
    let srv = spawn_tls().await;
    let probe = tls::probe(&srv.url).await.unwrap();
    assert_eq!(probe.pin, Some(Pin::new(srv.pin)));

    // Something else answering.
    let (other, _) = net::stub_tls(|_| net::json(200, r#"{"hello":"world"}"#)).await;
    assert_eq!(
        tls::probe(&other.url).await.unwrap_err(),
        SyncError::Protocol("not a Termlyx server".into())
    );
    // Nothing answering.
    let free = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let err = tls::probe(&format!("https://{free}")).await.unwrap_err();
    assert!(matches!(err, SyncError::Unreachable(_)), "{err:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn link_code_carries_client_address() {
    let srv = spawn_tls().await;
    let dir = tempfile::tempdir().unwrap();
    let mut a = local_device(dir.path(), "a");
    enable(&srv, &mut a).await;

    let (text, url, expires) = flows::create_link_code(a.handle(), None).await.unwrap();
    assert!(expires > 0);
    let code = parse_join_code(&text).unwrap();
    assert_eq!(code.kind, CodeKind::Link);
    assert_eq!(
        (code.url.as_str(), url.as_str()),
        (srv.url.as_str(), srv.url.as_str())
    );
    assert_eq!(code.pin, Some(srv.pin));

    let (text, url, _) = flows::create_link_code(a.handle(), Some("10.0.0.9"))
        .await
        .unwrap();
    let code = parse_join_code(&text).unwrap();
    assert_eq!(code.url, "https://10.0.0.9:8443");
    assert_eq!(url, "https://10.0.0.9:8443");
    assert_eq!(code.pin, Some(srv.pin));
    a.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn old_server_without_verify_endpoint() {
    // An older server: health answers, the verify route is its 404 fallback.
    let (old, _) = net::stub_tls(|req| match req.route().as_str() {
        "GET /v1/health" => net::json(200, r#"{"status":"ok","protocol":1}"#),
        _ => net::error(404, "not_found"),
    })
    .await;
    let err = flows::check_server(&address(&old.url), &ShortCode::from_bytes([3; 10]))
        .await
        .err()
        .unwrap();
    assert_eq!(err, SyncError::UpdateServer);
}

/// A prelogin answer asking for the review's downgrade: 8 KiB, one pass,
/// one lane. Anything else the stub is asked fails.
async fn weak_kdf_server() -> (net::Stub, Pin) {
    net::stub_tls(|req| match req.route().as_str() {
        "GET /v1/health" => net::json(200, r#"{"status":"ok"}"#),
        "POST /v1/prelogin" => net::json(
            200,
            format!(
                r#"{{"kdf":{{"mKib":8,"t":1,"p":1,"salt":"{}"}}}}"#,
                data_encoding::BASE64.encode(&[7; 16])
            ),
        ),
        _ => net::json(500, "{}"),
    })
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn join_refuses_a_weak_kdf_before_sending_an_auth_key() {
    let (fake, pin) = weak_kdf_server().await;
    let dir = tempfile::tempdir().unwrap();
    let b = blank_device(dir.path(), "b");
    let code = core_proto::JoinCode {
        kind: CodeKind::Link,
        url: fake.url.clone(),
        pin: Some(*pin.as_bytes()),
        token: [1; 32],
    };
    let err = flows::join_with_code(&b.vault, b.tokens(), &code, USER, pw(PW), false)
        .await
        .unwrap_err();
    assert_eq!(err, SyncError::WeakKdf);
    assert_eq!(fake.count("POST /v1/prelogin"), 1);
    assert_eq!(fake.count("POST /v1/login"), 0, "no AuthKey sent");
}

#[tokio::test(flavor = "multi_thread")]
async fn sign_in_again_refuses_a_weak_kdf_before_sending_an_auth_key() {
    let srv = spawn_tls().await;
    let dir = tempfile::tempdir().unwrap();
    let mut a = local_device(dir.path(), "a");
    enable(&srv, &mut a).await;
    a.stop().await;
    let (fake, pin) = weak_kdf_server().await;
    a.with(|v| {
        let mut cfg = SyncConfig::load(v).unwrap().unwrap();
        cfg.url = fake.url.clone();
        cfg.pin = Some(pin);
        cfg.save(v).unwrap();
    });
    a.start();
    let err = flows::sign_in_again(&a.vault, a.handle(), pw(PW))
        .await
        .unwrap_err();
    assert_eq!(err, SyncError::WeakKdf);
    assert_eq!(fake.count("POST /v1/prelogin"), 1);
    assert_eq!(fake.count("POST /v1/login"), 0, "no AuthKey sent");
    a.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn recover_on_new_device_refuses_a_weak_kdf() {
    let dir = tempfile::tempdir().unwrap();
    let a = local_device(dir.path(), "a");
    let mut material = a.with(|v| v.account_material().unwrap());
    material.kdf.m_kib = 8;
    material.kdf.t = 1;
    material.kdf.p = 1;
    let account = serde_json::to_string(&core_proto::AccountResp {
        material,
        account_rev: 3,
    })
    .unwrap();
    let tokens = format!(
        r#"{{"accessToken":"a","refreshToken":"r","expiresIn":900,"deviceId":"{}","node":1,"epoch":"e","deviceSecret":"{}"}}"#,
        core_model::Id::now_v7(),
        data_encoding::BASE64URL_NOPAD.encode(&[3; 32])
    );
    let (fake, pin) = net::stub_tls(move |req| match req.route().as_str() {
        "GET /v1/health" => net::json(200, r#"{"status":"ok"}"#),
        "POST /v1/recover-login" => net::json(200, tokens.clone()),
        "GET /v1/account" => net::json(200, account.clone()),
        _ => net::json(500, "{}"),
    })
    .await;
    let b = blank_device(dir.path(), "b");
    let err = flows::recover_on_new_device(
        Some(&b.vault),
        &fake.url,
        Some(pin),
        USER,
        &a.recovery,
        pw(NEW_PW),
    )
    .await
    .unwrap_err();
    assert_eq!(err, SyncError::WeakKdf);
    assert_eq!(fake.count("GET /v1/account"), 1);
    // Without a vault (the database failed to open) the floor is the
    // production default cost.
    let err =
        flows::recover_on_new_device(None, &fake.url, Some(pin), USER, &a.recovery, pw(NEW_PW))
            .await
            .unwrap_err();
    assert_eq!(err, SyncError::WeakKdf);
    assert_eq!(fake.count("GET /v1/account"), 2);
    assert_eq!(
        fake.count("POST /v1/account/recover"),
        0,
        "no AuthKey derived with the weak cost was sent"
    );
}
