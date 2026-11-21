use thiserror::Error;

#[derive(Debug, Error)]
pub enum SshError {
    /// TCP connect failure, handshake failure or timeout.
    #[error("connect failed: {0}")]
    Connect(String),
    /// The server rejected our credentials, the key could not be decoded, or
    /// a prompt for a missing secret went unanswered.
    #[error("authentication failed")]
    AuthFailed,
    /// The host key verifier rejected the server's key.
    #[error("host key rejected")]
    HostKeyRejected,
    /// The host key verifier could not check the key (its trusted-key store
    /// is unavailable); the connection was refused without asking anyone.
    #[error("vault locked: host keys cannot be verified")]
    HostKeyUnverifiable,
    /// A host key (the server's, or a stored one) could not be parsed or
    /// encoded in OpenSSH form, so it was never shown to the verifier.
    #[error("host key could not be encoded: {0}")]
    HostKeyEncoding(String),
    /// A channel-level failure (pty/shell request, channel open, ...).
    #[error("channel error: {0}")]
    Channel(String),
    /// An SFTP request failed (the message names the operation and path).
    #[error("sftp: {0}")]
    Sftp(String),
    /// The shell/session is closed.
    #[error("session closed")]
    Closed,
}
