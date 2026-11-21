//! Record envelopes: `0x01 | nonce(24) | ciphertext | tag(16)`.

use chacha20poly1305::{
    Key, KeyInit, XChaCha20Poly1305, XNonce,
    aead::{Aead, Payload},
};
use zeroize::Zeroizing;

use crate::CryptoError;

/// Envelope format version written by [`seal`].
pub const VERSION: u8 = 0x01;
/// XChaCha20-Poly1305 nonce length.
pub const NONCE_LEN: usize = 24;
/// Poly1305 tag length.
pub const TAG_LEN: usize = 16;
/// Shortest valid envelope: version, nonce and tag around an empty plaintext.
pub const MIN_LEN: usize = 1 + NONCE_LEN + TAG_LEN;

/// Associated data binding a ciphertext to where it belongs.
///
/// Serialized as `id|vault_id|kind|key_version`: the four fields as UTF-8,
/// joined with `|`, `key_version` in decimal. Text fields must not contain
/// `|` (ids are UUIDs and kinds are fixed snake_case words), which keeps the
/// encoding unambiguous; [`seal`] panics and [`open`] returns
/// [`CryptoError::Format`] if one does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Aad<'a> {
    /// Record id.
    pub id: &'a str,
    /// Owning vault id.
    pub vault_id: &'a str,
    /// Record kind (`host`, `group`, ...).
    pub kind: &'a str,
    /// Version of the key the record is sealed under.
    pub key_version: u32,
}

impl Aad<'_> {
    fn to_bytes(self) -> Option<Vec<u8>> {
        if [self.id, self.vault_id, self.kind]
            .iter()
            .any(|f| f.contains('|'))
        {
            return None;
        }
        Some(
            format!(
                "{}|{}|{}|{}",
                self.id, self.vault_id, self.kind, self.key_version
            )
            .into_bytes(),
        )
    }
}

fn cipher(key: &[u8; 32]) -> XChaCha20Poly1305 {
    XChaCha20Poly1305::new(&Key::from(*key))
}

/// Encrypts `plaintext` under `key`, binding `aad`. A fresh random nonce is
/// drawn from the OS for every call.
///
/// # Panics
///
/// Panics if an `aad` text field contains `|`, or if the OS RNG fails.
pub fn seal(key: &[u8; 32], aad: &Aad, plaintext: &[u8]) -> Vec<u8> {
    let aad_bytes = aad
        .to_bytes()
        .expect("AAD fields must not contain the '|' delimiter");
    let mut nonce = [0u8; NONCE_LEN];
    crate::keys::fill_random(&mut nonce);
    let ct = cipher(key)
        .encrypt(
            &XNonce::from(nonce),
            Payload {
                msg: plaintext,
                aad: &aad_bytes,
            },
        )
        .expect("XChaCha20-Poly1305 encryption of an in-memory buffer cannot fail");
    let mut out = Vec::with_capacity(1 + NONCE_LEN + ct.len());
    out.push(VERSION);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    out
}

/// Decrypts an envelope produced by [`seal`].
///
/// # Errors
///
/// * [`CryptoError::Format`]: shorter than [`MIN_LEN`], or `aad` contains `|`.
/// * [`CryptoError::BadVersion`]: first byte is not `0x01`.
/// * [`CryptoError::Decrypt`]: wrong key, wrong `aad`, or modified bytes.
pub fn open(key: &[u8; 32], aad: &Aad, envelope: &[u8]) -> Result<Zeroizing<Vec<u8>>, CryptoError> {
    if envelope.len() < MIN_LEN {
        return Err(CryptoError::Format);
    }
    if envelope[0] != VERSION {
        return Err(CryptoError::BadVersion);
    }
    let aad_bytes = aad.to_bytes().ok_or(CryptoError::Format)?;
    let (nonce, ct) = envelope[1..].split_at(NONCE_LEN);
    let nonce: [u8; NONCE_LEN] = nonce.try_into().map_err(|_| CryptoError::Format)?;
    cipher(key)
        .decrypt(
            &XNonce::from(nonce),
            Payload {
                msg: ct,
                aad: &aad_bytes,
            },
        )
        .map(Zeroizing::new)
        .map_err(|_| CryptoError::Decrypt)
}
