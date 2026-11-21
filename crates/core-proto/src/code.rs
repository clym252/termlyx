//! Join codes: the text a user pastes (or scans) to invite a new account or add
//! a device. `tether1-` + lowercase unpadded RFC 4648 base32 of
//! `[version=1][kind 0|1][has_pin 0|1][pin 32?][token 32][url_len u16 LE][url]`.

use std::net::{Ipv4Addr, Ipv6Addr};

use data_encoding::{BASE32_NOPAD, HEXLOWER};

const PREFIX: &str = "tether1-";
const VERSION: u8 = 1;

/// What a code redeems.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeKind {
    /// Single-use invite to create an account.
    Invite,
    /// Single-use link token to add a device to an existing account.
    Link,
}

/// Why a join code was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CodeError {
    #[error("not a Termlyx code")]
    BadPrefix,
    #[error("code is not valid base32")]
    BadEncoding,
    #[error("code is truncated")]
    Truncated,
    #[error("unsupported code version {0}")]
    UnsupportedVersion(u8),
    #[error("unknown code kind {0}")]
    BadKind(u8),
    #[error("bad flag byte {0}")]
    BadFlag(u8),
    #[error("code has trailing bytes")]
    TrailingBytes,
    #[error("server address is not valid")]
    BadUrl,
    #[error("server address must be https")]
    InsecureUrl,
}

/// A parsed join code.
#[derive(Clone, PartialEq, Eq)]
pub struct JoinCode {
    pub kind: CodeKind,
    /// Server base URL, `https://...` (or loopback `http://` when unpinned).
    pub url: String,
    /// SHA-256 of the server certificate's SPKI, when the server uses a
    /// self-signed certificate.
    pub pin: Option<[u8; 32]>,
    /// Single-use invite or link token.
    pub token: [u8; 32],
}

impl std::fmt::Debug for JoinCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "JoinCode({:?}, {}, pin: {})",
            self.kind,
            self.url,
            self.pin.is_some()
        )
    }
}

impl JoinCode {
    /// The text form users copy around.
    ///
    /// # Panics
    ///
    /// If `url` is longer than 65535 bytes (the length field is a `u16`).
    pub fn encode(&self) -> String {
        let url_len = u16::try_from(self.url.len()).expect("join code url longer than 65535 bytes");
        let mut b = Vec::with_capacity(3 + 32 + 32 + 2 + self.url.len());
        b.push(VERSION);
        b.push(match self.kind {
            CodeKind::Invite => 0,
            CodeKind::Link => 1,
        });
        b.push(u8::from(self.pin.is_some()));
        if let Some(pin) = &self.pin {
            b.extend_from_slice(pin);
        }
        b.extend_from_slice(&self.token);
        b.extend_from_slice(&url_len.to_le_bytes());
        b.extend_from_slice(self.url.as_bytes());
        format!("{PREFIX}{}", BASE32_NOPAD.encode(&b).to_lowercase())
    }

    /// Parse code text. Whitespace anywhere and letter case are ignored.
    pub fn parse(text: &str) -> Result<JoinCode, CodeError> {
        let compact: String = text
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
            .to_lowercase();
        let body = compact.strip_prefix(PREFIX).ok_or(CodeError::BadPrefix)?;
        let bytes = BASE32_NOPAD
            .decode(body.to_uppercase().as_bytes())
            .map_err(|_| CodeError::BadEncoding)?;

        let mut r = Reader(&bytes);
        let version = r.u8()?;
        if version != VERSION {
            return Err(CodeError::UnsupportedVersion(version));
        }
        let kind = match r.u8()? {
            0 => CodeKind::Invite,
            1 => CodeKind::Link,
            k => return Err(CodeError::BadKind(k)),
        };
        let pin = match r.u8()? {
            0 => None,
            1 => Some(r.array()?),
            f => return Err(CodeError::BadFlag(f)),
        };
        let token = r.array()?;
        let url_len = u16::from_le_bytes(r.array()?) as usize;
        let url = std::str::from_utf8(r.take(url_len)?).map_err(|_| CodeError::BadUrl)?;
        if !r.0.is_empty() {
            return Err(CodeError::TrailingBytes);
        }
        check_url(url, pin.is_some())?;
        Ok(JoinCode {
            kind,
            url: url.to_owned(),
            pin,
            token,
        })
    }
}

/// Cursor over the decoded payload.
struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], CodeError> {
        if self.0.len() < n {
            return Err(CodeError::Truncated);
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(head)
    }

    fn u8(&mut self) -> Result<u8, CodeError> {
        Ok(self.take(1)?[0])
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], CodeError> {
        Ok(self.take(N)?.try_into().expect("take returned N bytes"))
    }
}

/// `https://` always; `http://` only for a loopback host in a code with no pin.
pub(crate) fn check_url(url: &str, has_pin: bool) -> Result<(), CodeError> {
    let lower = url.to_ascii_lowercase();
    let (secure, rest) = if let Some(rest) = lower.strip_prefix("https://") {
        (true, rest)
    } else if let Some(rest) = lower.strip_prefix("http://") {
        (false, rest)
    } else {
        return Err(CodeError::InsecureUrl);
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty() || authority.contains('@') {
        // No host, or userinfo that could disguise the real host.
        return Err(CodeError::BadUrl);
    }
    if secure {
        return Ok(());
    }
    if !has_pin && is_loopback_authority(authority) {
        Ok(())
    } else {
        Err(CodeError::InsecureUrl)
    }
}

/// Host part of `host[:port]` / `[v6][:port]` is `localhost` or a loopback IP.
fn is_loopback_authority(authority: &str) -> bool {
    let host = if let Some(v6) = authority.strip_prefix('[') {
        match v6.split_once(']') {
            Some((h, _)) => h,
            None => return false,
        }
    } else {
        authority.split(':').next().unwrap_or("")
    };
    host == "localhost"
        || host.parse::<Ipv4Addr>().is_ok_and(|a| a.is_loopback())
        || host.parse::<Ipv6Addr>().is_ok_and(|a| a.is_loopback())
}

/// The text for a server certificate pin: `sha256:` and the 32 bytes as
/// lowercase hex, no separators. `termlyx-server fingerprint` prints exactly
/// this and the app shows exactly this, so the user can compare them.
pub fn fingerprint_text(pin: &[u8; 32]) -> String {
    format!("sha256:{}", HEXLOWER.encode(pin))
}

#[cfg(test)]
mod tests {
    #[test]
    fn fingerprint_text_is_sha256_and_lowercase_hex() {
        let mut pin = [0u8; 32];
        pin[0] = 0xab;
        pin[31] = 0xff;
        assert_eq!(
            super::fingerprint_text(&pin),
            format!("sha256:ab{}ff", "00".repeat(30))
        );
    }

    use super::*;

    fn sample(kind: CodeKind, pin: Option<[u8; 32]>) -> JoinCode {
        JoinCode {
            kind,
            url: "https://sync.example.com".into(),
            pin,
            token: [0x22; 32],
        }
    }

    #[test]
    fn join_code_round_trip() {
        for code in [
            sample(CodeKind::Invite, Some([0x11; 32])),
            sample(CodeKind::Invite, None),
            sample(CodeKind::Link, Some([0x33; 32])),
            sample(CodeKind::Link, None),
        ] {
            let text = code.encode();
            assert!(text.starts_with("tether1-"));
            assert_eq!(text, text.to_lowercase());
            assert!(!text.contains('='));
            assert_eq!(JoinCode::parse(&text).unwrap(), code);
        }
    }

    #[test]
    fn join_code_golden() {
        let invite = JoinCode::parse(include_str!("../tests/golden/invite.txt")).unwrap();
        assert_eq!(invite, sample(CodeKind::Invite, Some([0x11; 32])));
        let link = JoinCode::parse(include_str!("../tests/golden/link.txt")).unwrap();
        assert_eq!(link, sample(CodeKind::Link, None));
        // The encoder reproduces the committed text byte for byte.
        assert_eq!(
            invite.encode(),
            include_str!("../tests/golden/invite.txt").trim()
        );
        assert_eq!(
            link.encode(),
            include_str!("../tests/golden/link.txt").trim()
        );
    }

    /// Build code text from raw payload bytes, bypassing `encode`.
    fn raw(payload: &[u8]) -> String {
        format!(
            "tether1-{}",
            data_encoding::BASE32_NOPAD.encode(payload).to_lowercase()
        )
    }

    fn payload(version: u8, kind: u8, pin: Option<u8>, url: &str) -> Vec<u8> {
        let mut b = vec![version, kind, u8::from(pin.is_some())];
        if let Some(p) = pin {
            b.extend([p; 32]);
        }
        b.extend([0x22; 32]);
        b.extend((url.len() as u16).to_le_bytes());
        b.extend(url.as_bytes());
        b
    }

    #[test]
    fn join_code_rejects_bad_version() {
        let text = raw(&payload(2, 0, Some(1), "https://a.example"));
        assert_eq!(
            JoinCode::parse(&text),
            Err(CodeError::UnsupportedVersion(2))
        );
        // And a version this build does know still parses.
        assert!(JoinCode::parse(&raw(&payload(1, 0, Some(1), "https://a.example"))).is_ok());
    }

    #[test]
    fn join_code_rejects_plain_http_remote() {
        // Remote host, with and without a pin.
        for pin in [None, Some(1)] {
            let text = raw(&payload(1, 0, pin, "http://tether.example.com"));
            assert_eq!(JoinCode::parse(&text), Err(CodeError::InsecureUrl));
        }
        // Loopback over http is fine only without a pin.
        for url in [
            "http://localhost:8080",
            "http://127.0.0.1:8080",
            "http://[::1]:8080",
        ] {
            assert!(
                JoinCode::parse(&raw(&payload(1, 1, None, url))).is_ok(),
                "{url}"
            );
            assert_eq!(
                JoinCode::parse(&raw(&payload(1, 1, Some(1), url))),
                Err(CodeError::InsecureUrl)
            );
        }
        // Host tricks do not count as loopback.
        for url in [
            "http://localhost.evil.example",
            "http://127.0.0.1.evil.example",
        ] {
            assert_eq!(
                JoinCode::parse(&raw(&payload(1, 0, None, url))),
                Err(CodeError::InsecureUrl),
                "{url}"
            );
        }
        // Userinfo can disguise the host, so it is rejected outright.
        assert_eq!(
            JoinCode::parse(&raw(&payload(1, 0, None, "http://localhost@evil.example"))),
            Err(CodeError::BadUrl)
        );
        // Other schemes are rejected.
        assert_eq!(
            JoinCode::parse(&raw(&payload(1, 0, None, "ftp://a.example"))),
            Err(CodeError::InsecureUrl)
        );
    }

    #[test]
    fn join_code_ignores_case_and_spaces() {
        let code = sample(CodeKind::Invite, Some([0x11; 32]));
        let text = code.encode();
        let mut mangled = String::new();
        for (i, c) in text.to_uppercase().chars().enumerate() {
            mangled.push(c);
            if i % 5 == 4 {
                mangled.push_str(" \n\t");
            }
        }
        let mangled = format!("  {mangled}  ");
        assert_eq!(JoinCode::parse(&mangled).unwrap(), code);
    }

    #[test]
    fn join_code_rejects_malformed() {
        let good = sample(CodeKind::Invite, Some([0x11; 32])).encode();
        assert_eq!(JoinCode::parse(""), Err(CodeError::BadPrefix));
        assert_eq!(JoinCode::parse("tether2-aaaa"), Err(CodeError::BadPrefix));
        assert_eq!(JoinCode::parse("tether1-!!!"), Err(CodeError::BadEncoding));
        // Truncated at every length shorter than complete.
        for n in 8..good.len() - 1 {
            assert!(JoinCode::parse(&good[..n]).is_err(), "len {n}");
        }
        // Bad kind, bad flag, trailing bytes, non-UTF-8 url, empty url.
        assert_eq!(
            JoinCode::parse(&raw(&payload(1, 7, None, "https://a.example"))),
            Err(CodeError::BadKind(7))
        );
        let mut bad_flag = payload(1, 0, None, "https://a.example");
        bad_flag[2] = 2;
        assert_eq!(JoinCode::parse(&raw(&bad_flag)), Err(CodeError::BadFlag(2)));
        let mut trailing = payload(1, 0, None, "https://a.example");
        trailing.push(0);
        assert_eq!(
            JoinCode::parse(&raw(&trailing)),
            Err(CodeError::TrailingBytes)
        );
        let mut non_utf8 = payload(1, 0, None, "https://a.example");
        let n = non_utf8.len();
        non_utf8[n - 1] = 0xff;
        assert_eq!(JoinCode::parse(&raw(&non_utf8)), Err(CodeError::BadUrl));
        assert_eq!(
            JoinCode::parse(&raw(&payload(1, 0, None, "https://"))),
            Err(CodeError::BadUrl)
        );
    }

    #[test]
    fn debug_hides_token() {
        let dbg = format!("{:?}", sample(CodeKind::Invite, None));
        assert!(!dbg.contains("34"), "{dbg}");
        assert!(dbg.contains("sync.example.com"));
    }
}
