//! Rate limiting for the unauthenticated endpoints.
//!
//! * Sliding one-minute windows, in memory, per client address and per
//!   lowercase username ([`RateLimiter::check_attempt`]). They reset on restart,
//!   which only ever loosens them for a minute.
//! * Lockout after repeated failures lives in the database for real accounts
//!   (`accounts.failed_logins` / `locked_until` / `last_failed_at`, so it
//!   survives restarts). For usernames that do not exist the same rule is
//!   tracked here in memory, so an attacker cannot tell a locked real account
//!   from a made-up name by whether the lock ever appears. "Consecutive" is
//!   time-bounded for both: a failure more than one lockout window after the
//!   previous one starts the count again, which also lets idle entries be
//!   swept. Unknown-name state is lost on restart (documented in
//!   `docs/protocol.md`).
//!
//! Every method takes `now` in Unix milliseconds from the application clock, so
//! tests can move time.

use std::collections::{HashMap, VecDeque};
use std::convert::Infallible;
use std::hash::Hash;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Mutex;

use axum::extract::{ConnectInfo, FromRequestParts};
use axum::http::request::Parts;
use core_proto::ErrorCode;

use crate::error::{ApiError, ApiResult};
use crate::state::Limits;

/// Length of the sliding window.
const WINDOW_MS: i64 = 60_000;
/// Maps larger than this are swept of expired entries on insert.
const SWEEP_AT: usize = 4096;
/// Hard cap on tracked keys (a flood of distinct names or addresses): past it
/// the oldest information is evicted rather than growing without bound.
const MAX_KEYS: usize = 100_000;

/// Failure bookkeeping for a username that has no account.
#[derive(Debug, Clone, Copy)]
struct Fails {
    count: u32,
    locked_until: i64,
    last_failed: i64,
}

impl Fails {
    /// When this entry last mattered: the later of its last failure and the
    /// end of its lock.
    fn activity(&self) -> i64 {
        self.last_failed.max(self.locked_until)
    }
}

#[derive(Default)]
struct Inner {
    ip: HashMap<IpAddr, VecDeque<i64>>,
    /// IPv6 /48 networks (one tunnel-broker customer owns 65 536 /64s).
    net48: HashMap<IpAddr, VecDeque<i64>>,
    user: HashMap<String, VecDeque<i64>>,
    unknown: HashMap<String, Fails>,
}

/// In-memory sliding windows and unknown-username lockouts.
pub struct RateLimiter {
    inner: Mutex<Inner>,
    /// Bound on tracked unknown usernames.
    max_keys: usize,
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::with_max_keys(MAX_KEYS)
    }
}

/// How many /64s' worth of attempts one IPv6 /48 may use per window.
const NET48_FACTOR: u32 = 4;

/// The /48 an IPv6 address belongs to (IPv4: the address itself, already one key).
fn net48(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => {
            let mut o = v6.octets();
            o[6..].fill(0);
            IpAddr::V6(o.into())
        }
        v4 => v4,
    }
}

/// Drop timestamps that left the window.
fn prune(q: &mut VecDeque<i64>, now: i64) {
    while q.front().is_some_and(|&t| t <= now - WINDOW_MS) {
        q.pop_front();
    }
}

/// Is there room for one more hit under `max` per window?
fn has_room<K: Hash + Eq>(
    map: &mut HashMap<K, VecDeque<i64>>,
    key: &K,
    now: i64,
    max: u32,
) -> bool {
    match map.get_mut(key) {
        Some(q) => {
            prune(q, now);
            q.len() < max as usize
        }
        None => max > 0,
    }
}

/// Record a hit (call only after [`has_room`] said yes).
fn record<K: Hash + Eq + Clone>(map: &mut HashMap<K, VecDeque<i64>>, key: &K, now: i64) {
    if map.len() > SWEEP_AT {
        map.retain(|_, q| {
            prune(q, now);
            !q.is_empty()
        });
        if map.len() > MAX_KEYS {
            // Still huge with everything live: drop the quietest quarter.
            let mut by_age: Vec<(i64, K)> = map
                .iter()
                .map(|(k, q)| (q.back().copied().unwrap_or(0), k.clone()))
                .collect();
            by_age.sort_unstable_by_key(|(t, _)| *t);
            for (_, k) in by_age.into_iter().take(MAX_KEYS / 4) {
                map.remove(&k);
            }
        }
    }
    map.entry(key.clone()).or_default().push_back(now);
}

impl RateLimiter {
    /// A limiter that tracks at most `max_keys` unknown usernames (default
    /// 100 000); tests use a small cap.
    pub fn with_max_keys(max_keys: usize) -> Self {
        Self {
            inner: Mutex::default(),
            max_keys: max_keys.max(1),
        }
    }

    /// The client addresses currently tracked (for tests that check which
    /// address a real connection is counted under).
    #[doc(hidden)]
    pub fn tracked_ips(&self) -> Vec<IpAddr> {
        self.lock().ip.keys().copied().collect()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Count one prelogin/login/register/recover-login attempt from `ip` for
    /// `username`. `Err(rate_limited)` if either window is full; a refused
    /// attempt is not counted against the other window.
    pub fn check_attempt(
        &self,
        ip: IpAddr,
        username: &str,
        now: i64,
        limits: &Limits,
    ) -> ApiResult<()> {
        self.attempt(ip, Some(username), now, limits)
    }

    /// Like [`Self::check_attempt`] with only the address windows: for a
    /// request that proved it comes from one of the account's own devices,
    /// which a stranger filling the username's window must not block.
    pub fn check_ip(&self, ip: IpAddr, now: i64, limits: &Limits) -> ApiResult<()> {
        self.attempt(ip, None, now, limits)
    }

    fn attempt(
        &self,
        ip: IpAddr,
        username: Option<&str>,
        now: i64,
        limits: &Limits,
    ) -> ApiResult<()> {
        let mut g = self.lock();
        let user = username.map(str::to_owned);
        let net = net48(ip);
        if !has_room(&mut g.ip, &ip, now, limits.ip_per_minute)
            || !has_room(
                &mut g.net48,
                &net,
                now,
                limits.ip_per_minute.saturating_mul(NET48_FACTOR),
            )
            || user
                .as_ref()
                .is_some_and(|u| !has_room(&mut g.user, u, now, limits.username_per_minute))
        {
            return Err(ApiError::of(ErrorCode::RateLimited));
        }
        record(&mut g.ip, &ip, now);
        record(&mut g.net48, &net, now);
        if let Some(u) = &user {
            record(&mut g.user, u, now);
        }
        Ok(())
    }

    /// Is `username` (which has no account) locked out?
    pub fn unknown_locked(&self, username: &str, now: i64) -> bool {
        self.lock()
            .unknown
            .get(username)
            .is_some_and(|f| f.locked_until > now)
    }

    /// Count a failed login for a username without an account, with the same
    /// rule the database applies to real accounts (ten consecutive failures,
    /// where a failure more than one lockout window after the previous one
    /// restarts the count).
    ///
    /// The map is bounded: at the cap, entries that no longer matter (idle for
    /// a window, not locked) are swept, and if that is not enough the oldest
    /// quarter is evicted. Fresh names are therefore always tracked, so a flood
    /// of other names cannot stop a lock from appearing.
    pub fn record_unknown_failure(&self, username: &str, now: i64, limits: &Limits) {
        let mut g = self.lock();
        if g.unknown.len() >= self.max_keys && !g.unknown.contains_key(username) {
            g.unknown
                .retain(|_, f| f.locked_until > now || now - f.last_failed <= limits.lockout_ms);
            if g.unknown.len() >= self.max_keys {
                let mut by_age: Vec<(i64, String)> = g
                    .unknown
                    .iter()
                    .map(|(k, f)| (f.activity(), k.clone()))
                    .collect();
                by_age.sort_unstable();
                for (_, k) in by_age.into_iter().take((self.max_keys / 4).max(1)) {
                    g.unknown.remove(&k);
                }
            }
        }
        let f = g.unknown.entry(username.to_owned()).or_insert(Fails {
            count: 0,
            locked_until: 0,
            last_failed: now,
        });
        if now - f.last_failed > limits.lockout_ms {
            f.count = 0;
        }
        f.last_failed = now;
        f.count += 1;
        if f.count >= limits.lockout_failures {
            f.count = 0;
            f.locked_until = now + limits.lockout_ms;
        }
    }
}

/// The client's address for rate limiting: the peer address of the TCP
/// connection (IPv4-mapped IPv6 unwrapped; IPv6 reduced to its /64, since one
/// subscriber usually owns a whole /64). Falls back to `0.0.0.0` when the
/// server was not started with connect info (in-process router tests).
///
/// Behind a reverse proxy every client shares the proxy's address; the per-IP
/// limit then acts as a global one. Proxy headers are deliberately not
/// trusted (they are spoofable when the server is reachable directly).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientIp(pub IpAddr);

impl ClientIp {
    /// Normalise a peer address into the limiting key.
    pub fn from_addr(addr: IpAddr) -> Self {
        match addr.to_canonical() {
            IpAddr::V6(v6) => {
                let mut o = v6.octets();
                o[8..].fill(0);
                ClientIp(IpAddr::V6(o.into()))
            }
            v4 => ClientIp(v4),
        }
    }
}

impl<S: Send + Sync> FromRequestParts<S> for ClientIp {
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Infallible> {
        Ok(parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map_or(ClientIp(IpAddr::V4(Ipv4Addr::UNSPECIFIED)), |c| {
                ClientIp::from_addr(c.0.ip())
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn windows_slide_and_refusals_are_not_counted() {
        let l = RateLimiter::default();
        let lim = Limits {
            ip_per_minute: 3,
            username_per_minute: 2,
            ..Limits::default()
        };
        let a = ip("10.0.0.1");
        assert!(l.check_attempt(a, "u", 0, &lim).is_ok());
        assert!(l.check_attempt(a, "u", 1, &lim).is_ok());
        // Username window full; the refused attempt did not use an IP slot.
        assert!(l.check_attempt(a, "u", 2, &lim).is_err());
        assert!(l.check_attempt(a, "v", 3, &lim).is_ok());
        assert!(l.check_attempt(a, "w", 4, &lim).is_err());
        // Entries leave the window after a minute.
        assert!(l.check_attempt(a, "w", 60_001, &lim).is_ok());
        // A different address has its own window.
        assert!(l.check_attempt(ip("10.0.0.2"), "x", 5, &lim).is_ok());
    }

    #[test]
    fn many_64s_in_one_48_share_a_window() {
        let l = RateLimiter::default();
        let lim = Limits {
            ip_per_minute: 2,
            username_per_minute: 1000,
            ..Limits::default()
        };
        // Each /64 stays under its own limit, but the /48 allows only 2 * NET48_FACTOR.
        let n = (lim.ip_per_minute * NET48_FACTOR) as usize;
        for i in 0..n {
            let a = ip(&format!("2001:db8:1:{i:x}::1"));
            assert!(l.check_attempt(a, &format!("u{i}"), 0, &lim).is_ok(), "{i}");
        }
        assert!(l.check_attempt(ip("2001:db8:1:ffff::1"), "z", 1, &lim).is_err());
        // Another /48 is unaffected.
        assert!(l.check_attempt(ip("2001:db8:2::1"), "z", 1, &lim).is_ok());
    }

    #[test]
    fn unknown_username_locks_like_a_real_one() {
        let l = RateLimiter::default();
        let lim = Limits {
            lockout_failures: 3,
            lockout_ms: 1000,
            ..Limits::default()
        };
        for _ in 0..2 {
            l.record_unknown_failure("ghost", 10, &lim);
            assert!(!l.unknown_locked("ghost", 10));
        }
        l.record_unknown_failure("ghost", 10, &lim);
        assert!(l.unknown_locked("ghost", 10));
        assert!(l.unknown_locked("ghost", 1009));
        assert!(!l.unknown_locked("ghost", 1010));
        assert!(!l.unknown_locked("other", 10));
    }

    #[test]
    fn client_ip_normalises() {
        let v4 = ClientIp::from_addr(ip("::ffff:192.0.2.7"));
        assert_eq!(v4.0, ip("192.0.2.7"));
        let a = ClientIp::from_addr(ip("2001:db8:1:2:aaaa:bbbb:cccc:dddd"));
        let b = ClientIp::from_addr(ip("2001:db8:1:2:1:2:3:4"));
        assert_eq!(a, b);
        assert_eq!(a.0, ip("2001:db8:1:2::"));
        assert_ne!(a, ClientIp::from_addr(ip("2001:db8:1:3::1")));
    }

    #[test]
    fn failure_count_decays_after_window() {
        let l = RateLimiter::default();
        let lim = Limits {
            lockout_failures: 10,
            lockout_ms: 1000,
            ..Limits::default()
        };
        for i in 0..9 {
            l.record_unknown_failure("ghost", i, &lim);
        }
        // Quiet for longer than a window: the nine no longer count.
        l.record_unknown_failure("ghost", 2000, &lim);
        assert!(!l.unknown_locked("ghost", 2000));
        for i in 0..8 {
            l.record_unknown_failure("ghost", 2001 + i, &lim);
            assert!(!l.unknown_locked("ghost", 2001 + i));
        }
        // 1 + 9 consecutive failures within windows: now it locks.
        l.record_unknown_failure("ghost", 2010, &lim);
        assert!(l.unknown_locked("ghost", 2010));
    }

    #[test]
    fn unknown_name_lockout_survives_many_other_names() {
        let lim = Limits::default();
        let l = RateLimiter::with_max_keys(100);
        // Far more distinct names than the cap, one failure each.
        for i in 0..1000 {
            l.record_unknown_failure(&format!("flood{i}"), i, &lim);
        }
        assert!(l.lock().unknown.len() <= 100);
        // A fresh name still locks after ten failures, like a real account.
        let t = 1000;
        for k in 0..10 {
            assert!(!l.unknown_locked("target", t), "locked early at {k}");
            l.record_unknown_failure("target", t + k, &lim);
        }
        assert!(l.unknown_locked("target", t + 10));
        // And idle entries are swept once their window has passed.
        let later = t + lim.lockout_ms * 2;
        l.record_unknown_failure("another-fresh-name", later, &lim);
        for i in 0..100 {
            l.record_unknown_failure(&format!("late{i}"), later, &lim);
        }
        assert!(!l.lock().unknown.contains_key("flood0"));
    }
}
