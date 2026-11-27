//! Short setup/invite codes (`TLX-XXXX-XXXX-XXXX-XXXX`) and server address
//! parsing. 10 random bytes shown as 16 Crockford-style base32 symbols. The
//! server stores only [`ShortCode::key`] (the invite's `token_hash`); the code
//! itself is proved with HMACs, never sent. See `docs/protocol.md`.

use std::net::Ipv6Addr;
use std::sync::LazyLock;

use data_encoding::{Encoding, Specification};
use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};

use crate::code::{CodeError, check_url};

/// Port assumed when an address has none.
pub const DEFAULT_PORT: u16 = 8443;

const SYMBOLS: &str = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";

static ALPHABET: LazyLock<Encoding> = LazyLock::new(|| {
    let mut spec = Specification::new();
    spec.symbols.push_str(SYMBOLS);
    spec.encoding().expect("valid alphabet")
});

/// A short code. `Debug` hides it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ShortCode([u8; 10]);

impl std::fmt::Debug for ShortCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ShortCode(..)")
    }
}

impl std::fmt::Display for ShortCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = ALPHABET.encode(&self.0);
        write!(
            f,
            "TLX-{}-{}-{}-{}",
            &s[0..4],
            &s[4..8],
            &s[8..12],
            &s[12..16]
        )
    }
}

fn sha256(parts: &[&[u8]]) -> [u8; 32] {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p);
    }
    h.finalize().into()
}

impl ShortCode {
    pub fn from_bytes(b: [u8; 10]) -> Self {
        Self(b)
    }

    /// Parse user input: case, spaces, dashes and a leading `TLX` are optional;
    /// `O` reads as `0`, `I` and `L` as `1`.
    pub fn parse(text: &str) -> Result<Self, CodeError> {
        let mut s: String = text
            .chars()
            .filter(|c| !c.is_whitespace() && *c != '-')
            .collect::<String>()
            .to_ascii_uppercase();
        if s.len() > 16 {
            s = s
                .strip_prefix("TLX")
                .ok_or(CodeError::BadEncoding)?
                .to_owned();
        }
        let s: String = s
            .chars()
            .map(|c| match c {
                'O' => '0',
                'I' | 'L' => '1',
                c => c,
            })
            .collect();
        if s.len() != 16 {
            return Err(CodeError::BadEncoding);
        }
        let bytes = ALPHABET
            .decode(s.as_bytes())
            .map_err(|_| CodeError::BadEncoding)?;
        Ok(Self(bytes.try_into().map_err(|_| CodeError::BadEncoding)?))
    }

    /// The invite token this code stands for.
    pub fn token(&self) -> [u8; 32] {
        sha256(&[b"termlyx-short-code-v1", &self.0])
    }

    /// `SHA256(token)`: equals the stored `invites.token_hash`; the HMAC key.
    pub fn key(&self) -> [u8; 32] {
        sha256(&[&self.token()])
    }

    /// Public lookup id, safe to send.
    pub fn id(&self) -> [u8; 16] {
        sha256(&[b"termlyx-code-id-v1", &self.key()])[..16]
            .try_into()
            .expect("16 bytes")
    }
}

fn proof_mac(label: &[u8], key: &[u8; 32], binding: &[u8], nonce: &[u8; 32]) -> Hmac<Sha256> {
    let mut m = <Hmac<Sha256> as KeyInit>::new_from_slice(key).expect("hmac takes any key length");
    m.update(label);
    m.update(binding);
    m.update(nonce);
    m
}

fn proof(label: &[u8], key: &[u8; 32], binding: &[u8], nonce: &[u8; 32]) -> [u8; 32] {
    proof_mac(label, key, binding, nonce)
        .finalize()
        .into_bytes()
        .into()
}

const SERVER_LABEL: &[u8] = b"termlyx-code-server-v1";
const CLIENT_LABEL: &[u8] = b"termlyx-code-client-v1";

/// The binding of an unpinned proof: `host:port` of `url` (a
/// [`parse_address`] result), lowercased, so a relay at another address
/// cannot pass the real server's proof on.
pub fn host_binding(url: &str) -> String {
    url.strip_prefix("https://").unwrap_or(url).to_ascii_lowercase()
}

/// What the server returns to show it knows the code. `binding` is the 32-byte
/// SPKI pin when the client pinned, else the [`host_binding`] the client sent
/// (empty from older clients).
pub fn server_proof(key: &[u8; 32], binding: &[u8], client_nonce: &[u8; 32]) -> [u8; 32] {
    proof(SERVER_LABEL, key, binding, client_nonce)
}

/// What the client sends to redeem the code.
pub fn client_proof(key: &[u8; 32], binding: &[u8], client_nonce: &[u8; 32]) -> [u8; 32] {
    proof(CLIENT_LABEL, key, binding, client_nonce)
}

/// Is `got` the [`server_proof`] for these inputs? Constant-time.
pub fn verify_server_proof(
    key: &[u8; 32],
    binding: &[u8],
    client_nonce: &[u8; 32],
    got: &[u8],
) -> bool {
    proof_mac(SERVER_LABEL, key, binding, client_nonce)
        .verify_slice(got)
        .is_ok()
}

/// Is `got` the [`client_proof`] for these inputs? Constant-time.
pub fn verify_client_proof(
    key: &[u8; 32],
    binding: &[u8],
    client_nonce: &[u8; 32],
    got: &[u8],
) -> bool {
    proof_mac(CLIENT_LABEL, key, binding, client_nonce)
        .verify_slice(got)
        .is_ok()
}

/// Turn what a user typed (`host`, `host:port`, `https://host:port/`, bare or
/// bracketed IPv6) into `https://host:port`.
pub fn parse_address(input: &str) -> Result<String, CodeError> {
    let t = input.trim();
    let rest = match t.get(..8).filter(|p| p.eq_ignore_ascii_case("https://")) {
        Some(_) => &t[8..],
        None => t,
    };
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    if rest.to_ascii_lowercase().starts_with("http://") {
        return Err(CodeError::InsecureUrl);
    }
    if rest.is_empty() || rest.contains(|c: char| c.is_whitespace() || "/@?#".contains(c)) {
        return Err(CodeError::BadUrl);
    }
    let (host, port) = if let Some(v6) = rest.strip_prefix('[') {
        let (h, tail) = v6.split_once(']').ok_or(CodeError::BadUrl)?;
        match tail {
            "" => (h, None),
            t => (h, Some(t.strip_prefix(':').ok_or(CodeError::BadUrl)?)),
        }
    } else if rest.matches(':').count() > 1 {
        (rest, None) // bare IPv6
    } else {
        match rest.split_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (rest, None),
        }
    };
    let port = match port {
        Some(p) => p
            .parse::<u16>()
            .ok()
            .filter(|p| *p != 0)
            .ok_or(CodeError::BadUrl)?,
        None => DEFAULT_PORT,
    };
    let host = if host.contains(':') {
        format!(
            "[{}]",
            host.parse::<Ipv6Addr>().map_err(|_| CodeError::BadUrl)?
        )
    } else if !host.is_empty()
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
    {
        host.to_owned()
    } else {
        return Err(CodeError::BadUrl);
    };
    let url = format!("https://{host}:{port}");
    check_url(&url, false)?;
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;
    use data_encoding::HEXLOWER;

    #[test]
    fn short_code_round_trips() {
        let c = ShortCode::from_bytes([7; 10]);
        let t = c.to_string();
        assert!(t.starts_with("TLX-") && t.len() == 23);
        assert_eq!(ShortCode::parse(&t).unwrap(), c);
    }

    #[test]
    fn short_code_forgiving_input() {
        // Contains both `0` and `1`, so the look-alike substitutions are exercised.
        let c = ShortCode::from_bytes([0x08, 0x42, 0x10, 0x84, 0x21, 0x00, 0x42, 0x10, 0x84, 0x21]);
        let t = c.to_string();
        assert!(t.contains('0') && t.contains('1'), "{t}");
        for v in [
            t.replace('0', "o"),
            t.replace('1', "l"),
            t.replace('1', "L"),
            t.replace('1', "i"),
            t.to_lowercase(),
            t.replace('-', ""),
            t.replace('-', " "),
            t[4..].to_string(),
            t.replace('0', "O").replace('1', "I"),
        ] {
            assert_eq!(ShortCode::parse(&v).unwrap(), c, "{v}");
        }
    }

    #[test]
    fn short_code_rejects() {
        for bad in [
            "",
            "TLX-",
            "TLX-ABCD",
            "TLX-ABCD-EFGH-JKMN-PQRSU",
            "TLX-ABCD-EFGH-JKMN-PQR!",
        ] {
            assert!(ShortCode::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn derivations_are_stable() {
        let c = ShortCode::from_bytes([1; 10]);
        assert_eq!(
            HEXLOWER.encode(&c.token()),
            "bec9bec403dcb94c300b5d9aa2a28f524a629be17da910fd7d143114e6713d38"
        );
        assert_eq!(
            HEXLOWER.encode(&c.key()),
            "d0b1afce266e3ab93c64b04da65da9c63568c20807915f43d22da12c3e17c357"
        );
        assert_eq!(HEXLOWER.encode(&c.id()), "c98c922a13e8a939302e853fb81e35ec");
    }

    #[test]
    fn debug_hides_code() {
        let c = ShortCode::from_bytes([1; 10]);
        assert!(!format!("{c:?}").contains(&c.to_string()[4..8]));
    }

    #[test]
    fn proofs_depend_on_binding_and_side() {
        let k = [3; 32];
        let n = [4; 32];
        assert_ne!(server_proof(&k, &[], &n), client_proof(&k, &[], &n));
        assert_ne!(
            server_proof(&k, &[9; 32], &n),
            server_proof(&k, &[8; 32], &n)
        );
    }

    #[test]
    fn verify_server_proof_checks_every_input() {
        let (k, b, n) = ([3; 32], [9; 32], [4; 32]);
        let p = server_proof(&k, &b, &n);
        assert!(verify_server_proof(&k, &b, &n, &p));
        assert!(!verify_server_proof(&k, &[8; 32], &n, &p));
        assert!(!verify_server_proof(&k, &b, &[5; 32], &p));
        assert!(!verify_server_proof(&[2; 32], &b, &n, &p));
        assert!(!verify_server_proof(&k, &b, &n, &client_proof(&k, &b, &n)));
        assert!(!verify_server_proof(&k, &b, &n, &p[..31]));
    }

    #[test]
    fn verify_client_proof_checks_every_input() {
        let (k, n) = ([3; 32], [4; 32]);
        let p = client_proof(&k, &[], &n);
        assert!(verify_client_proof(&k, &[], &n, &p));
        assert!(!verify_client_proof(&k, &[9; 32], &n, &p));
        assert!(!verify_client_proof(&k, &[], &[5; 32], &p));
        assert!(!verify_client_proof(&[2; 32], &[], &n, &p));
        assert!(!verify_client_proof(
            &k,
            &[],
            &n,
            &server_proof(&k, &[], &n)
        ));
        assert!(!verify_client_proof(&k, &[], &n, &[]));
    }

    #[test]
    fn address_forms() {
        for (i, o) in [
            ("192.168.1.20", "https://192.168.1.20:8443"),
            (" nas.local ", "https://nas.local:8443"),
            ("https://nas.local:9000/", "https://nas.local:9000"),
            ("nas.local:9000", "https://nas.local:9000"),
            ("fd00::5", "https://[fd00::5]:8443"),
            ("[fd00::5]:9000", "https://[fd00::5]:9000"),
        ] {
            assert_eq!(parse_address(i).unwrap(), o);
        }
        assert_eq!(
            host_binding(&parse_address("NAS.Local").unwrap()),
            "nas.local:8443"
        );
        assert_eq!(
            host_binding(&parse_address("[FD00::5]:9000").unwrap()),
            "[fd00::5]:9000"
        );
        for bad in [
            "",
            "http://nas.local",
            "nas.local:0",
            "nas.local:99999",
            "nas.local/path",
            "a b",
            "[fd00::5]9000",
            "[fd00::5]junk",
        ] {
            assert!(parse_address(bad).is_err(), "{bad}");
        }
    }
}
