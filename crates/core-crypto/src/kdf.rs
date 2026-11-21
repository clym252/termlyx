//! Master-key derivation: Argon2id, then HKDF-SHA256 into two independent keys.

use argon2::{Algorithm, Argon2, Params, Version};
use hkdf::Hkdf;
use secrecy::{ExposeSecret, SecretBox, SecretString};
use sha2::Sha256;
use zeroize::Zeroizing;

use crate::CryptoError;

/// HKDF `info` for the server-facing authentication key.
pub const INFO_AUTH: &[u8] = b"tether/auth/v1";
/// HKDF `info` for the key-encryption key that wraps the account secret.
pub const INFO_KEK: &[u8] = b"tether/kek/v1";
/// HKDF `info` for the server-facing key derived from the recovery key.
pub const INFO_RECOVERY_AUTH: &[u8] = b"tether/recovery-auth/v1";
/// HKDF `info` for the key Windows Hello quick unlock derives from a Hello
/// signature (`docs/crypto.md`, "Quick unlock on Windows").
pub const INFO_HELLO_KEY: &[u8] = b"tether quick unlock windows hello v1";
/// HKDF `info` for the key Android quick unlock derives from a Keystore HMAC
/// (`docs/crypto.md`, "Quick unlock on Android").
pub const INFO_KEYSTORE_KEY: &[u8] = b"tether quick unlock android keystore v1";

/// Argon2id parameters, stored alongside the account so they can be raised later.
///
/// The salt is not secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KdfParams {
    /// Memory cost in KiB.
    pub m_kib: u32,
    /// Number of passes.
    pub t: u32,
    /// Degree of parallelism (lanes).
    pub p: u32,
    /// Random per-account salt.
    pub salt: [u8; 16],
}

impl KdfParams {
    /// Production memory cost: 64 MiB.
    pub const DEFAULT_M_KIB: u32 = 65536;
    /// Production pass count.
    pub const DEFAULT_T: u32 = 3;
    /// Production parallelism.
    pub const DEFAULT_P: u32 = 4;

    /// Production parameters (m = 64 MiB, t = 3, p = 4) with a fresh random salt.
    pub fn new_random() -> Self {
        let mut salt = [0u8; 16];
        crate::keys::fill_random(&mut salt);
        Self {
            m_kib: Self::DEFAULT_M_KIB,
            t: Self::DEFAULT_T,
            p: Self::DEFAULT_P,
            salt,
        }
    }
}

/// Keys derived from the master password.
#[derive(Debug)]
pub struct MasterKeys {
    /// Proves knowledge of the password to a server (unused until sync).
    pub auth_key: SecretBox<[u8; 32]>,
    /// Key-encryption key wrapping the account secret.
    pub kek: SecretBox<[u8; 32]>,
}

/// Inputs of one Argon2id invocation beyond the cost parameters.
///
/// `secret` and `associated_data` are the optional RFC 9106 inputs K and X;
/// Tether leaves both empty, but the RFC test vector uses them.
#[doc(hidden)]
pub struct Argon2Inputs<'a> {
    pub password: &'a [u8],
    pub salt: &'a [u8],
    pub secret: &'a [u8],
    pub associated_data: &'a [u8],
}

/// The single Argon2id configuration path (v1.3, Argon2id) used for derivation.
///
/// `out.len()` is the tag length. Exposed (hidden) so the RFC 9106 known-answer
/// test exercises exactly the code `derive_master` runs.
#[doc(hidden)]
pub fn argon2id_hash(
    m_kib: u32,
    t: u32,
    p: u32,
    input: &Argon2Inputs<'_>,
    out: &mut [u8],
) -> Result<(), CryptoError> {
    let kdf_err = |e: argon2::Error| CryptoError::Kdf(e.to_string());
    let mut builder = argon2::ParamsBuilder::new();
    builder
        .m_cost(m_kib)
        .t_cost(t)
        .p_cost(p)
        .output_len(out.len());
    if !input.associated_data.is_empty() {
        let data = argon2::AssociatedData::new(input.associated_data).map_err(kdf_err)?;
        builder.data(data);
    }
    let params: Params = builder.build().map_err(kdf_err)?;
    let ctx = if input.secret.is_empty() {
        Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
    } else {
        Argon2::new_with_secret(input.secret, Algorithm::Argon2id, Version::V0x13, params)
            .map_err(kdf_err)?
    };
    ctx.hash_password_into(input.password, input.salt, out)
        .map_err(kdf_err)
}

/// Derives the [`MasterKeys`] from a password.
///
/// `master = Argon2id(password, salt, m, t, p)` (32 bytes), then
/// `HKDF-SHA256(salt = none, ikm = master)` expands `tether/auth/v1` and
/// `tether/kek/v1` into 32 bytes each.
pub fn derive_master(password: &SecretString, p: &KdfParams) -> Result<MasterKeys, CryptoError> {
    let mut master = Zeroizing::new([0u8; 32]);
    argon2id_hash(
        p.m_kib,
        p.t,
        p.p,
        &Argon2Inputs {
            password: password.expose_secret().as_bytes(),
            salt: &p.salt,
            secret: &[],
            associated_data: &[],
        },
        master.as_mut_slice(),
    )?;

    let hk = Hkdf::<Sha256>::new(None, master.as_slice());
    let expand = |info: &[u8]| -> SecretBox<[u8; 32]> {
        let mut out = Zeroizing::new([0u8; 32]);
        hk.expand(info, out.as_mut_slice())
            .expect("32 bytes is a valid HKDF-SHA256 output length");
        SecretBox::new(Box::new(*out))
    };
    Ok(MasterKeys {
        auth_key: expand(INFO_AUTH),
        kek: expand(INFO_KEK),
    })
}
