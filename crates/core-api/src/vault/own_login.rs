//! A host's or group's own login: username and password set in its editor,
//! stored as an [`Identity`] whose `owner` is that host or group and linked
//! through its `settings.identity_id`, so inheritance and connecting treat
//! it like any identity. Own logins are hidden from the Keychain; once their
//! owner no longer links them (switched to another identity, or deleted),
//! [`VaultService::tidy_own_logins`] removes them.

use core_model::{Group, Host, Id, Identity};
use secrecy::SecretString;

use super::{VaultError, VaultService};

/// What to do with a stored password.
pub enum PasswordChange {
    Keep,
    Clear,
    Set(SecretString),
}

/// The record that owns a login.
enum Owner {
    Host(Host),
    Group(Group),
}

impl Owner {
    fn label(&self) -> &str {
        match self {
            Owner::Host(h) => &h.label,
            Owner::Group(g) => &g.name,
        }
    }

    fn identity_id(&self) -> Option<Id> {
        match self {
            Owner::Host(h) => h.settings.identity_id,
            Owner::Group(g) => g.settings.identity_id,
        }
    }
}

impl VaultService {
    /// Sets the own login of host or group `owner` (creating it, or updating
    /// the one it has) and links it on the owner, in one transaction.
    /// [`VaultError::MissingRef`] when `owner` is neither a host nor a group.
    pub fn set_own_login(
        &mut self,
        owner: Id,
        username: String,
        password: PasswordChange,
    ) -> Result<Identity, VaultError> {
        let record = self.owner(owner)?.ok_or(VaultError::MissingRef(owner))?;
        let existing = match record.identity_id() {
            Some(id) => self.get::<Identity>(id)?.filter(|i| i.owner == Some(owner)),
            None => None,
        };
        let previous = existing.as_ref().and_then(|i| i.password.clone());
        let identity = Identity {
            id: existing.as_ref().map_or_else(Id::now_v7, |i| i.id),
            label: record.label().to_owned(),
            username,
            password: match password {
                PasswordChange::Keep => previous,
                PasswordChange::Clear => None,
                PasswordChange::Set(p) => Some(p),
            },
            key_id: None,
            owner: Some(owner),
            extra: Default::default(),
        };
        let linked = match record {
            Owner::Host(mut h) => {
                h.settings.identity_id = Some(identity.id);
                Linked::Host(h)
            }
            Owner::Group(mut g) => {
                g.settings.identity_id = Some(identity.id);
                Linked::Group(g)
            }
        };
        let ctx = self.ctx()?;
        ctx.atomically(|| {
            ctx.upsert(identity.clone())?;
            match linked {
                Linked::Host(h) => ctx.upsert(h),
                Linked::Group(g) => ctx.upsert(g),
            }
        })?;
        Ok(identity)
    }

    /// Deletes own logins whose owner is gone or no longer links them (and
    /// that nothing else uses), and renames the others after their owner
    /// (a renamed host or group). Returns how many were deleted. An own
    /// login that an open conflict is about is kept until the conflict is
    /// resolved, so a version the user may still pick is never removed.
    pub fn tidy_own_logins(&mut self) -> Result<usize, VaultError> {
        let conflicted = self.conflicted_records()?;
        let mut gone = Vec::new();
        for identity in self.list::<Identity>()? {
            let Some(owner) = identity.owner else {
                continue;
            };
            if conflicted.contains(&identity.id) {
                continue;
            }
            match self.owner(owner)? {
                Some(o) if o.identity_id() == Some(identity.id) => {
                    if identity.label != o.label() {
                        let label = o.label().to_owned();
                        self.upsert(Identity { label, ..identity })?;
                    }
                }
                // The owner is gone: this is the owner delete's cascade.
                None => gone.push((identity.id, Some(owner))),
                // The owner switched to another login.
                Some(_) => gone.push((identity.id, None)),
            }
        }
        let mut deleted = 0;
        for (id, deleted_owner) in gone {
            let ctx = self.ctx()?;
            let result = ctx.atomically(|| {
                let Some(live) = ctx.row::<Identity>(id)? else {
                    return Err(VaultError::Store(core_store::StoreError::NotFound { id }));
                };
                ctx.delete_checked::<Identity>(id)?;
                // The owner's delete is not pushed yet: if it loses to an
                // edit, this login comes back with it.
                if let Some(owner) = deleted_owner
                    && ctx.store.is_dirty(&owner)? == Some(true)
                    && ctx.store.get(&owner)?.is_some_and(|r| r.deleted)
                {
                    ctx.stash_cascaded(owner, &live)?;
                }
                Ok(())
            });
            match result {
                Ok(()) => deleted += 1,
                // Something else still points at it (picked by hand): keep it.
                Err(VaultError::Referenced(_)) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(deleted)
    }

    fn owner(&self, id: Id) -> Result<Option<Owner>, VaultError> {
        if let Some(h) = self.get::<Host>(id)? {
            return Ok(Some(Owner::Host(h)));
        }
        Ok(self.get::<Group>(id)?.map(Owner::Group))
    }
}

enum Linked {
    Host(Host),
    Group(Group),
}
