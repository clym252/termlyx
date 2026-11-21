//! SSH key generation and import. Output is always OpenSSH-format text so the
//! rest of Tether (vault, transport) deals with a single private-key format.

use russh::keys::key::safe_rng;
use russh::keys::ssh_key::private::{KeypairData, RsaKeypair};
use russh::keys::ssh_key::{self, LineEnding};
use russh::keys::{Algorithm, EcdsaCurve, HashAlg, PrivateKey, decode_secret_key};
use secrecy::{ExposeSecret, SecretString};
use thiserror::Error;

/// Key types the keychain can generate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyAlgo {
    Ed25519,
    EcdsaP256,
    EcdsaP384,
    EcdsaP521,
    Rsa2048,
    Rsa4096,
}

/// A key in the form the keychain stores it. Maps onto `core_model::SshKey`.
#[derive(Debug, Clone)]
pub struct GeneratedKey {
    /// OpenSSH private key PEM. Encrypted iff a passphrase was supplied.
    pub private_openssh: SecretString,
    /// `algo base64 [comment]` public key line.
    pub public_openssh: String,
    /// `SHA256:` followed by unpadded base64 (OpenSSH format).
    pub sha256_fingerprint: String,
    /// OpenSSH algorithm name, e.g. `ssh-ed25519`.
    pub algorithm: String,
}

#[derive(Debug, Error)]
pub enum KeyError {
    #[error("key is encrypted and needs a passphrase")]
    NeedsPassphrase,
    #[error("wrong passphrase")]
    BadPassphrase,
    #[error("unsupported key: {0}")]
    Unsupported(String),
    #[error("could not parse key: {0}")]
    Parse(String),
}

/// Generates a key. With a passphrase the private key is encrypted
/// (`aes256-ctr` + bcrypt KDF, the OpenSSH default); the public line carries
/// `comment` when it is non-empty.
pub fn generate(
    algo: KeyAlgo,
    comment: &str,
    passphrase: Option<&SecretString>,
) -> Result<GeneratedKey, KeyError> {
    let mut rng = safe_rng();
    let mut key = match algo {
        KeyAlgo::Ed25519 => PrivateKey::random(&mut rng, Algorithm::Ed25519),
        KeyAlgo::EcdsaP256 => PrivateKey::random(&mut rng, ecdsa(EcdsaCurve::NistP256)),
        KeyAlgo::EcdsaP384 => PrivateKey::random(&mut rng, ecdsa(EcdsaCurve::NistP384)),
        KeyAlgo::EcdsaP521 => PrivateKey::random(&mut rng, ecdsa(EcdsaCurve::NistP521)),
        KeyAlgo::Rsa2048 => rsa(2048),
        KeyAlgo::Rsa4096 => rsa(4096),
    }
    .map_err(ssh_err)?;
    key.set_comment(comment);
    describe(key, passphrase)
}

/// Imports a private key from OpenSSH, PKCS#1 (`BEGIN RSA PRIVATE KEY`),
/// SEC1 (`BEGIN EC PRIVATE KEY`) or PKCS#8 (`BEGIN [ENCRYPTED] PRIVATE KEY`)
/// text and re-encodes it as OpenSSH.
///
/// `passphrase` is the passphrase of the *input*. If the input is encrypted
/// the output is re-encrypted with that same passphrase; if it is not, the
/// passphrase is ignored and the output is unencrypted. (The vault encrypts
/// stored keys regardless.) A missing passphrase gives `NeedsPassphrase`, a
/// wrong one `BadPassphrase`.
pub fn import(text: &str, passphrase: Option<&SecretString>) -> Result<GeneratedKey, KeyError> {
    let header = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or_default();
    let (key, was_encrypted) = if header == "-----BEGIN OPENSSH PRIVATE KEY-----" {
        decode_openssh(text, passphrase)?
    } else if header.starts_with("-----BEGIN ") && header.ends_with("PRIVATE KEY-----") {
        decode_pem(text, header, passphrase)?
    } else {
        return Err(KeyError::Parse("not a recognised private key".into()));
    };
    describe(key, if was_encrypted { passphrase } else { None })
}

fn ecdsa(curve: EcdsaCurve) -> Algorithm {
    Algorithm::Ecdsa { curve }
}

fn rsa(bits: usize) -> ssh_key::Result<PrivateKey> {
    let pair = RsaKeypair::random(&mut safe_rng(), bits)?;
    PrivateKey::new(KeypairData::Rsa(pair), "")
}

fn ssh_err(e: ssh_key::Error) -> KeyError {
    match e {
        ssh_key::Error::AlgorithmUnknown | ssh_key::Error::AlgorithmUnsupported { .. } => {
            KeyError::Unsupported(e.to_string())
        }
        e => KeyError::Parse(e.to_string()),
    }
}

fn decode_openssh(
    text: &str,
    passphrase: Option<&SecretString>,
) -> Result<(PrivateKey, bool), KeyError> {
    let key = PrivateKey::from_openssh(text).map_err(ssh_err)?;
    if !key.is_encrypted() {
        return Ok((key, false));
    }
    let passphrase = passphrase.ok_or(KeyError::NeedsPassphrase)?;
    let key = key
        .decrypt(passphrase.expose_secret())
        .map_err(|_| KeyError::BadPassphrase)?;
    Ok((key, true))
}

/// PKCS#1 / SEC1 / PKCS#8 PEM, decoded by russh (the same decoder the
/// transport uses for stored keys).
fn decode_pem(
    text: &str,
    header: &str,
    passphrase: Option<&SecretString>,
) -> Result<(PrivateKey, bool), KeyError> {
    let legacy_encrypted = text.lines().any(|l| l.trim() == "Proc-Type: 4,ENCRYPTED");
    if legacy_encrypted
        && !text
            .lines()
            .any(|l| l.trim().starts_with("DEK-Info: AES-128-CBC,"))
    {
        return Err(KeyError::Unsupported(
            "legacy PEM encryption other than AES-128-CBC".into(),
        ));
    }
    let encrypted = legacy_encrypted || header == "-----BEGIN ENCRYPTED PRIVATE KEY-----";
    if !encrypted {
        let key = decode_secret_key(text, None).map_err(russh_err)?;
        return Ok((key, false));
    }
    let passphrase = passphrase.ok_or(KeyError::NeedsPassphrase)?;
    // The framing was checked above, so a failure here is almost always a
    // wrong passphrase (bad padding / MAC); report it as one.
    let key = decode_secret_key(text, Some(passphrase.expose_secret()))
        .map_err(|_| KeyError::BadPassphrase)?;
    Ok((key, true))
}

fn russh_err(e: russh::keys::Error) -> KeyError {
    use russh::keys::Error as E;
    match e {
        E::UnsupportedKeyType { .. } | E::UnknownAlgorithm(_) => {
            KeyError::Unsupported(e.to_string())
        }
        E::KeyIsEncrypted => KeyError::NeedsPassphrase,
        e => KeyError::Parse(e.to_string()),
    }
}

/// Builds the stored form of `key`, encrypting the private half when a
/// passphrase is given.
fn describe(
    mut key: PrivateKey,
    passphrase: Option<&SecretString>,
) -> Result<GeneratedKey, KeyError> {
    let public = key.public_key();
    let public_openssh = public.to_openssh().map_err(ssh_err)?;
    let sha256_fingerprint = public.fingerprint(HashAlg::Sha256).to_string();
    let algorithm = public.algorithm().to_string();
    if let Some(p) = passphrase {
        key = key
            .encrypt(&mut safe_rng(), p.expose_secret())
            .map_err(ssh_err)?;
    }
    let pem = key.to_openssh(LineEnding::LF).map_err(ssh_err)?;
    Ok(GeneratedKey {
        private_openssh: SecretString::from(pem.as_str().to_owned()),
        public_openssh,
        sha256_fingerprint,
        algorithm,
    })
}

#[cfg(test)]
mod tests {
    use std::process::Command;
    use std::sync::Arc;
    use std::time::Duration;

    use bytes::Bytes;
    use secrecy::ExposeSecret;

    use super::*;
    use crate::{AcceptAll, AuthMethod, ConnectParams, RusshTransport, SshTransport};

    fn pw(s: &str) -> SecretString {
        SecretString::from(s.to_string())
    }

    fn fingerprint_of_public(line: &str) -> String {
        ssh_key::PublicKey::from_openssh(line)
            .expect("public line parses")
            .fingerprint(ssh_key::HashAlg::Sha256)
            .to_string()
    }

    #[test]
    fn generate_each_algo_and_reimport() {
        let cases = [
            (KeyAlgo::Ed25519, "ssh-ed25519"),
            (KeyAlgo::EcdsaP256, "ecdsa-sha2-nistp256"),
            (KeyAlgo::EcdsaP384, "ecdsa-sha2-nistp384"),
            (KeyAlgo::EcdsaP521, "ecdsa-sha2-nistp521"),
            (KeyAlgo::Rsa2048, "ssh-rsa"),
            (KeyAlgo::Rsa4096, "ssh-rsa"),
        ];
        for (algo, name) in cases {
            let k = generate(algo, "me@laptop", None).expect("generate");
            assert_eq!(k.algorithm, name, "{algo:?}");
            assert!(k.sha256_fingerprint.starts_with("SHA256:"), "{algo:?}");
            assert!(!k.sha256_fingerprint.contains('='), "{algo:?}");
            assert!(
                k.private_openssh
                    .expose_secret()
                    .starts_with("-----BEGIN OPENSSH PRIVATE KEY-----"),
                "{algo:?}"
            );
            assert!(k.public_openssh.starts_with(name), "{algo:?}");
            assert!(k.public_openssh.ends_with(" me@laptop"), "{algo:?}");
            assert_eq!(
                fingerprint_of_public(&k.public_openssh),
                k.sha256_fingerprint,
                "{algo:?}"
            );

            let again = import(k.private_openssh.expose_secret(), None).expect("reimport");
            assert_eq!(again.sha256_fingerprint, k.sha256_fingerprint, "{algo:?}");
            assert_eq!(again.algorithm, k.algorithm, "{algo:?}");
            assert_eq!(again.public_openssh, k.public_openssh, "{algo:?}");
        }
    }

    #[test]
    fn encrypted_key_requires_passphrase() {
        let k = generate(KeyAlgo::Ed25519, "enc", Some(&pw("correct horse"))).expect("generate");
        let text = k.private_openssh.expose_secret().to_string();

        assert!(matches!(
            import(&text, None),
            Err(KeyError::NeedsPassphrase)
        ));
        assert!(matches!(
            import(&text, Some(&pw("wrong"))),
            Err(KeyError::BadPassphrase)
        ));
        let ok = import(&text, Some(&pw("correct horse"))).expect("right passphrase");
        assert_eq!(ok.sha256_fingerprint, k.sha256_fingerprint);

        // Output stays encrypted with the same passphrase.
        let out = ok.private_openssh.expose_secret().to_string();
        assert!(matches!(import(&out, None), Err(KeyError::NeedsPassphrase)));
        import(&out, Some(&pw("correct horse"))).expect("re-encrypted with same passphrase");
    }

    #[test]
    fn imports_openssh_encrypted_from_ssh_keygen() {
        let text = include_str!("../tests/keys/ed25519_openssh_enc");
        assert!(matches!(import(text, None), Err(KeyError::NeedsPassphrase)));
        let k = import(text, Some(&pw("hunter2"))).expect("import");
        assert_eq!(k.algorithm, "ssh-ed25519");
        assert_eq!(
            k.sha256_fingerprint,
            "SHA256:aiNJWANrwyaSE/qtLZ0U0TsrQCz2EXw/JJZp4XyyhvY"
        );
        assert!(k.public_openssh.ends_with(" fixture"));
    }

    #[test]
    fn imports_pkcs8_and_pkcs1_rsa() {
        // Fingerprints are from `ssh-keygen -y -f <key>` piped to `ssh-keygen -lf`.
        let pkcs1 = import(include_str!("../tests/keys/rsa_pkcs1.pem"), None).expect("pkcs1");
        assert_eq!(pkcs1.algorithm, "ssh-rsa");
        assert_eq!(
            pkcs1.sha256_fingerprint,
            "SHA256:fYjHfrGU+mVj3xjhr3/iiSsfxuBaSPJxAeBVMpiXhe0"
        );
        assert!(
            pkcs1
                .private_openssh
                .expose_secret()
                .starts_with("-----BEGIN OPENSSH PRIVATE KEY-----")
        );

        let pkcs8 = import(include_str!("../tests/keys/rsa_pkcs8.pem"), None).expect("pkcs8");
        assert_eq!(pkcs8.algorithm, "ssh-rsa");
        assert_eq!(
            pkcs8.sha256_fingerprint,
            "SHA256:N1Qs02kbv+LICYefybfrSo+qRTHP7DbuakP7W30s0lk"
        );
    }

    #[test]
    fn imports_encrypted_pkcs8() {
        let text = include_str!("../tests/keys/rsa_pkcs8_enc.pem");
        assert!(matches!(import(text, None), Err(KeyError::NeedsPassphrase)));
        assert!(matches!(
            import(text, Some(&pw("nope"))),
            Err(KeyError::BadPassphrase)
        ));
        let k = import(text, Some(&pw("hunter2"))).expect("import");
        // Same key as rsa_pkcs8.pem, which this fixture was derived from.
        assert_eq!(
            k.sha256_fingerprint,
            "SHA256:N1Qs02kbv+LICYefybfrSo+qRTHP7DbuakP7W30s0lk"
        );
        // Stays encrypted under the same passphrase.
        let out = k.private_openssh.expose_secret().to_string();
        assert!(matches!(import(&out, None), Err(KeyError::NeedsPassphrase)));
        import(&out, Some(&pw("hunter2"))).expect("reimport");
    }

    #[test]
    fn imports_ecdsa_sec1_and_ed25519_pkcs8() {
        let ec = import(include_str!("../tests/keys/ecdsa_sec1.pem"), None).expect("ec");
        assert_eq!(ec.algorithm, "ecdsa-sha2-nistp256");
        assert_eq!(
            ec.sha256_fingerprint,
            "SHA256:0t2qxrvEhIwrJQT3wT7wvBl1zwisjedQKGFExUhWURk"
        );
        let ed = import(include_str!("../tests/keys/ed25519_pkcs8.pem"), None).expect("ed");
        assert_eq!(ed.algorithm, "ssh-ed25519");
    }

    #[test]
    fn unencrypted_input_ignores_passphrase() {
        let k = import(
            include_str!("../tests/keys/rsa_pkcs8.pem"),
            Some(&pw("unused")),
        )
        .expect("import");
        let out = k.private_openssh.expose_secret().to_string();
        import(&out, None).expect("output is not encrypted");
    }

    #[test]
    fn garbage_is_a_parse_error() {
        assert!(matches!(import("hello", None), Err(KeyError::Parse(_))));
        assert!(matches!(
            import(
                "-----BEGIN OPENSSH PRIVATE KEY-----\nAAAA\n-----END OPENSSH PRIVATE KEY-----\n",
                None
            ),
            Err(KeyError::Parse(_))
        ));
        let pubkey = generate(KeyAlgo::Ed25519, "", None)
            .expect("generate")
            .public_openssh;
        assert!(matches!(import(&pubkey, None), Err(KeyError::Parse(_))));
    }

    #[test]
    fn debug_output_redacts_private_key() {
        let k = generate(KeyAlgo::Ed25519, "", None).expect("generate");
        let body: String = k
            .private_openssh
            .expose_secret()
            .lines()
            .nth(1)
            .expect("body line")
            .to_string();
        assert!(!format!("{k:?}").contains(&body));
    }

    // --- docker fixture ---------------------------------------------------

    const CONTAINER: &str = "tether-sshd";
    const AUTHORIZED_KEYS: &str = "/config/.ssh/authorized_keys";
    const COMMENT: &str = "tether-keys-test-key";

    /// Removes the test key's line from the fixture's authorized_keys on drop,
    /// so the fixture is left as found even if the test panics.
    struct AuthorizedKeyGuard;

    impl Drop for AuthorizedKeyGuard {
        fn drop(&mut self) {
            let script = format!(
                "grep -vF ' {COMMENT}' {AUTHORIZED_KEYS} > /tmp/ak.new; \
                 cat /tmp/ak.new > {AUTHORIZED_KEYS}; rm -f /tmp/ak.new"
            );
            let _ = Command::new("docker")
                .args(["exec", CONTAINER, "sh", "-c", &script])
                .status();
        }
    }

    #[tokio::test]
    async fn fixture_key_authenticates() {
        if std::env::var("TETHER_SSH_FIXTURE").as_deref() != Ok("1") {
            eprintln!("skipping: set TETHER_SSH_FIXTURE=1 (and run tests/fixtures/up.sh)");
            return;
        }
        let key = generate(KeyAlgo::Ed25519, COMMENT, None).expect("generate");

        let _cleanup = AuthorizedKeyGuard;
        let script = format!(
            "printf '%s\\n' '{}' >> {AUTHORIZED_KEYS}",
            key.public_openssh
        );
        let status = Command::new("docker")
            .args(["exec", CONTAINER, "sh", "-c", &script])
            .status()
            .expect("run docker exec");
        assert!(status.success(), "could not append to authorized_keys");

        let mut shell = RusshTransport
            .open_shell(
                ConnectParams {
                    host: "127.0.0.1".into(),
                    port: 2222,
                    username: "tether".into(),
                    auth: AuthMethod::PrivateKey {
                        pem: key.private_openssh.clone(),
                        passphrase: None,
                    },
                    term: "xterm-256color".into(),
                    cols: 80,
                    rows: 24,
                    connect_timeout: Duration::from_secs(15),
                    keepalive: None,
                    env: Vec::new(),
                },
                Arc::new(AcceptAll),
            )
            .await
            .expect("generated key authenticates");
        shell
            .control
            .write(Bytes::from_static(b"echo keys-ok-$((6*7))\n"))
            .await
            .expect("write");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let mut acc = String::new();
        while !acc.contains("keys-ok-42") {
            let chunk = tokio::time::timeout_at(deadline, shell.output.recv())
                .await
                .expect("timed out waiting for command output")
                .expect("shell closed early");
            acc.push_str(&String::from_utf8_lossy(&chunk));
        }
        shell.control.close().await;
    }
}
