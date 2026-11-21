//! The vault key, HPKE-sealed (RFC 9180 base mode) to an account public key.
//!
//! Suite: DHKEM(X25519, HKDF-SHA256) / HKDF-SHA256 / ChaCha20-Poly1305.
//! Layout: `0x01 | enc(32) | ciphertext(32) | tag(16)`; HPKE `info` is
//! `tether/vault/v1|{vault_id}` and the HPKE AAD is empty.

use hpke::{
    Deserializable, OpModeR, OpModeS, Serializable, aead::ChaCha20Poly1305, kdf::HkdfSha256,
    kem::X25519HkdfSha256, single_shot_open, single_shot_seal,
};
use secrecy::{ExposeSecret, SecretBox};
use zeroize::Zeroizing;

use crate::CryptoError;

type Kem = X25519HkdfSha256;
type PublicKey = <Kem as hpke::Kem>::PublicKey;
type PrivateKey = <Kem as hpke::Kem>::PrivateKey;
type EncappedKey = <Kem as hpke::Kem>::EncappedKey;

/// Sealed vault key format version.
pub const VERSION: u8 = 0x01;
const ENC_LEN: usize = 32;
const TAG_LEN: usize = 16;
const VAULT_KEY_LEN: usize = 32;
/// Total length of a sealed vault key.
pub const SEALED_LEN: usize = 1 + ENC_LEN + VAULT_KEY_LEN + TAG_LEN;

fn info(vault_id: &str) -> Vec<u8> {
    format!("tether/vault/v1|{vault_id}").into_bytes()
}

/// Seals `vault_key` to `recipient_pub`. A fresh ephemeral key is used per call.
///
/// # Errors
///
/// [`CryptoError::Format`] if `recipient_pub` is not a usable X25519 public
/// key. RFC 9180 requires rejecting a recipient whose Diffie-Hellman output is
/// all zeros, so low-order points such as `[0u8; 32]` are refused.
///
/// # Panics
///
/// Panics if the OS RNG fails.
pub fn seal_vault_key(
    recipient_pub: &[u8; 32],
    vault_key: &[u8; 32],
    vault_id: &str,
) -> Result<Vec<u8>, CryptoError> {
    let pk = PublicKey::from_bytes(recipient_pub).map_err(|_| CryptoError::Format)?;
    let (enc, ct) = single_shot_seal::<ChaCha20Poly1305, HkdfSha256, Kem>(
        &OpModeS::Base,
        &pk,
        &info(vault_id),
        vault_key,
        &[],
    )
    .map_err(|_| CryptoError::Format)?;
    let mut out = Vec::with_capacity(SEALED_LEN);
    out.push(VERSION);
    out.extend_from_slice(&enc.to_bytes());
    out.extend_from_slice(&ct);
    Ok(out)
}

/// Opens a vault key sealed by [`seal_vault_key`].
///
/// # Errors
///
/// * [`CryptoError::Format`]: wrong length or an invalid encapsulated key.
/// * [`CryptoError::BadVersion`]: first byte is not `0x01`.
/// * [`CryptoError::Decrypt`]: wrong recipient, wrong `vault_id`, or tampering.
pub fn open_vault_key(
    recipient: &crate::AccountKeys,
    sealed: &[u8],
    vault_id: &str,
) -> Result<SecretBox<[u8; 32]>, CryptoError> {
    if sealed.len() < SEALED_LEN {
        return Err(CryptoError::Format);
    }
    if sealed[0] != VERSION {
        return Err(CryptoError::BadVersion);
    }
    if sealed.len() != SEALED_LEN {
        return Err(CryptoError::Format);
    }
    let (enc, ct) = sealed[1..].split_at(ENC_LEN);
    let enc = EncappedKey::from_bytes(enc).map_err(|_| CryptoError::Format)?;
    let sk = PrivateKey::from_bytes(recipient.secret.expose_secret())
        .map_err(|_| CryptoError::Format)?;
    let pt = Zeroizing::new(
        single_shot_open::<ChaCha20Poly1305, HkdfSha256, Kem>(
            &OpModeR::Base,
            &sk,
            &enc,
            &info(vault_id),
            ct,
            &[],
        )
        .map_err(|_| CryptoError::Decrypt)?,
    );
    let key: [u8; 32] = pt.as_slice().try_into().map_err(|_| CryptoError::Format)?;
    Ok(SecretBox::new(Box::new(key)))
}
