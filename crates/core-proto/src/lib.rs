//! Wire types shared by the Tether sync server and client, plus the invite and
//! add-device "join codes". No I/O and no crypto.
//!
//! # Conventions
//!
//! * JSON field names are camelCase.
//! * Every byte field in JSON (auth keys, envelopes, salts, wrapped secrets,
//!   tokens carried as bytes) is **base64, standard alphabet, with padding**
//!   (`data_encoding::BASE64`). There is no other byte encoding on the wire;
//!   only the join code uses base32 (see [`code`]).
//! * Opaque session tokens (`accessToken`, `refreshToken`) are plain strings
//!   chosen by the server. The access token must consist of HTTP token
//!   characters (no `/` or `=`) because it travels in a WebSocket
//!   subprotocol, see [`ws_bearer_protocol`].

pub mod api;
pub mod code;
pub mod short_code;

pub use api::*;
pub use code::{CodeError, CodeKind, JoinCode, fingerprint_text};
pub use short_code::{
    DEFAULT_PORT, ShortCode, client_proof, parse_address, server_proof, verify_client_proof,
    verify_server_proof,
};

/// Wire protocol major version. A server answers `update_client`/`update_server`
/// when the peer's major differs.
pub const PROTOCOL_VERSION: u32 = 1;

/// Request/response header carrying [`PROTOCOL_VERSION`]. HTTP header names are
/// case-insensitive; lowercase them where a library requires it.
pub const HEADER_PROTOCOL: &str = "X-Tether-Protocol";
/// The record kind set version this build of the protocol crate knows. Must
/// equal `core_model::KIND_SET_VERSION` (a test enforces it). The server
/// refuses an `X-Tether-Schema` far above it (more than 8 ahead), so a buggy
/// or hostile client cannot lock an account's other devices out of writing.
pub const KNOWN_SCHEMA: u32 = 3;
/// Request header carrying the client's record kind set version
/// (`core_model::KIND_SET_VERSION`).
pub const HEADER_SCHEMA: &str = "X-Tether-Schema";
/// Request header carrying the calling device's id.
pub const HEADER_DEVICE: &str = "X-Tether-Device";
/// Request header carrying the calling device's secret (`TokenResp.deviceSecret`),
/// required with [`HEADER_DEVICE`] for a password re-login.
pub const HEADER_DEVICE_SECRET: &str = "X-Tether-Device-Secret";

/// WebSocket subprotocol selecting protocol v1 event framing.
pub const WS_PROTOCOL: &str = "tether.v1";

const WS_BEARER_PREFIX: &str = "bearer.";

/// The WebSocket subprotocol string that carries an access token
/// (`bearer.<token>`), so the token never appears in a URL.
pub fn ws_bearer_protocol(token: &str) -> String {
    format!("{WS_BEARER_PREFIX}{token}")
}

/// Extract the token from a `bearer.<token>` subprotocol string. `None` when
/// the prefix is missing or the token is empty.
pub fn parse_ws_bearer_protocol(protocol: &str) -> Option<&str> {
    protocol
        .strip_prefix(WS_BEARER_PREFIX)
        .filter(|t| !t.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ws_bearer_round_trip() {
        assert_eq!(WS_PROTOCOL, "tether.v1");
        let p = ws_bearer_protocol("abc-DEF_123");
        assert_eq!(p, "bearer.abc-DEF_123");
        assert_eq!(parse_ws_bearer_protocol(&p), Some("abc-DEF_123"));
        assert_eq!(parse_ws_bearer_protocol("bearer."), None);
        assert_eq!(parse_ws_bearer_protocol("tether.v1"), None);
    }

    #[test]
    fn known_schema_matches_the_model() {
        assert_eq!(KNOWN_SCHEMA, core_model::KIND_SET_VERSION);
    }

    #[test]
    fn header_names() {
        assert_eq!(PROTOCOL_VERSION, 1);
        assert_eq!(HEADER_PROTOCOL, "X-Tether-Protocol");
        assert_eq!(HEADER_SCHEMA, "X-Tether-Schema");
        assert_eq!(HEADER_DEVICE, "X-Tether-Device");
        assert_eq!(HEADER_DEVICE_SECRET, "X-Tether-Device-Secret");
    }
}
