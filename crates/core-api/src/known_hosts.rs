//! Host-key verification against the vault's known hosts.
//!
//! Policy: a known key is accepted silently; an unknown host prompts
//! ([`Prompt::HostKeyUnknown`]); any other key for a known host, whatever
//! its type, prompts a blocking warning ([`Prompt::HostKeyChanged`]) whose
//! default is Reject. Nothing is ever accepted without the vault vouching
//! for it or the user saying so: an unanswered prompt or any answer but
//! `Accept` rejects, and a locked or uninitialised vault (or a vault error)
//! refuses as [`HostKeyDecision::Unverifiable`] without prompting.
//!
//! The transport asks servers for the key types stored for a host first
//! ([`HostKeyVerifier::known_algorithms`]), so a server with several host
//! keys presents the known one rather than tripping the warning.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use core_model::{Id, KnownHost, KnownHostSource};
use core_ssh::{HostKeyDecision, HostKeyInfo, HostKeyVerifier, host_matches};
use tracing::warn;

use crate::prompts::{Prompt, PromptBridge, PromptResponse};
use crate::vault::{SharedVault, VaultError};

/// [`HostKeyVerifier`] backed by the vault's [`KnownHost`] records, asking
/// the user through a [`PromptBridge`].
pub struct VaultHostKeyVerifier {
    vault: SharedVault,
    bridge: Arc<PromptBridge>,
}

/// What the vault says about a presented key.
enum Lookup {
    /// A matching entry holds exactly this key.
    Trusted,
    /// Entries for this host exist, but none holds this key.
    Changed {
        old: HostKeyInfo,
        /// Every host-specific entry for this host (all replaced if the user
        /// saves the new key). Glob entries are left alone: they vouch for
        /// other hosts too.
        stale: Vec<Id>,
    },
    /// Nothing stored for this host.
    Unknown,
}

impl VaultHostKeyVerifier {
    pub fn new(vault: SharedVault, bridge: Arc<PromptBridge>) -> Self {
        Self { vault, bridge }
    }

    /// Every stored entry whose pattern covers `host:port`, any key type.
    fn entries_for(&self, host: &str, port: u16) -> Result<Vec<KnownHost>, VaultError> {
        let entries = {
            let vault = self.vault.lock().unwrap_or_else(|p| p.into_inner());
            vault.list::<KnownHost>()?
        };
        Ok(entries
            .into_iter()
            .filter(|kh| host_matches(&kh.host_pattern, kh.port, host, port))
            .collect())
    }

    /// Trusted if any entry for `host:port` holds exactly `key`. Otherwise
    /// any entry for the host at all, whatever its key type, makes this a
    /// changed key: a server (or an attacker) presenting another key type
    /// must not get the milder unknown-host prompt.
    fn lookup(&self, host: &str, port: u16, key: &HostKeyInfo) -> Result<Lookup, VaultError> {
        let entries = self.entries_for(host, port)?;
        let line = |kh: &KnownHost| format!("{} {}", kh.algorithm, kh.public_key);
        if entries.iter().any(|kh| line(kh) == key.openssh) {
            return Ok(Lookup::Trusted);
        }
        // Show the stored key of the same type if there is one.
        let Some(shown) = entries
            .iter()
            .find(|kh| kh.algorithm == key.algorithm)
            .or_else(|| entries.first())
        else {
            return Ok(Lookup::Unknown);
        };
        Ok(Lookup::Changed {
            old: describe_stored(&line(shown), &shown.algorithm),
            stale: entries
                .iter()
                .filter(|kh| !is_glob(&kh.host_pattern))
                .map(|kh| kh.id)
                .collect(),
        })
    }

    /// Stores `key` for `host:port`, then drops the `stale` entries it
    /// replaces (not one transaction: if a delete fails, the old key stays
    /// stored next to the new one, and the error is returned).
    fn save(
        &self,
        host: &str,
        port: u16,
        key: &HostKeyInfo,
        stale: &[Id],
    ) -> Result<(), VaultError> {
        let (algorithm, public_key) = key
            .openssh
            .split_once(' ')
            .unwrap_or((key.algorithm.as_str(), ""));
        let entry = KnownHost {
            id: Id::now_v7(),
            host_pattern: host.to_ascii_lowercase(),
            port,
            algorithm: algorithm.to_owned(),
            public_key: public_key.to_owned(),
            added_at: unix_now(),
            source: KnownHostSource::Prompt,
            extra: Default::default(),
        };
        let mut vault = self.vault.lock().unwrap_or_else(|p| p.into_inner());
        vault.upsert(entry)?;
        for id in stale {
            vault.delete::<KnownHost>(*id)?;
        }
        Ok(())
    }

    /// Asks the user about `key` for `host:port`; `Accept` (saving if
    /// requested) or `Reject`.
    async fn ask(
        &self,
        host: &str,
        port: u16,
        prompt: Prompt,
        key: &HostKeyInfo,
        stale: &[Id],
    ) -> HostKeyDecision {
        match self.bridge.ask(prompt).await {
            PromptResponse::Accept { save } => {
                if save && let Err(e) = self.save(host, port, key, stale) {
                    // The user accepted; the connection goes ahead, but the
                    // next one will ask again.
                    warn!(%host, port, error = %e, "could not save accepted host key");
                }
                HostKeyDecision::Accept
            }
            _ => HostKeyDecision::Reject,
        }
    }
}

#[async_trait]
impl HostKeyVerifier for VaultHostKeyVerifier {
    async fn verify(&self, host: &str, port: u16, key: &HostKeyInfo) -> HostKeyDecision {
        let lookup = match self.lookup(host, port, key) {
            Ok(l) => l,
            Err(e) => {
                // Never accept what we cannot check; say why, without asking.
                warn!(
                    %host,
                    port,
                    error = %e,
                    "host key not verifiable: known hosts unavailable (unlock the vault)"
                );
                return HostKeyDecision::Unverifiable;
            }
        };
        match lookup {
            Lookup::Trusted => HostKeyDecision::Accept,
            Lookup::Unknown => {
                let prompt = Prompt::HostKeyUnknown {
                    host: host.to_owned(),
                    port,
                    key: key.clone(),
                };
                self.ask(host, port, prompt, key, &[]).await
            }
            Lookup::Changed { old, stale } => {
                warn!(%host, port, "server host key does not match the stored key");
                let prompt = Prompt::HostKeyChanged {
                    host: host.to_owned(),
                    port,
                    old,
                    new: key.clone(),
                };
                self.ask(host, port, prompt, key, &stale).await
            }
        }
    }

    /// Key types stored for `host:port`, so the transport asks the server
    /// for a known one first. Empty if the vault cannot be read (`verify`
    /// then reports the key as unverifiable).
    async fn known_algorithms(&self, host: &str, port: u16) -> Vec<String> {
        let mut algorithms: Vec<String> = Vec::new();
        for kh in self.entries_for(host, port).unwrap_or_default() {
            if !algorithms.contains(&kh.algorithm) {
                algorithms.push(kh.algorithm);
            }
        }
        algorithms
    }
}

/// A stored key as shown in the changed-key warning. A stored key that no
/// longer parses still produces the warning, with no fingerprint.
fn describe_stored(line: &str, algorithm: &str) -> HostKeyInfo {
    HostKeyInfo::from_openssh(line).unwrap_or_else(|_| HostKeyInfo {
        algorithm: algorithm.to_owned(),
        sha256_fingerprint: String::new(),
        openssh: line.to_owned(),
    })
}

fn is_glob(pattern: &str) -> bool {
    !pattern.starts_with("|1|") && pattern.contains(['*', '?'])
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}
