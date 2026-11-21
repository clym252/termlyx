//! Pure-Rust vault cryptography for Tether (RustCrypto only).
//!
//! * [`kdf`]: Argon2id master derivation split by HKDF-SHA256 into an
//!   `AuthKey` and a key-encryption key (KEK).
//! * [`envelope`]: XChaCha20-Poly1305 record envelopes bound to
//!   `id|vault_id|kind|key_version` as associated data.
//! * [`keys`]: X25519 account keys, secret wrapping and the recovery key.
//! * [`hpke_wrap`]: the vault key HPKE-sealed to an account public key.
//!
//! The byte layouts are part of the on-disk and sync format; see
//! `docs/crypto.md`. Secret material lives in [`secrecy::SecretBox`] or
//! [`zeroize::Zeroizing`] and is never `Debug`-printed.

pub mod envelope;
mod error;
pub mod hpke_wrap;
pub mod kdf;
pub mod keys;

pub use envelope::{Aad, open, seal};
pub use error::CryptoError;
pub use hpke_wrap::{open_vault_key, seal_vault_key};
pub use kdf::{KdfParams, MasterKeys, derive_master};
pub use keys::{
    AccountKeys, generate_account, hello_key, keystore_key, new_recovery_key, parse_recovery_key,
    random_key, recovery_auth_key, unwrap_secret, wrap_secret,
};
