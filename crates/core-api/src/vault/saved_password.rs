//! Saving a password typed at a connect prompt onto the host's identity
//! (see `crate::save_password`).

use core_model::{Host, Id, Identity};
use secrecy::SecretString;

use super::{PasswordChange, VaultError, VaultService};
use crate::prompts::SaveTarget;

/// What saving would do for a host.
enum Plan {
    /// Set the password of this (key-less) identity.
    Existing(Identity),
    /// Give the host its own login (an identity labelled after the host).
    New(String),
    /// The identity authenticates with a key: nothing to save.
    No,
}

impl VaultService {
    /// Where a password typed for `host_id`
    /// would be saved, or `None` when saving is not offered (the effective
    /// identity uses a key). Needs the vault unlocked.
    pub fn password_save_target(&self, host_id: Id) -> Result<Option<SaveTarget>, VaultError> {
        Ok(match self.save_plan(host_id)? {
            Plan::Existing(identity) => Some(SaveTarget {
                identity_label: identity.label,
                is_new: false,
            }),
            Plan::New(label) => Some(SaveTarget {
                identity_label: label,
                is_new: true,
            }),
            Plan::No => None,
        })
    }

    /// Saves `password` for `host_id`: on its effective identity when that
    /// has no key (identities are shared, so every host using it gets it),
    /// else as the host's own login (see `set_own_login`). Returns the
    /// identity id, or `None` when nothing was saved (the identity uses a
    /// key).
    pub fn save_host_password(
        &mut self,
        host_id: Id,
        username: &str,
        password: SecretString,
    ) -> Result<Option<Id>, VaultError> {
        match self.save_plan(host_id)? {
            Plan::Existing(identity) => {
                let id = identity.id;
                self.upsert(Identity {
                    password: Some(password),
                    ..identity
                })?;
                Ok(Some(id))
            }
            Plan::New(_) => {
                let own = self.set_own_login(
                    host_id,
                    username.to_owned(),
                    PasswordChange::Set(password),
                )?;
                Ok(Some(own.id))
            }
            Plan::No => Ok(None),
        }
    }

    fn save_plan(&self, host_id: Id) -> Result<Plan, VaultError> {
        let eff = self.effective_settings(host_id)?;
        match eff.identity_id.value {
            Some(id) => {
                let identity = self
                    .get::<Identity>(id)?
                    .ok_or(VaultError::MissingRef(id))?;
                Ok(if identity.key_id.is_some() {
                    Plan::No
                } else {
                    Plan::Existing(identity)
                })
            }
            None => {
                let host = self
                    .get::<Host>(host_id)?
                    .ok_or(VaultError::MissingRef(host_id))?;
                Ok(Plan::New(host.label))
            }
        }
    }
}
