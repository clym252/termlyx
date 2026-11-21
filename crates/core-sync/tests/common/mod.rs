//! Shared helpers for the engine and flow tests: vaults with the cheap test
//! KDF, devices signed in to an in-process TLS server, and waiting helpers.

#![allow(dead_code)]

pub mod net;

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use core_api::vault::{MemoryKeyStore, SharedVault, VaultService};
use core_model::{ConnSettings, Host, Id, Identity};
use core_sync::flows::{self, RecoveryChoice};
use core_sync::{
    JoinCode, MemoryTokenStore, Pin, SyncHandle, SyncStatus, TokenStore, parse_join_code,
};
use secrecy::SecretString;
use tether_server::testing::TestServer;

pub const PW: &str = "correct horse battery";
pub const NEW_PW: &str = "new staple";
pub const USER: &str = "alice";
pub const WAIT: Duration = Duration::from_secs(10);

pub fn pw(s: &str) -> SecretString {
    SecretString::from(s)
}

pub fn pin_of(srv: &TestServer) -> Option<Pin> {
    (srv.pin != [0; 32]).then(|| Pin::new(srv.pin))
}

/// One device: its vault, its credential store and (when running) its engine.
pub struct Dev {
    pub vault: SharedVault,
    pub tokens: Arc<MemoryTokenStore>,
    pub handle: Option<SyncHandle>,
    /// The recovery key text of the vault it created (first device only).
    pub recovery: String,
}

impl Dev {
    pub fn tokens(&self) -> Arc<dyn TokenStore> {
        self.tokens.clone()
    }

    pub fn handle(&self) -> &SyncHandle {
        self.handle.as_ref().expect("engine running")
    }

    /// Starts the engine (sync must be on).
    pub fn start(&mut self) {
        assert!(self.handle.is_none(), "engine already running");
        self.handle = Some(core_sync::start(self.vault.clone(), self.tokens()).expect("sync on"));
    }

    /// Stops the engine and waits for it to finish.
    pub async fn stop(&mut self) {
        if let Some(h) = self.handle.take() {
            h.stop().await;
        }
    }

    /// Runs a sync cycle requested now and returns how it ended.
    pub async fn sync(&self) -> SyncStatus {
        tokio::time::timeout(WAIT, self.handle().synced())
            .await
            .expect("sync cycle timed out")
    }

    /// Runs a sync cycle and insists it ended idle.
    pub async fn settle(&self) {
        let status = self.sync().await;
        assert!(matches!(status, SyncStatus::Idle { .. }), "{status:?}");
    }

    pub fn with<T>(&self, f: impl FnOnce(&mut VaultService) -> T) -> T {
        f(&mut self.vault.lock().unwrap())
    }

    pub fn hosts(&self) -> Vec<Host> {
        let mut hosts = self.with(|v| v.list::<Host>().unwrap());
        hosts.sort_by_key(|h| h.id);
        hosts
    }

    pub fn device_id(&self) -> Id {
        self.with(|v| core_sync::SyncConfig::load(v).unwrap().unwrap().device_id)
    }
}

fn open(path: &Path) -> SharedVault {
    let v = VaultService::open_for_tests(path, Box::new(MemoryKeyStore::default())).unwrap();
    Arc::new(Mutex::new(v))
}

/// A device with a new (unlocked) local vault, sync off.
pub fn local_device(dir: &Path, name: &str) -> Dev {
    let vault = open(&dir.join(format!("{name}.db")));
    let recovery = vault.lock().unwrap().create(pw(PW)).unwrap();
    Dev {
        vault,
        tokens: Arc::new(MemoryTokenStore::new()),
        handle: None,
        recovery,
    }
}

/// A device with no vault yet (first run).
pub fn blank_device(dir: &Path, name: &str) -> Dev {
    Dev {
        vault: open(&dir.join(format!("{name}.db"))),
        tokens: Arc::new(MemoryTokenStore::new()),
        handle: None,
        recovery: String::new(),
    }
}

/// Turns sync on for `dev` with a fresh invite (keeping its recovery key),
/// starts its engine and waits for the first sync.
pub async fn enable(srv: &TestServer, dev: &mut Dev) {
    let code = parse_join_code(&srv.make_invite().await).unwrap();
    let result = flows::enable_with_invite(
        &dev.vault,
        dev.tokens(),
        &code,
        USER,
        pw(PW),
        RecoveryChoice::Existing(SecretString::from(dev.recovery.clone())),
    )
    .await
    .expect("enable");
    assert!(result.new_recovery_text.is_none());
    dev.start();
    dev.settle().await;
}

/// An add-device code from `from`. It carries the address and pin `from`
/// uses (not the server's placeholder `https://localhost`).
pub async fn link_code(srv: &TestServer, from: &Dev) -> JoinCode {
    let (code, _, _) = flows::create_link_code(from.handle(), None)
        .await
        .expect("link code");
    let code = parse_join_code(&code).unwrap();
    assert_eq!(code.pin, Some(srv.pin));
    assert_eq!(code.url, srv.url);
    code
}

/// A new device joined through an add-device code from `from`, engine
/// started and synced.
pub async fn join(srv: &TestServer, dir: &Path, name: &str, from: &Dev) -> Dev {
    let mut dev = blank_device(dir, name);
    let code = link_code(srv, from).await;
    flows::join_with_code(&dev.vault, dev.tokens(), &code, USER, pw(PW), false)
        .await
        .expect("join");
    dev.start();
    dev.settle().await;
    dev
}

pub fn host(label: &str) -> Host {
    Host {
        id: Id::now_v7(),
        group_id: None,
        label: label.into(),
        address: "10.0.0.1".into(),
        tag_ids: vec![],
        settings: ConnSettings::default(),
        forward_rule_ids: vec![],
        extra: Default::default(),
    }
}

pub fn identity(password: &str) -> Identity {
    Identity {
        id: Id::now_v7(),
        label: "prod".into(),
        username: "root".into(),
        password: Some(SecretString::from(password)),
        key_id: None,
        owner: None,
        extra: Default::default(),
    }
}

/// Polls `f` until it holds or `WAIT` passes.
pub async fn eventually(what: &str, mut f: impl FnMut() -> bool) {
    let start = Instant::now();
    while !f() {
        assert!(start.elapsed() < WAIT, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Waits until the engine's status matches `pred`.
pub async fn status_becomes(dev: &Dev, pred: impl Fn(&SyncStatus) -> bool) -> SyncStatus {
    let mut rx = dev.handle().status();
    let wait = rx.wait_for(|s| pred(s));
    tokio::time::timeout(WAIT, wait)
        .await
        .expect("timed out waiting for a status")
        .expect("engine gone")
        .clone()
}

/// Every live row the server holds: `(id, kind, hlc)`.
pub async fn server_rows(dev: &Dev) -> Vec<(String, String, String)> {
    let client = dev.handle().client().clone();
    let mut rows = std::collections::BTreeMap::new();
    let mut since = 0;
    loop {
        let page = client.changes(since, 1000).await.expect("changes");
        for r in page.changes {
            if r.deleted {
                rows.remove(&r.id);
            } else {
                rows.insert(r.id.clone(), (r.id, r.kind, r.hlc));
            }
        }
        since = page.next;
        if !page.more {
            break;
        }
    }
    rows.into_values().collect()
}

/// A local vault set up as if sync had been turned on against `url` (a stub
/// server): config, bookkeeping and stored credentials, engine not started.
/// Returns the device and its device id.
pub fn stub_device(dir: &Path, url: &str) -> (Dev, Id) {
    let dev = local_device(dir, "stubbed");
    let device = Id::now_v7();
    dev.with(|v| {
        core_sync::SyncConfig {
            url: url.into(),
            pin: None,
            username: USER.into(),
            device_id: device,
        }
        .save(v)
        .unwrap();
        v.sync_put_all(&[
            ("enabled", b"1"),
            ("cursor", b"0"),
            ("epoch", b"stub-epoch"),
            ("account_rev", b"1"),
            ("kinds_version", b"2"),
        ])
        .unwrap();
    });
    dev.tokens
        .save(
            device,
            &core_sync::DeviceCredentials {
                refresh: SecretString::from("refresh"),
                device_secret: SecretString::from("secret"),
            },
        )
        .unwrap();
    (dev, device)
}

/// Stub answers for a healthy server: refresh, account (the device's own
/// material) and an empty change log. `None` for anything else.
pub fn healthy(req: &net::Req, device: Id, material: &str) -> Option<net::Reply> {
    Some(match req.route().as_str() {
        "POST /v1/refresh" => net::json(
            200,
            format!(
                r#"{{"accessToken":"access","refreshToken":"refresh-{}","expiresIn":900,"deviceId":"{device}","node":1,"epoch":"stub-epoch"}}"#,
                Id::now_v7().simple()
            ),
        ),
        "GET /v1/account" => net::json(200, format!(r#"{{"material":{material},"accountRev":1}}"#)),
        "GET /v1/sync/changes" => {
            let since = req
                .path
                .split("since=")
                .nth(1)
                .and_then(|r| r.split('&').next())
                .unwrap_or("0")
                .to_owned();
            net::json(
                200,
                format!(r#"{{"changes":[],"next":{since},"more":false,"epoch":"stub-epoch"}}"#),
            )
        }
        _ => return None,
    })
}

/// Answers a push by accepting every item.
pub fn accept_push(req: &net::Req) -> net::Reply {
    let push: core_proto::PushReq = serde_json::from_slice(&req.body).unwrap();
    let results: Vec<core_proto::PushResult> = push
        .items
        .iter()
        .enumerate()
        .map(|(i, item)| core_proto::PushResult::Ok {
            id: item.row.id.clone(),
            rev: 1,
            seq: 100 + i as i64,
        })
        .collect();
    net::json(
        200,
        serde_json::to_string(&core_proto::PushResp { results }).unwrap(),
    )
}

/// Ids of the rows the stub accepted in pushes.
pub fn pushed_ids(stub: &net::Stub) -> Vec<String> {
    stub.log
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r.route() == "POST /v1/sync/push")
        .flat_map(|r| {
            let push: core_proto::PushReq = serde_json::from_slice(&r.body).unwrap();
            push.items.into_iter().map(|i| i.row.id)
        })
        .collect()
}
