//! OpenSSH `known_hosts` files: parsing (for import) and host-pattern
//! matching (for verification against stored entries).
//!
//! Supported per line: `hosts keytype base64 [comment]`, where `hosts` is a
//! comma-separated list of plain names or addresses (with `*`/`?` globs),
//! `[host]:port`, or hashed `|1|salt|hash` names. Skipped: blank lines,
//! `#` comments, marker lines (`@cert-authority`, `@revoked`), lines with a
//! negated (`!host`) pattern (their meaning depends on the whole line, which
//! this one-entry-per-pattern model cannot keep), and lines whose key does
//! not parse or does not match its key type.

use data_encoding::BASE64;
use hmac::{Hmac, KeyInit, Mac};
use russh::keys::PublicKey;
use sha1::Sha1;

use crate::hostkey::HostKeyInfo;

/// Port of a hashed entry: the real port is inside the hash.
pub const HASHED_PORT: u16 = 0;

/// The default SSH port, which OpenSSH writes without brackets.
const DEFAULT_PORT: u16 = 22;

/// One trusted key for one host pattern. A file line listing several hosts
/// yields one entry per host. Maps field-for-field onto
/// `core_model::KnownHost` (plus an id, timestamp and source).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownHostEntry {
    /// A host name or address (lowercased), a glob such as `*.corp`, or a
    /// hashed `|1|salt|hash` name exactly as in the file.
    pub host_pattern: String,
    /// The port: 22 unless the file used `[host]:port`, and
    /// [`HASHED_PORT`] (0) for hashed names, whose port is inside the hash.
    pub port: u16,
    /// Key algorithm, e.g. `ssh-ed25519` (as [`HostKeyInfo::algorithm`]).
    pub algorithm: String,
    /// Base64 public key blob (the second field of an OpenSSH key line).
    pub public_key: String,
}

/// Parses an OpenSSH `known_hosts` file. Lines it cannot use are skipped
/// (see the module docs); it never fails.
pub fn parse_known_hosts(text: &str) -> Vec<KnownHostEntry> {
    text.lines().flat_map(parse_line).collect()
}

fn parse_line(line: &str) -> Vec<KnownHostEntry> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return Vec::new();
    }
    let mut fields = line.split_whitespace();
    let (Some(hosts), Some(keytype), Some(blob)) = (fields.next(), fields.next(), fields.next())
    else {
        return Vec::new();
    };
    // `@cert-authority`, `@revoked` and any future marker.
    if hosts.starts_with('@') {
        return Vec::new();
    }
    let Some((algorithm, public_key)) = canonical_key(keytype, blob) else {
        return Vec::new();
    };
    let patterns: Vec<&str> = hosts.split(',').filter(|p| !p.is_empty()).collect();
    if patterns.iter().any(|p| p.starts_with('!')) {
        return Vec::new();
    }
    patterns
        .into_iter()
        .filter_map(parse_pattern)
        .map(|(host_pattern, port)| KnownHostEntry {
            host_pattern,
            port,
            algorithm: algorithm.clone(),
            public_key: public_key.clone(),
        })
        .collect()
}

/// The key as `(algorithm, base64)` in the same canonical form the
/// transport reports for server keys, or `None` if it does not parse or does
/// not match `keytype`.
fn canonical_key(keytype: &str, blob: &str) -> Option<(String, String)> {
    let key = PublicKey::from_openssh(&format!("{keytype} {blob}")).ok()?;
    let info = HostKeyInfo::from_public_key(&key).ok()?;
    if info.algorithm != keytype {
        return None;
    }
    let (algorithm, public_key) = info.openssh.split_once(' ')?;
    Some((algorithm.to_owned(), public_key.to_owned()))
}

/// One host pattern from a line, as `(host_pattern, port)`.
fn parse_pattern(p: &str) -> Option<(String, u16)> {
    if p.starts_with("|1|") {
        return parse_hashed(p).map(|_| (p.to_owned(), HASHED_PORT));
    }
    if p.starts_with('[') {
        let (host, port) = parse_bracketed(p)?;
        return Some((host.to_ascii_lowercase(), port));
    }
    Some((p.to_ascii_lowercase(), DEFAULT_PORT))
}

/// `[host]:port` → `(host, port)`.
fn parse_bracketed(p: &str) -> Option<(&str, u16)> {
    let rest = p.strip_prefix('[')?;
    let (host, port) = rest.split_once("]:")?;
    if host.is_empty() {
        return None;
    }
    Some((host, port.parse().ok()?))
}

/// `|1|salt|hash` → `(salt, hash)`, both base64-decoded SHA-1 sized.
fn parse_hashed(p: &str) -> Option<(Vec<u8>, Vec<u8>)> {
    let (salt, hash) = p.strip_prefix("|1|")?.split_once('|')?;
    let salt = BASE64.decode(salt.as_bytes()).ok()?;
    let hash = BASE64.decode(hash.as_bytes()).ok()?;
    (salt.len() == 20 && hash.len() == 20).then_some((salt, hash))
}

/// Whether a stored pattern (with the port stored next to it) covers
/// `host:port`.
///
/// * Hashed `|1|salt|hash`: HMAC-SHA1 of the name the way OpenSSH hashes it,
///   `host` for port 22 and `[host]:port` otherwise (`pattern_port` is
///   ignored).
/// * `[host]:port`: its own port (`pattern_port` is ignored).
/// * Anything else: a name or `*`/`?` glob, on `pattern_port`.
///
/// Names compare case-insensitively.
pub fn host_matches(pattern: &str, pattern_port: u16, host: &str, port: u16) -> bool {
    let host = host.to_ascii_lowercase();
    if pattern.starts_with("|1|") {
        let Some((salt, hash)) = parse_hashed(pattern) else {
            return false;
        };
        let name = if port == DEFAULT_PORT {
            host
        } else {
            format!("[{host}]:{port}")
        };
        let Ok(mut mac) = <Hmac<Sha1> as KeyInit>::new_from_slice(&salt) else {
            return false;
        };
        mac.update(name.as_bytes());
        return mac.verify_slice(&hash).is_ok();
    }
    if pattern.starts_with('[') {
        return match parse_bracketed(pattern) {
            Some((p_host, p_port)) => p_port == port && glob(&p_host.to_ascii_lowercase(), &host),
            None => false,
        };
    }
    pattern_port == port && glob(&pattern.to_ascii_lowercase(), &host)
}

/// `*` matches any run of characters, `?` exactly one.
fn glob(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0, 0);
    // Position of the last `*` and the text index it is currently covering.
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ti));
            pi += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == '*')
}

#[cfg(test)]
mod tests {
    use super::glob;

    #[test]
    fn glob_matches_like_openssh_patterns() {
        assert!(glob("*", ""));
        assert!(glob("*", "anything"));
        assert!(glob("a*c", "abbbc"));
        assert!(glob("a*c", "ac"));
        assert!(!glob("a*c", "ab"));
        assert!(glob("?.corp", "x.corp"));
        assert!(!glob("?.corp", ".corp"));
        assert!(glob("*.*.corp", "a.b.corp"));
        assert!(!glob("host", "host2"));
        assert!(glob("", ""));
        assert!(!glob("", "x"));
    }
}
