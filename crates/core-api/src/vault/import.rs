//! Applying an ssh_config [`ImportPlan`] to the vault.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use core_model::{Id, SshKey};
use core_ssh::keys::{GeneratedKey, KeyError};

use super::{VaultError, VaultService};
use crate::import::{ImportPlan, ImportReport, KeyFailure};

impl VaultService {
    /// Writes `plan` to the vault: its key files (loaded through
    /// `key_loader`), then identities, then hosts.
    ///
    /// A key that fails to load never aborts the import: the identities that
    /// use it are stored without a key and the failure is listed in the
    /// report (`needs_passphrase` for an encrypted key, so the UI can ask
    /// and attach the key later). Loaded keys become [`SshKey`]s labelled
    /// with the file name.
    ///
    /// All keys are loaded first, outside the database transaction; the
    /// writes then happen in one transaction, so a failure there (a locked
    /// vault, a plan whose hosts reference a missing identity) leaves the
    /// vault exactly as it was.
    pub fn apply_import(
        &mut self,
        plan: ImportPlan,
        key_loader: impl Fn(&Path) -> Result<GeneratedKey, KeyError>,
    ) -> Result<ImportReport, VaultError> {
        let ctx = self.ctx()?;

        let mut keys: Vec<SshKey> = Vec::new();
        let mut key_ids: HashMap<&PathBuf, Id> = HashMap::new();
        let mut key_failures = Vec::new();
        for path in &plan.key_files {
            match key_loader(path) {
                Ok(loaded) => {
                    let key = SshKey {
                        id: Id::now_v7(),
                        label: path.file_name().map_or_else(
                            || path.display().to_string(),
                            |n| n.to_string_lossy().into_owned(),
                        ),
                        algorithm: loaded.algorithm,
                        private_openssh: loaded.private_openssh,
                        passphrase: None,
                        public_openssh: loaded.public_openssh,
                        certificate: None,
                        extra: Default::default(),
                    };
                    key_ids.insert(path, key.id);
                    keys.push(key);
                }
                Err(err) => key_failures.push(KeyFailure {
                    path: path.clone(),
                    needs_passphrase: matches!(err, KeyError::NeedsPassphrase),
                    message: err.to_string(),
                }),
            }
        }

        let ImportPlan {
            hosts,
            mut identities,
            identity_key_files,
            ..
        } = plan;
        for identity in &mut identities {
            identity.key_id = identity_key_files
                .iter()
                .find(|(id, _)| *id == identity.id)
                .and_then(|(_, path)| key_ids.get(path).copied());
        }

        let report = ImportReport {
            hosts_added: hosts.len(),
            identities_added: identities.len(),
            keys_added: keys.len(),
            key_failures: key_failures.clone(),
        };
        // Keys, then identities, then hosts: each kind only references the
        // previous one.
        ctx.atomically(|| {
            for key in keys {
                ctx.upsert(key)?;
            }
            for identity in identities {
                ctx.upsert(identity)?;
            }
            for host in hosts {
                ctx.upsert(host)?;
            }
            Ok(())
        })?;
        Ok(report)
    }
}
