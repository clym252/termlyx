/// Errors returned by `core-crypto`. Variants carry no secret material.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CryptoError {
    /// The envelope's leading version byte is not one this build understands.
    #[error("unsupported envelope version")]
    BadVersion,
    /// Authentication failed: wrong key, wrong associated data, or tampered bytes.
    #[error("decryption failed")]
    Decrypt,
    /// Key derivation failed, typically because the parameters are invalid.
    #[error("key derivation failed: {0}")]
    Kdf(String),
    /// The input is structurally malformed (too short, bad encoding, bad length).
    #[error("malformed input")]
    Format,
}
