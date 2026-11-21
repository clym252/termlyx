//! The one error type of the sync transport.
//!
//! `Display` strings are the plain messages the UI shows: no URLs, tokens or
//! response bodies ever appear in them.

use core_api::vault::VaultError;
use core_proto::{CodeError, ErrorBody, ErrorCode};

/// Why a sync call failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SyncError {
    /// The server presented a certificate that does not match the pin.
    #[error("server certificate changed")]
    CertificateChanged,
    /// The server at the typed address could not prove it knows the setup or
    /// invite code; nothing secret was sent to it.
    #[error("couldn't verify this server")]
    ServerUnverified,
    /// The server asked for cheaper password key derivation than this device
    /// accepts (`VaultService::kdf_floor`): a downgrade that would make the
    /// AuthKey cheap to brute-force. Nothing was derived or sent.
    #[error("the server asked for weaker password protection than Termlyx allows")]
    WeakKdf,
    /// The server could not be reached (connect, DNS, TLS or timeout). Carries
    /// a short reason that never contains a URL or a token.
    #[error("server unreachable")]
    Unreachable(String),
    /// The server revoked this device.
    #[error("device revoked")]
    Revoked,
    /// The session is not (or no longer) valid and could not be refreshed;
    /// sign in again.
    #[error("not authorized; sign in again")]
    Unauthorized,
    /// The server needs a newer client.
    #[error("update Termlyx")]
    UpdateClient,
    /// The client is newer than the server.
    #[error("the sync server is older than this app; update the server")]
    UpdateServer,
    /// This device's clock is too far from the server's.
    #[error("device clock is off; check the date and time")]
    ClockSkew,
    /// The cursor is older than the server's history; a full resync is needed.
    #[error("sync history is gone; a full resync is needed")]
    Gone,
    /// Too many requests; try again later.
    #[error("too many attempts; try again later")]
    RateLimited,
    /// Any other error the server reported.
    #[error("{message}")]
    Server {
        /// The server's machine-readable code.
        code: ErrorCode,
        /// The server's plain-language message.
        message: String,
    },
    /// The server's answer was not what the protocol says.
    #[error("unexpected response from the server: {0}")]
    Protocol(String),
    /// A join code was malformed.
    #[error("invalid code: {0}")]
    InvalidCode(String),
    /// The credential store (keychain or fallback) failed.
    #[error("could not access stored credentials: {0}")]
    Storage(String),
    /// The credential store is out of reach only until the device is
    /// unlocked (the iOS keychain while the phone is locked). Transient: never
    /// a reason to move the credentials elsewhere.
    #[error("stored credentials are unavailable while this device is locked")]
    StorageLocked,
    /// The master password is wrong (locally, or the server refused the
    /// AuthKey derived from it).
    #[error("wrong password")]
    WrongPassword,
    /// The recovery key is malformed or does not belong to this account.
    #[error("wrong recovery key")]
    WrongRecoveryKey,
    /// This device cannot sign in again with the password alone: its stored
    /// credentials are gone, or the server no longer knows the device. It
    /// must be added again with an add-device code.
    #[error("this device must be added to the account again with an add-device code")]
    NeedsDeviceCode,
    /// The vault is locked; unlock it first.
    #[error("the vault is locked")]
    Locked,
    /// This device already has a vault: joining would replace it (ask the
    /// user, then join with `replace_existing`).
    #[error("this device already has a vault")]
    VaultExists,
    /// The local vault failed (its own plain message).
    #[error("{0}")]
    Vault(String),
}

impl From<VaultError> for SyncError {
    fn from(e: VaultError) -> Self {
        match e {
            VaultError::WrongPassword => SyncError::WrongPassword,
            VaultError::WrongRecoveryKey => SyncError::WrongRecoveryKey,
            VaultError::Locked => SyncError::Locked,
            VaultError::AlreadyInitialized => SyncError::VaultExists,
            VaultError::WeakKdf => SyncError::WeakKdf,
            // VaultError's messages carry no secrets or decrypted fields.
            other => SyncError::Vault(other.to_string()),
        }
    }
}

impl SyncError {
    /// Map a server error body to the matching variant.
    pub fn from_body(body: ErrorBody) -> SyncError {
        match body.code {
            ErrorCode::UpdateClient => SyncError::UpdateClient,
            ErrorCode::UpdateServer => SyncError::UpdateServer,
            ErrorCode::ClockSkew => SyncError::ClockSkew,
            ErrorCode::Gone => SyncError::Gone,
            ErrorCode::RateLimited => SyncError::RateLimited,
            ErrorCode::Revoked => SyncError::Revoked,
            ErrorCode::Unauthorized => SyncError::Unauthorized,
            code => SyncError::Server {
                code,
                message: body.message,
            },
        }
    }
}

impl From<CodeError> for SyncError {
    fn from(e: CodeError) -> Self {
        match e {
            // A code from a newer app: this app cannot read it.
            CodeError::UnsupportedVersion(_) => SyncError::UpdateClient,
            other => SyncError::InvalidCode(other.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_strings_are_plain() {
        assert_eq!(
            SyncError::CertificateChanged.to_string(),
            "server certificate changed"
        );
        assert_eq!(SyncError::Revoked.to_string(), "device revoked");
        assert_eq!(SyncError::UpdateClient.to_string(), "update Termlyx");
        assert_eq!(
            SyncError::Unreachable("connection refused".into()).to_string(),
            "server unreachable"
        );
    }

    #[test]
    fn codes_map_to_variants() {
        let body = |code| ErrorBody {
            code,
            message: "m".into(),
        };
        assert_eq!(
            SyncError::from_body(body(ErrorCode::UpdateClient)),
            SyncError::UpdateClient
        );
        assert_eq!(
            SyncError::from_body(body(ErrorCode::UpdateServer)),
            SyncError::UpdateServer
        );
        assert_eq!(
            SyncError::from_body(body(ErrorCode::ClockSkew)),
            SyncError::ClockSkew
        );
        assert_eq!(SyncError::from_body(body(ErrorCode::Gone)), SyncError::Gone);
        assert_eq!(
            SyncError::from_body(body(ErrorCode::RateLimited)),
            SyncError::RateLimited
        );
        assert_eq!(
            SyncError::from_body(body(ErrorCode::Revoked)),
            SyncError::Revoked
        );
        assert_eq!(
            SyncError::from_body(body(ErrorCode::Conflict)),
            SyncError::Server {
                code: ErrorCode::Conflict,
                message: "m".into()
            }
        );
    }
}
