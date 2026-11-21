//! The HTTP client: one async method per server endpoint (`docs/protocol.md`).
//!
//! # Sessions
//!
//! A [`Client`] holds the short-lived access token in memory only. The
//! long-lived credentials (refresh token + device secret) are kept in a
//! [`TokenStore`]. Refresh tokens are single-use, so a refresh *rotates* them:
//! the new refresh token is saved to the store **before** the new access
//! token is used (a crash in between must not leave the device holding a
//! rotated-out token, which the server treats as theft and revokes the device
//! for). When the save fails the new tokens are still kept in memory (the old
//! refresh token is already dead), the call that rotated returns
//! [`SyncError::Storage`], [`Client::credentials_unsaved`] stays set, and
//! every authenticated call retries the save first (failing with the store's
//! error until it works): the store must not keep the rotated-out token, since
//! presenting it after a restart gets the device revoked.
//!
//! Before rotating, the store is asked whether it could save the result
//! ([`TokenStore::ready`]): the iOS keychain refuses every access while the
//! device is locked, and a refresh then fails with
//! [`SyncError::StorageLocked`] without touching the server.
//!
//! A request answered `401 unauthorized` refreshes once and is retried once;
//! `401 revoked` is final ([`SyncError::Revoked`]). Concurrent requests share
//! one refresh.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use core_model::Id;
use core_proto::{
    AccountResp, ChangesResp, CodeVerifyReq, CodeVerifyResp, DeviceView, ErrorBody, ErrorCode,
    HEADER_DEVICE, HEADER_DEVICE_SECRET, HEADER_PROTOCOL, HEADER_SCHEMA, KNOWN_SCHEMA, LinkResp,
    LoginReq, PROTOCOL_VERSION, PasswordChangeReq, PreloginReq, PreloginResp, PushReq, PushResp,
    RecoverReq, RecoveryResetReq, RecoveryUpdateReq, RefreshReq, RegisterReq, RenameDeviceReq,
    TokenResp,
};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderValue};
use reqwest::{Method, Url};
use secrecy::{ExposeSecret, SecretString};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::error::SyncError;
use crate::tls::{self, Pin};
use crate::tokens::{DeviceCredentials, MemoryTokenStore, TokenStore};

/// TCP + TLS connect timeout.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Whole-request timeout.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// An access token this close to its expiry is refreshed before use.
const EXPIRY_MARGIN: Duration = Duration::from_secs(30);
/// Largest response body accepted (a full page of 1 MiB envelopes is ~1 GiB in
/// theory; real pages are far smaller, and a hostile server must not fill
/// memory).
const MAX_RESPONSE: usize = 64 * 1024 * 1024;
/// Largest error body read.
const MAX_ERROR_BODY: usize = 64 * 1024;
/// How long a device's first contact with a server waits for the connection
/// to open ([`Client::reach`]): on iOS the first connections to the local
/// network fail until the user answers the permission prompt. Elsewhere
/// there is no such prompt, and no wait.
pub(crate) const FIRST_CONTACT_PATIENCE: Duration = if cfg!(target_os = "ios") {
    Duration::from_secs(60)
} else {
    Duration::ZERO
};
/// Pause between connection attempts in [`Client::reach`].
const REACH_RETRY: Duration = Duration::from_secs(1);

#[derive(Default)]
struct Session {
    /// The device this client acts as, once it has one.
    device: Option<Id>,
    /// Refresh token + device secret, cached from the store / last rotation.
    creds: Option<DeviceCredentials>,
    access: Option<SecretString>,
    expires_at: Option<Instant>,
    /// Bumped whenever the access token is replaced, so a request that failed
    /// with an old token can tell another request already refreshed.
    generation: u64,
    /// The last rotation could not be saved to the store.
    unsaved: bool,
}

struct Inner {
    http: reqwest::Client,
    tls: Arc<rustls::ClientConfig>,
    base: String,
    pin: Option<Pin>,
    session: Mutex<Session>,
    store: Mutex<Arc<dyn TokenStore>>,
    /// Serialises refreshes: refresh tokens are single-use.
    refresh_gate: tokio::sync::Mutex<()>,
}

/// A client for one sync server. Cheap to clone; clones share the session.
#[derive(Clone)]
pub struct Client {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the tokens, never the credentials.
        f.debug_struct("Client")
            .field("base", &self.inner.base)
            .field("pin", &self.inner.pin)
            .field("device", &self.device())
            .finish_non_exhaustive()
    }
}

/// One HTTP exchange.
struct Call {
    method: Method,
    path: String,
    body: Option<Vec<u8>>,
    /// Sign the request with the access token (and refresh on 401).
    bearer: bool,
    /// `X-Tether-Device-Secret`, for a password re-login.
    device_secret: Option<SecretString>,
}

impl Call {
    fn new(method: Method, path: impl Into<String>) -> Call {
        Call {
            method,
            path: path.into(),
            body: None,
            bearer: false,
            device_secret: None,
        }
    }

    fn bearer(mut self) -> Call {
        self.bearer = true;
        self
    }

    fn json<T: Serialize>(mut self, body: &T) -> Result<Call, SyncError> {
        // Not `{:?}`/`Display`: serde's message can echo field names only.
        self.body = Some(
            serde_json::to_vec(body)
                .map_err(|_| SyncError::Protocol("request could not be encoded".into()))?,
        );
        Ok(self)
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    // No code path panics while holding these locks.
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn is_loopback_host(host: &str) -> bool {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

fn bad_url() -> SyncError {
    SyncError::Protocol("server address is not valid".into())
}

impl Client {
    /// A client for the server at `url`.
    ///
    /// `url` must be `https://`; plain `http://` is accepted only for a
    /// loopback host and without a pin (as in join codes). With a `pin`, only
    /// the certificate with that key is trusted; without one, normal WebPKI
    /// trust applies. Nothing is contacted until the first call. Credentials
    /// are kept in memory until [`Client::with_store`] gives a real store.
    pub fn new(url: &str, pin: Option<Pin>) -> Result<Client, SyncError> {
        Client::with_tls(url, pin, tls::client_config(pin))
    }

    /// [`Client::new`] with its own TLS configuration (the probe's).
    pub(crate) fn with_tls(
        url: &str,
        pin: Option<Pin>,
        tls: rustls::ClientConfig,
    ) -> Result<Client, SyncError> {
        let parsed = Url::parse(url.trim()).map_err(|_| bad_url())?;
        let host = parsed.host_str().ok_or_else(bad_url)?;
        if !parsed.username().is_empty() || parsed.password().is_some() {
            return Err(bad_url());
        }
        match parsed.scheme() {
            "https" => {}
            "http" if pin.is_none() && is_loopback_host(host) => {}
            _ => return Err(bad_url()),
        }
        let authority = match parsed.port() {
            Some(port) => format!("{host}:{port}"),
            None => host.to_owned(),
        };
        let base = format!(
            "{}://{authority}{}",
            parsed.scheme(),
            parsed.path().trim_end_matches('/'),
        );

        let tls = Arc::new(tls);
        let http = reqwest::Client::builder()
            .tls_backend_preconfigured(tls.as_ref().clone())
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            // A redirect could carry the bearer token to another host.
            .redirect(reqwest::redirect::Policy::none())
            // The server is on the LAN or behind the user's VPN; the pin is
            // checked end to end either way, but do not guess at proxies.
            .no_proxy()
            .user_agent(concat!("tether-sync/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|_| SyncError::Protocol("could not set up the HTTP client".into()))?;
        Ok(Client {
            inner: Arc::new(Inner {
                http,
                tls,
                base,
                pin,
                session: Mutex::new(Session::default()),
                store: Mutex::new(Arc::new(MemoryTokenStore::new())),
                refresh_gate: tokio::sync::Mutex::new(()),
            }),
        })
    }

    /// Use `store` for the device credentials (a fresh client keeps them in
    /// memory only).
    pub fn with_store(self, store: Arc<dyn TokenStore>) -> Client {
        self.set_store(store);
        self
    }

    /// Act as `device` from the start: the first authenticated call loads its
    /// credentials from the store and refreshes, without waiting for
    /// [`Client::resume`]. The engine builds its client this way, so calls
    /// through [`crate::SyncHandle::client`] work right after
    /// [`crate::start`].
    pub fn for_device(self, device: Id) -> Client {
        lock(&self.inner.session).device = Some(device);
        self
    }

    /// Replace the credential store (for example after the keychain failed
    /// and the engine switches to its file fallback). Call [`Client::persist`]
    /// afterwards to write the current credentials to the new store.
    pub fn set_store(&self, store: Arc<dyn TokenStore>) {
        *lock(&self.inner.store) = store;
    }

    /// The server's base URL, without a trailing slash.
    pub fn base_url(&self) -> &str {
        &self.inner.base
    }

    /// The certificate pin in use, if any.
    pub fn pin(&self) -> Option<Pin> {
        self.inner.pin
    }

    /// The device this client acts as, once signed in or resumed (or from the
    /// start, see [`Client::for_device`]).
    pub fn device(&self) -> Option<Id> {
        lock(&self.inner.session).device
    }

    /// Whether this client holds device credentials (signed in, resumed, or
    /// loaded by a call since [`Client::for_device`]).
    pub(crate) fn has_credentials(&self) -> bool {
        lock(&self.inner.session).creds.is_some()
    }

    /// Whether the last token rotation could not be saved to the store (the
    /// new tokens are in memory only until [`Client::persist`] succeeds).
    pub fn credentials_unsaved(&self) -> bool {
        lock(&self.inner.session).unsaved
    }

    pub(crate) fn tls_config(&self) -> Arc<rustls::ClientConfig> {
        self.inner.tls.clone()
    }

    fn store(&self) -> Arc<dyn TokenStore> {
        lock(&self.inner.store).clone()
    }

    // -- store access (blocking calls kept off the async threads) ----------

    async fn store_load(&self, device: Id) -> Result<Option<DeviceCredentials>, SyncError> {
        let store = self.store();
        blocking(move || store.load(device)).await
    }

    async fn store_save(&self, device: Id, creds: DeviceCredentials) -> Result<(), SyncError> {
        let store = self.store();
        blocking(move || store.save(device, &creds)).await
    }

    // -- session management ------------------------------------------------

    /// Act as `device` using the credentials saved in the store. Returns
    /// `false` (and changes nothing) if the store has none. The first
    /// authenticated call then refreshes to get an access token.
    ///
    /// Already acting as `device` (see [`Client::for_device`]), it keeps the
    /// session: credentials a call has loaded or rotated meanwhile are at
    /// least as new as the store's, and the access token stays valid, so a
    /// call waiting to refresh is not left without one.
    pub async fn resume(&self, device: Id) -> Result<bool, SyncError> {
        let _gate = self.inner.refresh_gate.lock().await;
        {
            let s = lock(&self.inner.session);
            if s.device == Some(device) && s.creds.is_some() {
                return Ok(true);
            }
        }
        let Some(creds) = self.store_load(device).await? else {
            return Ok(false);
        };
        let mut s = lock(&self.inner.session);
        if s.device == Some(device) {
            s.creds = Some(creds);
            return Ok(true);
        }
        s.device = Some(device);
        s.creds = Some(creds);
        s.access = None;
        s.expires_at = None;
        s.generation += 1;
        s.unsaved = false;
        Ok(true)
    }

    /// Take over the session in a `register`, `login` or `recover_login`
    /// response: save the credentials to the store (first), then use the new
    /// access token.
    ///
    /// A response without a refresh token (the limited token of
    /// `recover_login`) only sets the access token; there is nothing to save
    /// and no device. If saving fails nothing changes and the caller still
    /// holds `resp`, so it can [`Client::set_store`] a fallback and retry.
    pub async fn adopt(&self, resp: &TokenResp) -> Result<(), SyncError> {
        let _gate = self.inner.refresh_gate.lock().await;
        let access = SecretString::from(resp.access_token.clone());
        let expires_at = Instant::now() + Duration::from_secs(resp.expires_in);
        if resp.refresh_token.is_empty() {
            let mut s = lock(&self.inner.session);
            s.access = Some(access);
            s.expires_at = Some(expires_at);
            s.generation += 1;
            return Ok(());
        }
        let device = Id::parse_str(&resp.device_id)
            .map_err(|_| SyncError::Protocol("the server sent a malformed device id".into()))?;
        // The secret is issued once, when the device is created; a re-login
        // keeps the one already held.
        let secret = if resp.device_secret.is_empty() {
            let held = {
                let s = lock(&self.inner.session);
                s.creds
                    .as_ref()
                    .filter(|_| s.device == Some(device))
                    .map(|c| SecretString::from(c.device_secret.expose_secret().to_owned()))
            };
            match held {
                Some(secret) => secret,
                None => self
                    .store_load(device)
                    .await?
                    .map(|c| c.device_secret)
                    .ok_or_else(|| {
                        SyncError::Protocol("the server did not issue a device secret".into())
                    })?,
            }
        } else {
            SecretString::from(resp.device_secret.clone())
        };
        let creds = DeviceCredentials {
            refresh: SecretString::from(resp.refresh_token.clone()),
            device_secret: secret,
        };
        self.store_save(device, creds.clone()).await?;
        let mut s = lock(&self.inner.session);
        s.device = Some(device);
        s.creds = Some(creds);
        s.access = Some(access);
        s.expires_at = Some(expires_at);
        s.generation += 1;
        s.unsaved = false;
        Ok(())
    }

    /// Write the current credentials to the store (after a failed save, or
    /// after [`Client::set_store`]). Authenticated calls do this by
    /// themselves while [`Client::credentials_unsaved`] is set.
    pub async fn persist(&self) -> Result<(), SyncError> {
        let _gate = self.inner.refresh_gate.lock().await;
        self.persist_locked().await
    }

    /// [`Client::persist`] with the refresh gate already held.
    async fn persist_locked(&self) -> Result<(), SyncError> {
        let (device, creds) = {
            let s = lock(&self.inner.session);
            match (s.device, s.creds.clone()) {
                (Some(d), Some(c)) => (d, c),
                _ => return Ok(()),
            }
        };
        self.store_save(device, creds).await?;
        lock(&self.inner.session).unsaved = false;
        Ok(())
    }

    /// While the last rotation is unsaved, try to save it before anything
    /// else happens: the store still holds the rotated-out refresh token, and
    /// presenting that after a restart makes the server revoke the device.
    /// Fails (with the store's error) while the store keeps refusing.
    async fn ensure_saved(&self) -> Result<(), SyncError> {
        if !self.credentials_unsaved() {
            return Ok(());
        }
        let _gate = self.inner.refresh_gate.lock().await;
        if !self.credentials_unsaved() {
            return Ok(());
        }
        self.persist_locked().await
    }

    /// Drop the session from memory and delete the saved credentials. Use
    /// after leaving sync; the server-side device is untouched (see
    /// [`Client::logout`], [`Client::revoke_device`]).
    pub async fn forget(&self) -> Result<(), SyncError> {
        let _gate = self.inner.refresh_gate.lock().await;
        let device = {
            let mut s = lock(&self.inner.session);
            let device = s.device.take();
            *s = Session {
                generation: s.generation + 1,
                ..Session::default()
            };
            device
        };
        if let Some(device) = device {
            let store = self.store();
            blocking(move || store.delete(device)).await?;
        }
        Ok(())
    }

    /// Rotate the refresh token now and get a new access token. Most callers
    /// never need this: authenticated calls refresh by themselves.
    ///
    /// Returns [`SyncError::Storage`] if the rotated credentials could not be
    /// saved; the new tokens are then in effect anyway (see the module docs).
    pub async fn refresh(&self) -> Result<(), SyncError> {
        let _gate = self.inner.refresh_gate.lock().await;
        self.rotate_and_save().await
    }

    /// Rotate, then make sure the new refresh token is saved. A failed save is
    /// retried once; if that fails too the caller gets the error (the engine
    /// must see it) while the new tokens stay in effect in memory and
    /// [`Client::credentials_unsaved`] stays set. Caller holds the gate.
    async fn rotate_and_save(&self) -> Result<(), SyncError> {
        if let Some(save_error) = self.rotate().await? {
            tracing::warn!(error = %save_error, "rotated sync credentials could not be saved");
            self.persist_locked().await?;
        }
        Ok(())
    }

    /// The access token and its generation, if one is held and not about to
    /// expire.
    fn fresh_access(&self) -> Option<(SecretString, u64)> {
        let s = lock(&self.inner.session);
        let token = s.access.as_ref()?;
        // Only worth refreshing when there is something to refresh with.
        let stale = s
            .expires_at
            .is_some_and(|at| Instant::now() + EXPIRY_MARGIN >= at);
        if stale && Self::can_refresh_in(&s) {
            return None;
        }
        Some((
            SecretString::from(token.expose_secret().to_owned()),
            s.generation,
        ))
    }

    fn can_refresh_in(s: &Session) -> bool {
        s.device.is_some() || s.creds.is_some()
    }

    fn can_refresh(&self) -> bool {
        Self::can_refresh_in(&lock(&self.inner.session))
    }

    fn generation(&self) -> u64 {
        lock(&self.inner.session).generation
    }

    /// An access token valid for a while, refreshing first if there is none
    /// or it is about to expire. Returns it with its generation.
    pub(crate) async fn access(&self) -> Result<(SecretString, u64), SyncError> {
        self.ensure_saved().await?;
        if let Some(found) = self.fresh_access() {
            return Ok(found);
        }
        let seen = self.generation();
        self.refresh_after(seen).await?;
        self.current_access().ok_or(SyncError::Unauthorized)
    }

    /// The held access token whatever its age.
    fn current_access(&self) -> Option<(SecretString, u64)> {
        let s = lock(&self.inner.session);
        let token = s.access.as_ref()?;
        Some((
            SecretString::from(token.expose_secret().to_owned()),
            s.generation,
        ))
    }

    /// Refresh because a request made with the access token of generation
    /// `seen` was refused (or none is held). If another task already
    /// refreshed meanwhile, does nothing.
    pub(crate) async fn refresh_after(&self, seen: u64) -> Result<(), SyncError> {
        let _gate = self.inner.refresh_gate.lock().await;
        if self.generation() != seen {
            return Ok(());
        }
        self.rotate_and_save().await
    }

    /// `POST /v1/refresh` with the held refresh token. Caller holds the gate.
    /// `Ok(Some(e))`: refreshed, but saving failed with `e`.
    async fn rotate(&self) -> Result<Option<SyncError>, SyncError> {
        let (device, cached) = {
            let s = lock(&self.inner.session);
            (s.device, s.creds.clone())
        };
        let device = device.ok_or(SyncError::Unauthorized)?;
        let creds = match cached {
            Some(c) => c,
            None => self
                .store_load(device)
                .await?
                .ok_or(SyncError::Unauthorized)?,
        };
        // A store that could not save the rotated token now (the iOS
        // keychain while the device is locked) must not be rotated into a
        // token it would lose: ask first, before the server kills this one.
        let store = self.store();
        blocking(move || store.ready(device)).await?;
        let body = RefreshReq {
            refresh_token: creds.refresh.expose_secret().to_owned(),
        };
        // A refresh that is itself refused as `unauthorized` stays that.
        // (`send_once`, not `call`: a refresh never refreshes.)
        let call = Call::new(Method::POST, "/v1/refresh").json(&body)?;
        drop(body);
        let bytes = self.send_once(&call, None).await?;
        let resp: TokenResp = serde_json::from_slice(&bytes)
            .map_err(|_| SyncError::Protocol("the server's answer could not be read".into()))?;
        let rotated = DeviceCredentials {
            refresh: SecretString::from(resp.refresh_token.clone()),
            device_secret: creds.device_secret,
        };
        // Save first: the old refresh token is dead the moment the server
        // answered, so the new one must reach durable storage before anything
        // depends on it.
        let saved = self.store_save(device, rotated.clone()).await;
        let mut s = lock(&self.inner.session);
        s.creds = Some(rotated);
        s.access = Some(SecretString::from(resp.access_token.clone()));
        s.expires_at = Some(Instant::now() + Duration::from_secs(resp.expires_in));
        s.generation += 1;
        s.unsaved = saved.is_err();
        Ok(saved.err())
    }

    // -- HTTP plumbing -----------------------------------------------------

    /// Run `call`; for a bearer call, refresh once on `unauthorized` and retry
    /// once. `Revoked` is never retried.
    async fn call_json<R: DeserializeOwned>(&self, call: Call) -> Result<R, SyncError> {
        let bytes = self.call(call).await?;
        serde_json::from_slice(&bytes)
            .map_err(|_| SyncError::Protocol("the server's answer could not be read".into()))
    }

    async fn call_empty(&self, call: Call) -> Result<(), SyncError> {
        self.call(call).await.map(|_| ())
    }

    async fn call(&self, call: Call) -> Result<Vec<u8>, SyncError> {
        if !call.bearer {
            return self.send_once(&call, None).await;
        }
        let (token, seen) = self.access().await?;
        match self.send_once(&call, Some(&token)).await {
            Err(SyncError::Unauthorized) if self.can_refresh() => {
                self.refresh_after(seen).await?;
                let (token, _) = self.current_access().ok_or(SyncError::Unauthorized)?;
                self.send_once(&call, Some(&token)).await
            }
            other => other,
        }
    }

    async fn send_once(
        &self,
        call: &Call,
        token: Option<&SecretString>,
    ) -> Result<Vec<u8>, SyncError> {
        let url = format!("{}{}", self.inner.base, call.path);
        let mut req = self
            .inner
            .http
            .request(call.method.clone(), url)
            .header(HEADER_PROTOCOL, PROTOCOL_VERSION.to_string())
            .header(HEADER_SCHEMA, KNOWN_SCHEMA.to_string());
        if let Some(device) = self.device() {
            req = req.header(HEADER_DEVICE, device.to_string());
        }
        if let Some(secret) = &call.device_secret {
            req = req.header(HEADER_DEVICE_SECRET, secret_header(secret.expose_secret())?);
        }
        if let Some(token) = token {
            req = req.header(
                AUTHORIZATION,
                secret_header(&format!("Bearer {}", token.expose_secret()))?,
            );
        }
        if let Some(body) = &call.body {
            req = req
                .header(CONTENT_TYPE, "application/json")
                .body(body.clone());
        }
        let mut res = req.send().await.map_err(|e| transport(&e))?;

        // A server speaking a newer protocol than this app understands.
        if newer_protocol(res.headers()) {
            return Err(SyncError::UpdateClient);
        }
        let status = res.status();
        let limit = if status.is_success() {
            MAX_RESPONSE
        } else {
            MAX_ERROR_BODY
        };
        let mut body = Vec::new();
        while let Some(chunk) = res.chunk().await.map_err(|e| transport(&e))? {
            if body.len() + chunk.len() > limit {
                if status.is_success() {
                    return Err(SyncError::Protocol(
                        "the server's answer is too large".into(),
                    ));
                }
                break;
            }
            body.extend_from_slice(&chunk);
        }
        if status.is_success() {
            return Ok(body);
        }
        match serde_json::from_slice::<ErrorBody>(&body) {
            Ok(err) => Err(SyncError::from_body(err)),
            Err(_) => Err(SyncError::Protocol(format!(
                "unexpected HTTP status {}",
                status.as_u16()
            ))),
        }
    }

    /// The WebSocket URL of the events endpoint.
    pub(crate) fn events_url(&self) -> String {
        let rest = self
            .inner
            .base
            .strip_prefix("https://")
            .map(|r| format!("wss://{r}"))
            .or_else(|| {
                self.inner
                    .base
                    .strip_prefix("http://")
                    .map(|r| format!("ws://{r}"))
            })
            .unwrap_or_else(|| self.inner.base.clone());
        format!("{rest}/v1/events")
    }

    /// Whether a refresh could be attempted (a device session exists).
    pub(crate) fn has_session(&self) -> bool {
        self.can_refresh()
    }

    // -- endpoints ---------------------------------------------------------

    /// Waits, for up to `patience`, until a connection to the server opens.
    /// iOS fails an app's first connections to the local network while it
    /// asks the user for permission, so a join's first request would fail
    /// before the user could tap Allow. Only a connection that could not be
    /// opened is tried again (nothing reached the server, so no code is
    /// spent); any answer at all ends the wait, and a timeout, a TLS failure
    /// or a changed certificate fails at once.
    pub async fn reach(&self, patience: Duration) -> Result<(), SyncError> {
        let deadline = tokio::time::Instant::now() + patience;
        loop {
            let url = format!("{}/v1/health", self.inner.base);
            let err = match self.inner.http.get(url).send().await {
                Ok(_) => return Ok(()),
                Err(e) => e,
            };
            let error = transport(&err);
            let refused = err.is_connect()
                && matches!(&error, SyncError::Unreachable(why) if why != "timed out" && !why.starts_with("TLS"));
            if !refused || tokio::time::Instant::now() + REACH_RETRY >= deadline {
                return Err(error);
            }
            tokio::time::sleep(REACH_RETRY).await;
        }
    }

    /// `GET /v1/health`: the raw body.
    pub(crate) async fn health(&self) -> Result<Vec<u8>, SyncError> {
        self.send_once(&Call::new(Method::GET, "/v1/health"), None)
            .await
    }

    /// `POST /v1/code/verify`: the server's proof that it knows a short code.
    /// A server without the endpoint (older than this app) is
    /// [`SyncError::UpdateServer`].
    pub async fn verify_code(&self, req: &CodeVerifyReq) -> Result<CodeVerifyResp, SyncError> {
        match self
            .call_json(Call::new(Method::POST, "/v1/code/verify").json(req)?)
            .await
        {
            Err(SyncError::Server {
                code: ErrorCode::NotFound,
                ..
            }) => Err(SyncError::UpdateServer),
            other => other,
        }
    }

    /// `POST /v1/prelogin`: the KDF parameters to derive the AuthKey with.
    pub async fn prelogin(&self, req: &PreloginReq) -> Result<PreloginResp, SyncError> {
        self.call_json(Call::new(Method::POST, "/v1/prelogin").json(req)?)
            .await
    }

    /// `POST /v1/register`: create an account with an invite. The response
    /// carries the device secret exactly once: pass it to [`Client::adopt`]
    /// before doing anything else.
    pub async fn register(&self, req: &RegisterReq) -> Result<TokenResp, SyncError> {
        self.call_json(Call::new(Method::POST, "/v1/register").json(req)?)
            .await
    }

    /// `POST /v1/login`. With `req.link_token` it adds this device to the
    /// account (the response carries the new device secret). Without one it
    /// signs back in an existing device and sends its id and secret, which
    /// the client must hold ([`Client::resume`]); otherwise the server
    /// refuses it.
    pub async fn login(&self, req: &LoginReq) -> Result<TokenResp, SyncError> {
        let mut call = Call::new(Method::POST, "/v1/login").json(req)?;
        if req.link_token.is_none() {
            call.device_secret = self.held_device_secret().await?;
        }
        self.call_json(call).await
    }

    async fn held_device_secret(&self) -> Result<Option<SecretString>, SyncError> {
        let (device, cached) = {
            let s = lock(&self.inner.session);
            (
                s.device,
                s.creds
                    .as_ref()
                    .map(|c| SecretString::from(c.device_secret.expose_secret().to_owned())),
            )
        };
        match (cached, device) {
            (Some(secret), _) => Ok(Some(secret)),
            (None, Some(device)) => Ok(self.store_load(device).await?.map(|c| c.device_secret)),
            (None, None) => Ok(None),
        }
    }

    /// `POST /v1/recover-login`: sign in with the recovery auth key. The
    /// response holds a limited-scope access token: [`Client::adopt`] it, then
    /// call [`Client::account`] and [`Client::recovery_reset`].
    pub async fn recover_login(&self, req: &RecoverReq) -> Result<TokenResp, SyncError> {
        self.call_json(Call::new(Method::POST, "/v1/recover-login").json(req)?)
            .await
    }

    /// `POST /v1/logout`: end this device's session. The device stays
    /// registered; sign in again with [`Client::login`].
    pub async fn logout(&self) -> Result<(), SyncError> {
        self.call_empty(Call::new(Method::POST, "/v1/logout").bearer())
            .await?;
        let mut s = lock(&self.inner.session);
        s.access = None;
        s.expires_at = None;
        s.generation += 1;
        Ok(())
    }

    /// `GET /v1/devices`: the account's devices.
    pub async fn devices(&self) -> Result<Vec<DeviceView>, SyncError> {
        self.call_json(Call::new(Method::GET, "/v1/devices").bearer())
            .await
    }

    /// `PATCH /v1/devices/{id}`: rename a device.
    pub async fn rename_device(&self, device: Id, name: &str) -> Result<(), SyncError> {
        let body = RenameDeviceReq { name: name.into() };
        self.call_empty(
            Call::new(Method::PATCH, format!("/v1/devices/{device}"))
                .bearer()
                .json(&body)?,
        )
        .await
    }

    /// `DELETE /v1/devices/{id}`: revoke a device (this one included).
    pub async fn revoke_device(&self, device: Id) -> Result<(), SyncError> {
        self.call_empty(Call::new(Method::DELETE, format!("/v1/devices/{device}")).bearer())
            .await
    }

    /// `POST /v1/link`: an add-device code for another device.
    pub async fn create_link(&self) -> Result<LinkResp, SyncError> {
        self.call_json(Call::new(Method::POST, "/v1/link").bearer())
            .await
    }

    /// `GET /v1/account`: the wrapped key material and `accountRev`. Works
    /// with the limited token from [`Client::recover_login`].
    pub async fn account(&self) -> Result<AccountResp, SyncError> {
        self.call_json(Call::new(Method::GET, "/v1/account").bearer())
            .await
    }

    /// `POST /v1/account/password`: change the master password.
    pub async fn change_password(&self, req: &PasswordChangeReq) -> Result<AccountResp, SyncError> {
        self.call_json(
            Call::new(Method::POST, "/v1/account/password")
                .bearer()
                .json(req)?,
        )
        .await
    }

    /// `POST /v1/account/recover`: reset the password with the recovery key
    /// (needs the limited token from [`Client::recover_login`]).
    pub async fn recovery_reset(&self, req: &RecoveryResetReq) -> Result<AccountResp, SyncError> {
        self.call_json(
            Call::new(Method::POST, "/v1/account/recover")
                .bearer()
                .json(req)?,
        )
        .await
    }

    /// `PUT /v1/account/recovery`: replace the recovery key.
    pub async fn put_recovery(&self, req: &RecoveryUpdateReq) -> Result<AccountResp, SyncError> {
        self.call_json(
            Call::new(Method::PUT, "/v1/account/recovery")
                .bearer()
                .json(req)?,
        )
        .await
    }

    /// `GET /v1/sync/changes`: one page of the vault's change log after
    /// `since` (`0` = everything). [`SyncError::Gone`] means the cursor is too
    /// old and a full resync is needed.
    pub async fn changes(&self, since: i64, limit: u32) -> Result<ChangesResp, SyncError> {
        self.call_json(
            Call::new(
                Method::GET,
                format!("/v1/sync/changes?since={since}&limit={limit}"),
            )
            .bearer(),
        )
        .await
    }

    /// `POST /v1/sync/push`: push rows. [`SyncError::Gone`] means the server
    /// lacks a record the device believes it has: full resync.
    pub async fn push(&self, req: &PushReq) -> Result<PushResp, SyncError> {
        self.call_json(
            Call::new(Method::POST, "/v1/sync/push")
                .bearer()
                .json(req)?,
        )
        .await
    }
}

/// A header value holding a secret: rejected (not logged) if it has
/// characters HTTP does not allow.
fn secret_header(value: &str) -> Result<HeaderValue, SyncError> {
    let mut v = HeaderValue::from_str(value)
        .map_err(|_| SyncError::Protocol("a credential has characters HTTP cannot carry".into()))?;
    v.set_sensitive(true);
    Ok(v)
}

/// Whether a response says the server's protocol major is above ours.
pub(crate) fn newer_protocol(headers: &reqwest::header::HeaderMap) -> bool {
    headers
        .get(HEADER_PROTOCOL)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u32>().ok())
        .is_some_and(|major| major > PROTOCOL_VERSION)
}

fn transport(e: &reqwest::Error) -> SyncError {
    tls::classify(e, e.is_timeout())
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, SyncError> + Send + 'static,
) -> Result<T, SyncError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|_| SyncError::Storage("the credential store task failed".into()))?
}
