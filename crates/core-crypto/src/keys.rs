//! Account keypair, secret wrapping and the recovery key.

use data_encoding::BASE32_NOPAD;
use hkdf::Hkdf;
use hpke::{Deserializable, Kem as _, Serializable, kem::X25519HkdfSha256};
use secrecy::{ExposeSecret, SecretBox};
use sha2::Sha256;
use zeroize::{Zeroize, Zeroizing};

use crate::{
    CryptoError,
    envelope::{Aad, open, seal},
    kdf::{INFO_HELLO_KEY, INFO_KEYSTORE_KEY, INFO_RECOVERY_AUTH},
};

/// Fills `buf` from the OS CSPRNG.
///
/// # Panics
///
/// Panics if the OS RNG is unavailable; there is no safe way to continue.
pub(crate) fn fill_random(buf: &mut [u8]) {
    getrandom::fill(buf).expect("OS random number generator failed");
}

fn secret_box(bytes: [u8; 32]) -> SecretBox<[u8; 32]> {
    SecretBox::new(Box::new(bytes))
}

/// 32 fresh random bytes as a secret (vault keys, recovery keys).
pub fn random_key() -> SecretBox<[u8; 32]> {
    let mut k = Zeroizing::new([0u8; 32]);
    fill_random(k.as_mut_slice());
    secret_box(*k)
}

/// An X25519 account keypair. `public` is shareable; `secret` is wrapped by the
/// KEK and the recovery key before it is persisted.
#[derive(Debug)]
pub struct AccountKeys {
    /// X25519 public key (32 bytes, RFC 7748 encoding).
    pub public: [u8; 32],
    /// X25519 private key.
    pub secret: SecretBox<[u8; 32]>,
}

impl AccountKeys {
    /// Rebuilds the keypair from a stored private key (for example after
    /// [`unwrap_secret`]), recomputing the public half.
    pub fn from_secret(secret: [u8; 32]) -> Self {
        let mut secret = Zeroizing::new(secret);
        let sk = <<X25519HkdfSha256 as hpke::Kem>::PrivateKey as Deserializable>::from_bytes(
            secret.as_slice(),
        )
        .expect("any 32 bytes are a valid X25519 private key");
        let pk = X25519HkdfSha256::sk_to_pk(&sk);
        let mut public = [0u8; 32];
        public.copy_from_slice(&pk.to_bytes());
        let account = Self {
            public,
            secret: secret_box(*secret),
        };
        secret.zeroize();
        account
    }
}

/// Generates a new random X25519 account keypair.
pub fn generate_account() -> AccountKeys {
    let mut sk = Zeroizing::new([0u8; 32]);
    fill_random(sk.as_mut_slice());
    AccountKeys::from_secret(*sk)
}

fn wrap_aad(label: &str) -> Aad<'_> {
    Aad {
        id: label,
        vault_id: "",
        kind: "wrap",
        key_version: 1,
    }
}

/// Encrypts a 32-byte secret under `kek` as an ordinary envelope whose AAD is
/// `label||wrap|1` (`id = label`, empty `vault_id`, `kind = "wrap"`,
/// `key_version = 1`).
///
/// # Panics
///
/// Panics if `label` contains `|` (see [`Aad`]), or if the OS RNG fails.
pub fn wrap_secret(kek: &[u8; 32], secret: &[u8; 32], label: &str) -> Vec<u8> {
    seal(kek, &wrap_aad(label), secret)
}

/// Reverses [`wrap_secret`].
///
/// # Errors
///
/// Envelope errors from [`open`], plus [`CryptoError::Format`] if the
/// plaintext is not exactly 32 bytes.
pub fn unwrap_secret(
    kek: &[u8; 32],
    wrapped: &[u8],
    label: &str,
) -> Result<SecretBox<[u8; 32]>, CryptoError> {
    let pt = open(kek, &wrap_aad(label), wrapped)?;
    let bytes: [u8; 32] = pt.as_slice().try_into().map_err(|_| CryptoError::Format)?;
    Ok(secret_box(bytes))
}

/// Number of base32 characters in an unpadded encoding of 32 bytes.
const RECOVERY_CHARS: usize = 52;

/// Generates a recovery key and its human-readable form: RFC 4648 base32
/// without padding, uppercase, in groups of four joined by `-`
/// (`ABCD-EFGH-...`, 13 groups, 64 characters).
pub fn new_recovery_key() -> (SecretBox<[u8; 32]>, String) {
    let key = random_key();
    let encoded = Zeroizing::new(BASE32_NOPAD.encode(key.expose_secret()));
    let text = encoded
        .as_bytes()
        .chunks(4)
        .map(|c| std::str::from_utf8(c).expect("base32 output is ASCII"))
        .collect::<Vec<_>>()
        .join("-");
    (key, text)
}

/// Derives the key a client presents to the server to prove it holds the
/// recovery key: `HKDF-Expand(HKDF-Extract(salt = none, IKM = recovery),
/// "tether/recovery-auth/v1", 32)`.
///
/// The recovery key itself wraps the account secret, so it never leaves the
/// device; this one-way derivation is what the server may see (and verify).
pub fn recovery_auth_key(recovery: &[u8; 32]) -> SecretBox<[u8; 32]> {
    let hk = Hkdf::<Sha256>::new(None, recovery);
    let mut out = Zeroizing::new([0u8; 32]);
    hk.expand(INFO_RECOVERY_AUTH, out.as_mut_slice())
        .expect("32 bytes is a valid HKDF-SHA256 output length");
    secret_box(*out)
}

/// Derives Windows Hello quick unlock's HelloKey from the Hello signature of
/// the stored challenge: `HKDF-Expand(HKDF-Extract(salt = none, IKM =
/// signature), "tether quick unlock windows hello v1", 32)`. The caller
/// zeroizes `signature` afterwards.
pub fn hello_key(signature: &[u8]) -> SecretBox<[u8; 32]> {
    quick_unlock_key(signature, INFO_HELLO_KEY)
}

/// Derives Android quick unlock's wrapping key from the Keystore key's
/// HMAC-SHA256 of the stored challenge: `HKDF-Expand(HKDF-Extract(salt =
/// none, IKM = mac), "tether quick unlock android keystore v1", 32)`. The
/// caller zeroizes `mac` afterwards.
pub fn keystore_key(mac: &[u8]) -> SecretBox<[u8; 32]> {
    quick_unlock_key(mac, INFO_KEYSTORE_KEY)
}

fn quick_unlock_key(ikm: &[u8], info: &[u8]) -> SecretBox<[u8; 32]> {
    let hk = Hkdf::<Sha256>::new(None, ikm);
    let mut out = Zeroizing::new([0u8; 32]);
    hk.expand(info, out.as_mut_slice())
        .expect("32 bytes is a valid HKDF-SHA256 output length");
    secret_box(*out)
}

/// Parses a recovery key. Whitespace and `-` are ignored and letters are
/// case-insensitive.
///
/// # Errors
///
/// [`CryptoError::Format`] for characters outside the base32 alphabet or any
/// length other than 52 significant characters.
pub fn parse_recovery_key(s: &str) -> Result<SecretBox<[u8; 32]>, CryptoError> {
    let cleaned: Zeroizing<String> = Zeroizing::new(
        s.chars()
            .filter(|c| !c.is_whitespace() && *c != '-')
            .map(|c| c.to_ascii_uppercase())
            .collect(),
    );
    if cleaned.len() != RECOVERY_CHARS {
        return Err(CryptoError::Format);
    }
    let decoded = Zeroizing::new(
        BASE32_NOPAD
            .decode(cleaned.as_bytes())
            .map_err(|_| CryptoError::Format)?,
    );
    let bytes: [u8; 32] = decoded
        .as_slice()
        .try_into()
        .map_err(|_| CryptoError::Format)?;
    Ok(secret_box(bytes))
}
