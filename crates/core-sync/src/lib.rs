//! Tether sync, client side.
//!
//! * Transport: a pinned-TLS HTTP client for every server endpoint
//!   ([`Client`]), the events WebSocket ([`ws::connect`]), the device
//!   credential store ([`TokenStore`], with a fallback into the vault
//!   database, [`SyncStateTokenStore`]) and the error type ([`SyncError`]).
//! * The engine ([`start`], [`SyncHandle`]): pull, merge, push and keep the
//!   account material current, with live triggers and a status.
//! * The account flows ([`flows`]): enable, join, add a device, password
//!   change, recovery, turn off.
//!
//! No Tauri types: the desktop app and (later) mobile share this crate.

pub mod client;
mod engine;
pub mod error;
mod fallback;
pub mod flows;
mod state;
pub mod tls;
pub mod tokens;
pub mod ws;

pub use client::Client;
pub use core_proto::{Event, JoinCode};
#[cfg(feature = "test-util")]
pub use engine::{Engine, TickOutcome};
pub use engine::{SyncHandle, start};
pub use error::SyncError;
pub use fallback::SyncStateTokenStore;
pub use state::{SyncConfig, SyncErrorKind, SyncNotice, SyncStatus, is_enabled};
pub use tls::{Pin, client_config};
pub use tokens::{DeviceCredentials, KeyringTokenStore, MemoryTokenStore, TokenStore};

/// Log filter directives every binary that embeds this crate must apply
/// **after** the user's own filter (`RUST_LOG`), so nothing the user sets can
/// lower them.
///
/// Why: tungstenite logs the raw WebSocket upgrade request at `trace`, and
/// that request carries the access token (`Sec-WebSocket-Protocol:
/// tether.v1, bearer.<token>`); reqwest's connection-verbose mode logs raw
/// bytes at `trace` too. Marking a header value sensitive does not help, the
/// logger formats the serialized bytes. With `RUST_LOG=trace` live tokens
/// would land in the log. Capping the HTTP/WebSocket stack at `info` keeps
/// request contents out of it. In tracing-subscriber, apply them with
/// `filter.add_directive(d.parse()?)` for each entry; a directive for the
/// same target replaces the user's.
pub const LOG_DIRECTIVES: &[&str] = &[
    "tungstenite=info",
    "tokio_tungstenite=info",
    "reqwest=info",
    "hyper=info",
    "hyper_util=info",
];

/// Parse a pasted join code (invite or add-device). A code written by a newer
/// app (unsupported version) is [`SyncError::UpdateClient`]; any other defect
/// is [`SyncError::InvalidCode`].
pub fn parse_join_code(text: &str) -> Result<JoinCode, SyncError> {
    Ok(JoinCode::parse(text)?)
}
