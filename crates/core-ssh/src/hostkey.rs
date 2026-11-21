use async_trait::async_trait;
use russh::keys::ssh_key;
use russh::keys::{HashAlg, PublicKey, PublicKeyOrCertificate};
use serde::Serialize;

use crate::error::SshError;

/// A server host key, described the way OpenSSH presents it. Serializes with
/// camelCase field names (it is shown to the user in host-key prompts).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostKeyInfo {
    /// Algorithm name, e.g. `ssh-ed25519`.
    pub algorithm: String,
    /// `SHA256:` followed by unpadded base64 (OpenSSH format).
    pub sha256_fingerprint: String,
    /// `algo base64` public key line (no comment).
    pub openssh: String,
}

impl HostKeyInfo {
    /// Describes a public key given as an OpenSSH line (`algo base64
    /// [comment]`), e.g. a stored known-hosts entry.
    pub fn from_openssh(line: &str) -> Result<Self, SshError> {
        let key = PublicKey::from_openssh(line.trim())
            .map_err(|e| SshError::HostKeyEncoding(e.to_string()))?;
        Self::from_public_key(&key).map_err(|e| SshError::HostKeyEncoding(e.to_string()))
    }

    pub(crate) fn from_public_key(key: &PublicKey) -> Result<Self, ssh_key::Error> {
        Ok(Self {
            algorithm: key.algorithm().to_string(),
            sha256_fingerprint: key.fingerprint(HashAlg::Sha256).to_string(),
            openssh: openssh_line(key.to_openssh()?)?,
        })
    }

    pub(crate) fn from_server_key(key: &PublicKeyOrCertificate) -> Result<Self, ssh_key::Error> {
        match key {
            PublicKeyOrCertificate::PublicKey { key, .. } => Self::from_public_key(key),
            PublicKeyOrCertificate::Certificate(cert) => Ok(Self {
                algorithm: cert.algorithm().to_string(),
                sha256_fingerprint: cert.public_key().fingerprint(HashAlg::Sha256).to_string(),
                openssh: openssh_line(cert.to_openssh()?)?,
            }),
        }
    }
}

/// Keep only `algo base64`, dropping any trailing comment.
fn openssh_line(full: String) -> Result<String, ssh_key::Error> {
    let mut parts = full.split_whitespace();
    match (parts.next(), parts.next()) {
        (Some(algo), Some(data)) => Ok(format!("{algo} {data}")),
        _ => Err(ssh_key::Error::Encoding(ssh_key::encoding::Error::Length)),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostKeyDecision {
    Accept,
    Reject,
    /// The key could not be checked at all (the trusted-key store is
    /// unavailable, e.g. the vault is locked). The connection is refused,
    /// like `Reject`, but reported as
    /// [`SshError::HostKeyUnverifiable`] so the UI can say why.
    Unverifiable,
}

/// Decides whether to trust a server's host key.
///
/// `ConnectParams::connect_timeout` deliberately excludes time spent in
/// `verify` (it may be waiting on a human), so implementations must bound
/// their own wait, e.g. a prompt with a timeout that returns `Reject`.
#[async_trait]
pub trait HostKeyVerifier: Send + Sync {
    async fn verify(&self, host: &str, port: u16, key: &HostKeyInfo) -> HostKeyDecision;

    /// Key algorithms (OpenSSH key type names, e.g. `ssh-ed25519`) already
    /// trusted for `host:port`. The transport asks the server for these
    /// first, so a server with several host keys presents the known one.
    /// Default: none (the transport's normal preference order).
    async fn known_algorithms(&self, _host: &str, _port: u16) -> Vec<String> {
        Vec::new()
    }
}

/// Accepts every host key, i.e. performs NO host-key verification. Gated
/// behind the `test-util` feature, for tests only: the app verifies against
/// the vault's known hosts, and CI fails if `test-util` reaches the app's
/// normal dependency graph.
#[cfg(feature = "test-util")]
#[derive(Debug, Clone, Copy, Default)]
pub struct AcceptAll;

#[cfg(feature = "test-util")]
#[async_trait]
impl HostKeyVerifier for AcceptAll {
    async fn verify(&self, _host: &str, _port: u16, _key: &HostKeyInfo) -> HostKeyDecision {
        HostKeyDecision::Accept
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_key_info_uses_openssh_formats() {
        let pub_line = include_str!("../../../tests/fixtures/keys/id_ed25519_test.pub");
        let key = PublicKey::from_openssh(pub_line).expect("parse pub key");
        let info = HostKeyInfo::from_public_key(&key).expect("describe key");
        assert_eq!(info.algorithm, "ssh-ed25519");
        assert!(info.sha256_fingerprint.starts_with("SHA256:"));
        assert!(!info.sha256_fingerprint.contains('='));
        let expected: Vec<&str> = pub_line.split_whitespace().take(2).collect();
        assert_eq!(info.openssh, expected.join(" "));
    }
}
