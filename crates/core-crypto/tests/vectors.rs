//! Behavioural and known-answer tests for `core-crypto`.

use core_crypto::kdf::{Argon2Inputs, argon2id_hash};
use core_crypto::{
    Aad, AccountKeys, CryptoError, KdfParams, derive_master, generate_account, hello_key,
    keystore_key, new_recovery_key, open, open_vault_key, parse_recovery_key, random_key,
    recovery_auth_key, seal, seal_vault_key, unwrap_secret, wrap_secret,
};
use proptest::prelude::*;
use secrecy::{ExposeSecret, SecretString};

const GOLDEN_KEY: [u8; 32] = [0x42; 32];
const GOLDEN_PLAINTEXT: &[u8] = b"tether golden envelope v1 \xe2\x9c\x93";
const GOLDEN_AAD: Aad<'static> = Aad {
    id: "0198a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b",
    vault_id: "0198a1b2-0000-7000-8000-000000000001",
    kind: "host",
    key_version: 1,
};

fn aad() -> Aad<'static> {
    Aad {
        id: "id-1",
        vault_id: "vault-1",
        kind: "host",
        key_version: 1,
    }
}

/// Small Argon2 cost for tests that are not checking the production parameters.
fn cheap(salt: [u8; 16]) -> KdfParams {
    KdfParams {
        m_kib: 64,
        t: 1,
        p: 1,
        salt,
    }
}

/// RFC 9106 section 5.3: Argon2id with secret K and associated data X.
#[test]
fn argon2id_rfc9106_vector() {
    let mut tag = [0u8; 32];
    argon2id_hash(
        32,
        3,
        4,
        &Argon2Inputs {
            password: &[0x01; 32],
            salt: &[0x02; 16],
            secret: &[0x03; 8],
            associated_data: &[0x04; 12],
        },
        &mut tag,
    )
    .unwrap();
    let expected: [u8; 32] = [
        0x0d, 0x64, 0x0d, 0xf5, 0x8d, 0x78, 0x76, 0x6c, 0x08, 0xc0, 0x37, 0xa3, 0x4a, 0x8b, 0x53,
        0xc9, 0xd0, 0x1e, 0xf0, 0x45, 0x2d, 0x75, 0xb6, 0x5e, 0xb5, 0x25, 0x20, 0xe9, 0x6b, 0x01,
        0xe6, 0x59,
    ];
    assert_eq!(tag, expected);
}

/// One run at the production cost: deterministic, and the two sub-keys differ.
#[test]
fn derive_master_is_deterministic_and_splits_keys() {
    let params = KdfParams {
        m_kib: 65536,
        t: 3,
        p: 4,
        salt: [7u8; 16],
    };
    let pw = SecretString::from("correct horse battery staple");
    let a = derive_master(&pw, &params).unwrap();
    let b = derive_master(&pw, &params).unwrap();
    assert_eq!(a.auth_key.expose_secret(), b.auth_key.expose_secret());
    assert_eq!(a.kek.expose_secret(), b.kek.expose_secret());
    assert_ne!(a.auth_key.expose_secret(), a.kek.expose_secret());

    let other = derive_master(&SecretString::from("different"), &cheap([7u8; 16])).unwrap();
    assert_ne!(other.kek.expose_secret(), a.kek.expose_secret());
}

/// Known answer at small cost. The HKDF split was cross-checked against an
/// independent HMAC-SHA256 implementation; the Argon2 stage by the RFC vector.
#[test]
fn derive_master_golden_v1() {
    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }
    let keys = derive_master(
        &SecretString::from("tether test password"),
        &KdfParams {
            m_kib: 64,
            t: 1,
            p: 1,
            salt: [0x5a; 16],
        },
    )
    .unwrap();
    assert_eq!(
        hex(keys.auth_key.expose_secret()),
        "02667b62a9233242bb13eca866e20754bda427e07fafffe3387f3d34e2429955"
    );
    assert_eq!(
        hex(keys.kek.expose_secret()),
        "ff3ae68a5204cbf3d0efec047f846d8fcf8d9eb6eebf1307c75432d17331bd93"
    );
}

#[test]
fn derive_master_depends_on_salt() {
    let pw = SecretString::from("pw");
    let a = derive_master(&pw, &cheap([1u8; 16])).unwrap();
    let b = derive_master(&pw, &cheap([2u8; 16])).unwrap();
    assert_ne!(a.kek.expose_secret(), b.kek.expose_secret());
}

#[test]
fn derive_master_rejects_invalid_params() {
    let bad = KdfParams {
        m_kib: 1,
        t: 3,
        p: 4,
        salt: [0u8; 16],
    };
    let err = derive_master(&SecretString::from("pw"), &bad).unwrap_err();
    assert!(matches!(err, CryptoError::Kdf(_)), "{err:?}");
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(24))]

    #[test]
    fn seal_open_roundtrip(pt in proptest::collection::vec(any::<u8>(), 0..65536)) {
        let key = [9u8; 32];
        let env = seal(&key, &aad(), &pt);
        prop_assert_eq!(env[0], 0x01);
        prop_assert_eq!(env.len(), 1 + 24 + pt.len() + 16);
        let out = open(&key, &aad(), &env).unwrap();
        prop_assert_eq!(&out[..], &pt[..]);
    }

    #[test]
    fn tamper_any_byte_fails(
        pt in proptest::collection::vec(any::<u8>(), 0..256),
        idx in any::<prop::sample::Index>(),
        flip in 1u8..=255,
    ) {
        let key = [9u8; 32];
        let mut env = seal(&key, &aad(), &pt);
        let i = idx.index(env.len());
        env[i] ^= flip;
        let err = open(&key, &aad(), &env).unwrap_err();
        if i == 0 {
            prop_assert_eq!(err, CryptoError::BadVersion);
        } else {
            prop_assert_eq!(err, CryptoError::Decrypt);
        }
    }
}

#[test]
fn nonce_is_random_per_seal() {
    let key = [9u8; 32];
    let a = seal(&key, &aad(), b"same");
    let b = seal(&key, &aad(), b"same");
    assert_ne!(a[1..25], b[1..25]);
    assert_ne!(a, b);
}

#[test]
fn aad_mismatch_fails() {
    let key = [9u8; 32];
    let env = seal(&key, &aad(), b"secret");
    let variants = [
        Aad {
            id: "id-2",
            ..aad()
        },
        Aad {
            vault_id: "vault-2",
            ..aad()
        },
        Aad {
            kind: "group",
            ..aad()
        },
        Aad {
            key_version: 2,
            ..aad()
        },
    ];
    for v in &variants {
        assert_eq!(open(&key, v, &env).unwrap_err(), CryptoError::Decrypt);
    }
    assert_eq!(
        open(&[1u8; 32], &aad(), &env).unwrap_err(),
        CryptoError::Decrypt
    );
}

#[test]
fn unknown_version_rejected() {
    let key = [9u8; 32];
    let mut env = seal(&key, &aad(), b"secret");
    env[0] = 0x02;
    assert_eq!(
        open(&key, &aad(), &env).unwrap_err(),
        CryptoError::BadVersion
    );
}

#[test]
fn short_input_is_format_error() {
    let key = [9u8; 32];
    assert_eq!(open(&key, &aad(), &[]).unwrap_err(), CryptoError::Format);
    assert_eq!(
        open(&key, &aad(), &[0x01; 40]).unwrap_err(),
        CryptoError::Format
    );
    // Exactly nonce + tag with an empty ciphertext is the minimum, and must
    // reach decryption (and fail authentication), not the length check.
    assert_eq!(
        open(&key, &aad(), &[0x01; 41]).unwrap_err(),
        CryptoError::Decrypt
    );
}

#[test]
#[should_panic(expected = "'|'")]
fn seal_rejects_delimiter_in_aad_field() {
    let _ = seal(&[0u8; 32], &Aad { id: "a|b", ..aad() }, b"x");
}

#[test]
fn open_rejects_delimiter_in_aad_field() {
    let env = seal(&[0u8; 32], &aad(), b"x");
    let bad = Aad { id: "a|b", ..aad() };
    assert_eq!(
        open(&[0u8; 32], &bad, &env).unwrap_err(),
        CryptoError::Format
    );
}

#[test]
fn golden_envelope_v1() {
    let env = include_bytes!("golden/envelope_v1.bin");
    assert_eq!(env[0], 0x01);
    let pt = open(&GOLDEN_KEY, &GOLDEN_AAD, env).expect("golden envelope must keep decrypting");
    assert_eq!(&pt[..], GOLDEN_PLAINTEXT);
}

/// Generators are no-ops unless `TETHER_REGEN_GOLDEN=1`, so `--include-ignored`
/// cannot silently rewrite the committed fixtures.
fn regen_enabled() -> bool {
    std::env::var("TETHER_REGEN_GOLDEN").as_deref() == Ok("1")
}

/// One-off fixture generator:
/// `TETHER_REGEN_GOLDEN=1 cargo test -p core-crypto --test vectors -- --ignored --exact generate_golden`
#[test]
#[ignore = "regenerates tests/golden/envelope_v1.bin; only run to bump the format"]
fn generate_golden() {
    if !regen_enabled() {
        return;
    }
    let env = seal(&GOLDEN_KEY, &GOLDEN_AAD, GOLDEN_PLAINTEXT);
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden/envelope_v1.bin");
    std::fs::write(path, env).unwrap();
}

const GOLDEN_ACCOUNT_SECRET: [u8; 32] = [0x24; 32];
const GOLDEN_VAULT_KEY: [u8; 32] = [0x77; 32];
const GOLDEN_VAULT_ID: &str = "0198a1b2-0000-7000-8000-000000000001";

#[test]
fn golden_sealed_vault_key_v1() {
    let sealed = include_bytes!("golden/vault_key_v1.bin");
    let account = AccountKeys::from_secret(GOLDEN_ACCOUNT_SECRET);
    let key = open_vault_key(&account, sealed, GOLDEN_VAULT_ID)
        .expect("golden sealed vault key must keep decrypting");
    assert_eq!(key.expose_secret(), &GOLDEN_VAULT_KEY);
}

/// One-off fixture generator:
/// `TETHER_REGEN_GOLDEN=1 cargo test -p core-crypto --test vectors -- --ignored --exact generate_golden_vault_key`
#[test]
#[ignore = "regenerates tests/golden/vault_key_v1.bin; only run to bump the format"]
fn generate_golden_vault_key() {
    if !regen_enabled() {
        return;
    }
    let account = AccountKeys::from_secret(GOLDEN_ACCOUNT_SECRET);
    let sealed = seal_vault_key(&account.public, &GOLDEN_VAULT_KEY, GOLDEN_VAULT_ID).unwrap();
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden/vault_key_v1.bin");
    std::fs::write(path, sealed).unwrap();
}

#[test]
fn wrap_unwrap_secret_roundtrip() {
    let kek = [5u8; 32];
    let secret = [6u8; 32];
    let wrapped = wrap_secret(&kek, &secret, "account-key");
    assert_eq!(wrapped.len(), 1 + 24 + 32 + 16);
    let out = unwrap_secret(&kek, &wrapped, "account-key").unwrap();
    assert_eq!(out.expose_secret(), &secret);
    assert_eq!(
        unwrap_secret(&kek, &wrapped, "other-label").unwrap_err(),
        CryptoError::Decrypt
    );
    assert_eq!(
        unwrap_secret(&[0u8; 32], &wrapped, "account-key").unwrap_err(),
        CryptoError::Decrypt
    );
}

#[test]
fn vault_key_hpke_roundtrip_and_wrong_recipient_fails() {
    let alice = generate_account();
    let bob = generate_account();
    assert_ne!(alice.public, bob.public);
    let vault_key = random_key();

    let sealed = seal_vault_key(&alice.public, vault_key.expose_secret(), "vault-1").unwrap();
    assert_eq!(sealed[0], 0x01);
    assert_eq!(sealed.len(), 1 + 32 + 32 + 16);

    let opened = open_vault_key(&alice, &sealed, "vault-1").unwrap();
    assert_eq!(opened.expose_secret(), vault_key.expose_secret());

    assert_eq!(
        open_vault_key(&bob, &sealed, "vault-1").unwrap_err(),
        CryptoError::Decrypt
    );
    // The vault id is bound through the HPKE info string.
    assert_eq!(
        open_vault_key(&alice, &sealed, "vault-2").unwrap_err(),
        CryptoError::Decrypt
    );
    // Every seal uses a fresh ephemeral key.
    let again = seal_vault_key(&alice.public, vault_key.expose_secret(), "vault-1").unwrap();
    assert_ne!(sealed, again);
}

#[test]
fn seal_vault_key_rejects_low_order_recipient() {
    for bad in [[0u8; 32], {
        let mut one = [0u8; 32];
        one[0] = 1;
        one
    }] {
        assert!(seal_vault_key(&bad, &[1u8; 32], "v").is_err());
    }
}

#[test]
fn vault_key_envelope_errors() {
    let alice = generate_account();
    let sealed = seal_vault_key(&alice.public, &[3u8; 32], "v").unwrap();
    let mut bad_version = sealed.clone();
    bad_version[0] = 0x02;
    assert_eq!(
        open_vault_key(&alice, &bad_version, "v").unwrap_err(),
        CryptoError::BadVersion
    );
    assert_eq!(
        open_vault_key(&alice, &sealed[..40], "v").unwrap_err(),
        CryptoError::Format
    );
    let mut tampered = sealed;
    let last = tampered.len() - 1;
    tampered[last] ^= 1;
    assert_eq!(
        open_vault_key(&alice, &tampered, "v").unwrap_err(),
        CryptoError::Decrypt
    );
}

#[test]
fn account_public_matches_secret() {
    // Rebuilding the account from the stored secret yields the same public key.
    let a = generate_account();
    let rebuilt = AccountKeys::from_secret(*a.secret.expose_secret());
    assert_eq!(rebuilt.public, a.public);
}

#[test]
fn recovery_key_roundtrip_ignores_spaces_and_case() {
    let (key, text) = new_recovery_key();
    assert_eq!(text.len(), 52 + 12); // 52 base32 chars in 13 groups of 4
    assert!(text.split('-').all(|g| g.len() == 4));
    assert_eq!(text, text.to_uppercase());

    let parsed = parse_recovery_key(&text).unwrap();
    assert_eq!(parsed.expose_secret(), key.expose_secret());

    let messy = format!("  {}  ", text.to_lowercase().replace('-', " "));
    let parsed = parse_recovery_key(&messy).unwrap();
    assert_eq!(parsed.expose_secret(), key.expose_secret());

    let compact = text.replace('-', "");
    let parsed = parse_recovery_key(&compact).unwrap();
    assert_eq!(parsed.expose_secret(), key.expose_secret());
}

#[test]
fn recovery_key_rejects_bad_input() {
    let (_, text) = new_recovery_key();
    assert_eq!(parse_recovery_key("").unwrap_err(), CryptoError::Format);
    assert_eq!(
        parse_recovery_key(&text[..text.len() - 5]).unwrap_err(),
        CryptoError::Format
    );
    assert_eq!(
        parse_recovery_key(&format!("{text}AAAA")).unwrap_err(),
        CryptoError::Format
    );
    // '1', '0', '8' and '9' are not in the RFC 4648 base32 alphabet.
    let invalid = "1".repeat(52);
    assert_eq!(
        parse_recovery_key(&invalid).unwrap_err(),
        CryptoError::Format
    );
}

#[test]
fn recovery_key_wraps_account_secret() {
    let kek = derive_master(&SecretString::from("pw"), &cheap([4u8; 16]))
        .unwrap()
        .kek;
    let account = generate_account();
    let (recovery, text) = new_recovery_key();

    let by_kek = wrap_secret(
        kek.expose_secret(),
        account.secret.expose_secret(),
        "account",
    );
    let by_recovery = wrap_secret(
        recovery.expose_secret(),
        account.secret.expose_secret(),
        "account",
    );

    let back = parse_recovery_key(&text).unwrap();
    let a = unwrap_secret(kek.expose_secret(), &by_kek, "account").unwrap();
    let b = unwrap_secret(back.expose_secret(), &by_recovery, "account").unwrap();
    assert_eq!(a.expose_secret(), account.secret.expose_secret());
    assert_eq!(b.expose_secret(), account.secret.expose_secret());
}

#[test]
fn secrets_are_not_debug_printed() {
    let account = generate_account();
    let rendered = format!("{account:?}");
    let secret_hex: String = account
        .secret
        .expose_secret()
        .iter()
        .map(|b| format!("{b:?}"))
        .collect::<Vec<_>>()
        .join(", ");
    assert!(!rendered.contains(&secret_hex));
    assert!(rendered.contains("REDACTED"));
}

/// HMAC-SHA256 (RFC 2104) written out by hand over bare SHA-256, so the
/// recovery-auth known answer does not depend on the `hkdf` crate.
fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut k = [0u8; 64];
    k[..key.len()].copy_from_slice(key); // keys here are at most 32 bytes
    let mut inner = Sha256::new();
    inner.update(k.map(|b| b ^ 0x36));
    inner.update(msg);
    let mut outer = Sha256::new();
    outer.update(k.map(|b| b ^ 0x5c));
    outer.update(inner.finalize());
    outer.finalize().into()
}

/// `RecoveryAuthKey = HKDF-Expand(HKDF-Extract(none, recovery),
/// "tether/recovery-auth/v1", 32)`. The pinned value was computed from the
/// hand-written HMAC above (HKDF-Extract with a zero salt, then the single
/// Expand block `T(1) = HMAC(PRK, info || 0x01)`) and cross-checked against
/// Python's `hmac`.
#[test]
fn recovery_auth_key_kat() {
    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }
    let recovery = [0x42u8; 32];
    let prk = hmac_sha256(&[0u8; 32], &recovery);
    let mut block = b"tether/recovery-auth/v1".to_vec();
    block.push(0x01);
    let by_hand = hmac_sha256(&prk, &block);

    let got = recovery_auth_key(&recovery);
    assert_eq!(got.expose_secret(), &by_hand);
    assert_eq!(
        hex(got.expose_secret()),
        "35ae02c78ba0576e6d622f54e2d3fe90a00924a87ef1351fe43a7fe6fc79cfc5"
    );
}

/// Windows Hello quick unlock's HelloKey (`docs/crypto.md`, "Quick unlock on
/// Windows"): `HKDF-SHA256(salt = none, IKM = signature)` expanded with
/// "tether quick unlock windows hello v1", 32 bytes. The input stands in for
/// a 2048-bit RSA signature; the pinned value was cross-checked against
/// Python's `hmac`.
#[test]
fn hello_key_kat() {
    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }
    let signature = [0x5au8; 256];
    let prk = hmac_sha256(&[0u8; 32], &signature);
    let mut block = b"tether quick unlock windows hello v1".to_vec();
    block.push(0x01);
    let by_hand = hmac_sha256(&prk, &block);

    let got = hello_key(&signature);
    assert_eq!(got.expose_secret(), &by_hand);
    assert_eq!(
        hex(got.expose_secret()),
        "62cc64b0488677d86953db006ec45b47855cd448287f0d8e344e832e0fa94109"
    );
}

/// Android quick unlock's wrapping key (`docs/crypto.md`, "Quick unlock on
/// Android"): `HKDF-SHA256(salt = none, IKM = mac)` expanded with
/// "tether quick unlock android keystore v1", 32 bytes. The input stands in
/// for the Keystore's 32-byte HMAC-SHA256 of the challenge; the pinned value
/// was cross-checked against Python's `hmac`.
#[test]
fn keystore_key_kat() {
    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }
    let mac = [0x5au8; 32];
    let prk = hmac_sha256(&[0u8; 32], &mac);
    let mut block = b"tether quick unlock android keystore v1".to_vec();
    block.push(0x01);
    let by_hand = hmac_sha256(&prk, &block);

    let got = keystore_key(&mac);
    assert_eq!(got.expose_secret(), &by_hand);
    assert_eq!(
        hex(got.expose_secret()),
        "2e62a087827a9f81b3a541295a1ed80b61f0b89c448e5ce0c2a0bdfaa49729d4"
    );
    // Its own info string: never Windows Hello's key for the same input.
    assert_ne!(got.expose_secret(), hello_key(&mac).expose_secret());
}

/// The server-facing value must not be the key that unwraps the account secret.
#[test]
fn recovery_auth_differs_from_wrap_key() {
    let recovery = random_key();
    let auth = recovery_auth_key(recovery.expose_secret());
    assert_ne!(auth.expose_secret(), recovery.expose_secret());
}
