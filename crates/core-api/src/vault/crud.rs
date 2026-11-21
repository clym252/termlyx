//! Entity CRUD with referential integrity.
//!
//! Rules (never leave a dangling reference):
//! * Upserting a value that refers to a missing id (group, parent group,
//!   identity, key, tag, host) fails with [`VaultError::MissingRef`]; a group
//!   move that would create a loop fails with [`VaultError::Cycle`].
//! * `ForwardRule.host_id` is authoritative: `Host.forward_rule_ids` is
//!   maintained by rule upserts/deletes and recomputed on every host upsert.
//! * Deleting an identity or key still in use, or a group that still has
//!   children, fails with [`VaultError::Referenced`] listing the users.
//!   Deleting a tag strips it from hosts; deleting a host deletes its rules.
//!
//! Every multi-record change runs in one store transaction.

use std::collections::{HashMap, HashSet};

use core_model::{
    Conflict, EffectiveSettings, Extra, ForwardRule, Group, Hlc, Host, Id, Identity, Kind,
    KnownHost, Snippet, SshKey, Tag, group_chain, resolve,
};
use core_store::{RecordRow, Store, StoreError, open_row, seal_row};
use secrecy::ExposeSecret;
use serde::{Serialize, de::DeserializeOwned};
use zeroize::Zeroizing;

use super::service::Unlocked;
use super::{VaultError, VaultService, app_defaults};

/// What [`VaultService::delete_group`] does with a group's subgroups and hosts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildPolicy {
    /// Re-parent subgroups and hosts to the deleted group's parent (or the
    /// top level).
    MoveToParent,
    /// Delete the whole subtree: subgroups, their hosts and those hosts'
    /// forward rules.
    DeleteRecursive,
}

/// A record type stored in the vault. Implemented for every `core_model`
/// entity; sealed, so other types cannot claim a [`Kind`].
pub trait Entity: Serialize + DeserializeOwned + sealed::Hooks {
    /// The record kind (part of the envelope AAD).
    const KIND: Kind;
    /// The record id.
    fn id(&self) -> Id;
    /// The fields of the record this build does not know (see [`Extra`]).
    fn extra_mut(&mut self) -> &mut Extra;
}

/// Per-kind integrity hooks. Private: reachable only through [`Entity`].
mod sealed {
    use super::{Ctx, VaultError};
    use core_model::Id;

    pub trait Hooks: Sized {
        /// Read-time view of the live records of this kind, in store order:
        /// hides duplicates without deleting them. Identity by default.
        fn collapse(items: Vec<Self>) -> Vec<Self> {
            items
        }
        /// Unsets references to records that no longer exist (they read as
        /// unset anyway). Run only when writing back a version from a
        /// conflict, which may predate those records' deletion.
        fn clear_dangling(&mut self, _ctx: &Ctx<'_>) -> Result<(), VaultError> {
            Ok(())
        }
        /// Checks references and normalizes derived fields; may update other
        /// records. Runs in the upsert's transaction before `self` is written.
        fn before_upsert(&mut self, _ctx: &Ctx<'_>) -> Result<(), VaultError> {
            Ok(())
        }
        /// Refuses (`Referenced`) or cascades. Runs in the delete's
        /// transaction before the record is tombstoned.
        fn before_delete(_ctx: &Ctx<'_>, _id: Id) -> Result<(), VaultError> {
            Ok(())
        }
    }
}

/// Decrypting/encrypting view of the store for one unlocked session.
pub struct Ctx<'a> {
    pub(super) store: &'a Store,
    pub(super) keys: &'a Unlocked,
}

impl Ctx<'_> {
    pub(super) fn next_hlc(&self) -> Hlc {
        let hlc = Hlc::now(&self.keys.last_hlc.get(), self.keys.node);
        self.keys.last_hlc.set(hlc);
        hlc
    }

    /// The live row with `id` if it is a `T` in this vault.
    pub(super) fn row<T: Entity>(&self, id: Id) -> Result<Option<RecordRow>, VaultError> {
        Ok(self
            .store
            .get(&id)?
            .filter(|r| !r.deleted && r.kind == T::KIND && r.vault_id == self.keys.vault_id))
    }

    pub(super) fn decode<T: Entity>(&self, row: &RecordRow) -> Result<T, VaultError> {
        let plaintext = open_row(self.keys.vault_key.expose_secret(), row)?;
        serde_json::from_slice(&plaintext).map_err(|_| VaultError::BadRecord(row.id))
    }

    pub(super) fn get<T: Entity>(&self, id: Id) -> Result<Option<T>, VaultError> {
        self.row::<T>(id)?.map(|r| self.decode(&r)).transpose()
    }

    /// Live records of type `T`, after the kind's read-time collapse (see
    /// `Hooks::collapse`).
    pub(super) fn list<T: Entity>(&self) -> Result<Vec<T>, VaultError> {
        let all = self
            .store
            .list(&self.keys.vault_id, T::KIND)?
            .iter()
            .map(|r| self.decode(r))
            .collect::<Result<Vec<T>, _>>()?;
        Ok(T::collapse(all))
    }

    pub(super) fn put<T: Entity>(&self, value: &T) -> Result<(), VaultError> {
        let id = value.id();
        let plaintext =
            Zeroizing::new(serde_json::to_vec(value).map_err(|_| VaultError::BadRecord(id))?);
        let row = seal_row(
            self.keys.vault_key.expose_secret(),
            self.keys.vault_id,
            T::KIND,
            id,
            self.keys.key_version,
            self.next_hlc(),
            &plaintext,
        );
        self.forget_stashed(id)?;
        Ok(self.store.put(&row)?)
    }

    pub(super) fn tombstone(&self, id: Id) -> Result<(), VaultError> {
        self.forget_stashed(id)?;
        Ok(self.store.tombstone(&id, self.next_hlc())?)
    }

    fn require<T: Entity>(&self, id: Id) -> Result<(), VaultError> {
        match self.row::<T>(id)? {
            Some(_) => Ok(()),
            None => Err(VaultError::MissingRef(id)),
        }
    }

    /// `None` in place of a reference to a record that is not a live `T`.
    fn live_or_unset<T: Entity>(&self, id: Option<Id>) -> Result<Option<Id>, VaultError> {
        Ok(match id {
            Some(id) if self.row::<T>(id)?.is_some() => Some(id),
            _ => None,
        })
    }

    fn require_opt<T: Entity>(&self, id: Option<Id>) -> Result<(), VaultError> {
        id.map_or(Ok(()), |id| self.require::<T>(id))
    }

    /// The identity a host or group `owner` links: it must exist and be a
    /// Keychain identity or `owner`'s own login (another record's own login
    /// is inherited, never linked).
    fn require_login(&self, owner: Id, identity: Option<Id>) -> Result<(), VaultError> {
        let Some(id) = identity else { return Ok(()) };
        let found = self
            .get::<Identity>(id)?
            .ok_or(VaultError::MissingRef(id))?;
        match found.owner {
            Some(o) if o != owner => Err(VaultError::ForeignOwnLogin(id)),
            _ => Ok(()),
        }
    }

    /// Deletes a host's forward rules (the host is being deleted), stashing
    /// each for a revival should the delete lose to an edit (see
    /// [`cascade`](super::cascade)).
    fn delete_rules_of(&self, host_id: Id) -> Result<(), VaultError> {
        self.clear_cascade(host_id)?;
        for r in self.list::<ForwardRule>()? {
            if r.host_id == host_id {
                let live = self.store.get(&r.id)?;
                self.tombstone(r.id)?;
                if let Some(live) = live {
                    self.stash_cascaded(host_id, &live)?;
                }
            }
        }
        Ok(())
    }

    /// Runs `f` in one store transaction. Transactions do not nest: inside
    /// `f` write with [`upsert`](Self::upsert), not `VaultService::upsert`.
    pub(super) fn atomically<T>(
        &self,
        f: impl FnOnce() -> Result<T, VaultError>,
    ) -> Result<T, VaultError> {
        self.store.atomically(f)
    }

    /// The body of [`VaultService::upsert`]: id-conflict check, reference
    /// checks and normalisation, then the write. Needs an open transaction
    /// to be atomic. A deleted record stays deleted: upserting its id is
    /// `Store(NotFound)` (a stale editor must not resurrect it). A value
    /// with no unknown fields inherits the stored record's, so an editor
    /// that predates a newer version's fields does not erase them.
    pub(super) fn upsert<T: Entity>(&self, mut value: T) -> Result<(), VaultError> {
        let id = value.id();
        let mut stored_extra = None;
        if let Some(existing) = self.store.get(&id)? {
            if existing.kind != T::KIND || existing.vault_id != self.keys.vault_id {
                return Err(VaultError::IdConflict(id));
            }
            if existing.deleted {
                return Err(not_found(id));
            }
            if value.extra_mut().is_empty() {
                stored_extra = self
                    .decode::<T>(&existing)
                    .ok()
                    .map(|mut old| std::mem::take(old.extra_mut()));
            }
        }
        if let Some(extra) = stored_extra {
            *value.extra_mut() = extra;
        }
        value.before_upsert(self)?;
        self.put(&value)
    }

    /// Writes `value` exactly as given (its own unknown fields, none
    /// inherited), and unlike [`upsert`](Self::upsert) also over a deleted
    /// record: resolving a conflict may bring a version back. References to
    /// records that no longer exist are unset first (spec: missing
    /// references read as unset); every other check still runs. Needs an
    /// open transaction.
    pub(super) fn restore<T: Entity>(&self, mut value: T) -> Result<(), VaultError> {
        let id = value.id();
        if let Some(existing) = self.store.get(&id)?
            && (existing.kind != T::KIND || existing.vault_id != self.keys.vault_id)
        {
            return Err(VaultError::IdConflict(id));
        }
        value.clear_dangling(self)?;
        value.before_upsert(self)?;
        self.put(&value)
    }

    /// The body of [`VaultService::delete`]: the kind's delete checks and
    /// cascades, then the tombstone. A record that is already gone is left
    /// as it is. Needs an open transaction.
    pub(super) fn delete_checked<T: Entity>(&self, id: Id) -> Result<(), VaultError> {
        if self.row::<T>(id)?.is_none() {
            return Ok(());
        }
        T::before_delete(self, id)?;
        self.tombstone(id)
    }
}

/// Top-level fields of a record kind that hold secrets. Conflict views
/// replace them with a `{"hasSecret": true}` marker and only
/// [`VaultService::reveal_conflict_field`] returns their values. Keep in step
/// with the secret (`SecretString`) fields of the entities below.
pub(super) fn secret_fields(kind: Kind) -> &'static [&'static str] {
    match kind {
        Kind::Identity => &["password"],
        Kind::SshKey => &["private_openssh", "passphrase"],
        Kind::Group
        | Kind::Host
        | Kind::Tag
        | Kind::KnownHost
        | Kind::ForwardRule
        | Kind::Preference
        | Kind::Conflict
        | Kind::Snippet => &[],
    }
}

fn not_found(id: Id) -> VaultError {
    VaultError::Store(StoreError::NotFound { id })
}

impl VaultService {
    pub(super) fn ctx(&self) -> Result<Ctx<'_>, VaultError> {
        let keys = self.unlocked.as_ref().ok_or(VaultError::Locked)?;
        Ok(Ctx {
            store: &self.store,
            keys,
        })
    }

    /// All live records of type `T`. A record that fails to decrypt is an
    /// error naming it (`Store(Corrupt { id })`), never silently skipped.
    pub fn list<T: Entity>(&self) -> Result<Vec<T>, VaultError> {
        self.ctx()?.list()
    }

    /// The live record `id` if it exists and is a `T`.
    pub fn get<T: Entity>(&self, id: Id) -> Result<Option<T>, VaultError> {
        self.ctx()?.get(id)
    }

    /// Inserts or replaces `value` after checking its references (see the
    /// module docs). Refuses an id that belongs to a record of another kind.
    pub fn upsert<T: Entity>(&mut self, value: T) -> Result<(), VaultError> {
        let ctx = self.ctx()?;
        ctx.atomically(|| ctx.upsert(value))
    }

    /// [`upsert`](Self::upsert)s every value in one transaction: all are
    /// written, or (on the first error) none is.
    pub fn upsert_all<T: Entity>(
        &mut self,
        values: impl IntoIterator<Item = T>,
    ) -> Result<(), VaultError> {
        let ctx = self.ctx()?;
        ctx.atomically(|| values.into_iter().try_for_each(|v| ctx.upsert(v)))
    }

    /// Deletes record `id` of type `T`. Returns [`VaultError::Referenced`]
    /// (deleting nothing) while other records still use it, and
    /// `Store(NotFound)` if there is no such live `T`. Groups with children
    /// need [`delete_group`](Self::delete_group).
    pub fn delete<T: Entity>(&mut self, id: Id) -> Result<(), VaultError> {
        let ctx = self.ctx()?;
        ctx.atomically(|| {
            if ctx.row::<T>(id)?.is_none() {
                return Err(not_found(id));
            }
            T::before_delete(&ctx, id)?;
            ctx.tombstone(id)
        })
    }

    /// Deletes a group, handling its subgroups and hosts per `policy`.
    pub fn delete_group(&mut self, id: Id, policy: ChildPolicy) -> Result<(), VaultError> {
        let ctx = self.ctx()?;
        ctx.atomically(|| {
            let group = ctx.get::<Group>(id)?.ok_or_else(|| not_found(id))?;
            ctx.clear_cascade(id)?;
            let groups = ctx.list::<Group>()?;
            let hosts = ctx.list::<Host>()?;
            match policy {
                ChildPolicy::MoveToParent => {
                    for mut child in groups.into_iter().filter(|g| g.parent_id == Some(id)) {
                        child.parent_id = group.parent_id;
                        ctx.put(&child)?;
                    }
                    for mut host in hosts.into_iter().filter(|h| h.group_id == Some(id)) {
                        host.group_id = group.parent_id;
                        ctx.put(&host)?;
                    }
                }
                ChildPolicy::DeleteRecursive => {
                    let subtree = subtree(&groups, id);
                    for host in hosts
                        .iter()
                        .filter(|h| h.group_id.is_some_and(|g| subtree.contains(&g)))
                    {
                        ctx.delete_rules_of(host.id)?;
                        ctx.tombstone(host.id)?;
                    }
                    for g in subtree.iter().filter(|g| **g != id) {
                        ctx.tombstone(*g)?;
                    }
                }
            }
            ctx.tombstone(id)
        })
    }

    /// Effective connection settings of a host: [`app_defaults`], then its
    /// group chain root to leaf, then the host. `Store(NotFound)` for an
    /// unknown host.
    pub fn effective_settings(&self, host_id: Id) -> Result<EffectiveSettings, VaultError> {
        let ctx = self.ctx()?;
        let host = ctx
            .get::<Host>(host_id)?
            .ok_or_else(|| not_found(host_id))?;
        let groups: HashMap<Id, Group> = ctx
            .list::<Group>()?
            .into_iter()
            .map(|g| (g.id, g))
            .collect();
        resolve(&app_defaults(), &groups, &host).map_err(|_| VaultError::Cycle)
    }
}

/// `root` and all groups below it.
fn subtree(groups: &[Group], root: Id) -> HashSet<Id> {
    let mut found = HashSet::from([root]);
    let mut frontier = vec![root];
    while let Some(parent) = frontier.pop() {
        for g in groups {
            if g.parent_id == Some(parent) && found.insert(g.id) {
                frontier.push(g.id);
            }
        }
    }
    found
}

macro_rules! entity {
    ($($ty:ty => $kind:ident),* $(,)?) => {$(
        impl Entity for $ty {
            const KIND: Kind = Kind::$kind;
            fn id(&self) -> Id {
                self.id
            }
            fn extra_mut(&mut self) -> &mut Extra {
                &mut self.extra
            }
        }
    )*};
}

entity!(
    Group => Group,
    Host => Host,
    Tag => Tag,
    Identity => Identity,
    SshKey => SshKey,
    KnownHost => KnownHost,
    ForwardRule => ForwardRule,
    Conflict => Conflict,
    Snippet => Snippet,
);

/// Two known-host rows trust the same key (two devices accepted it on their
/// own, then synced).
fn same_key(a: &KnownHost, b: &KnownHost) -> bool {
    a.host_pattern == b.host_pattern
        && a.port == b.port
        && a.algorithm == b.algorithm
        && a.public_key == b.public_key
}

impl sealed::Hooks for KnownHost {
    /// Duplicates (same pattern, port, algorithm and key) collapse to the
    /// entry with the lowest id; the others stay stored, just unlisted.
    fn collapse(items: Vec<Self>) -> Vec<Self> {
        let mut lowest: HashMap<(&str, u16, &str, &str), Id> = HashMap::new();
        for k in &items {
            let key = (
                k.host_pattern.as_str(),
                k.port,
                k.algorithm.as_str(),
                k.public_key.as_str(),
            );
            let id = lowest.entry(key).or_insert(k.id);
            *id = (*id).min(k.id);
        }
        let keep: HashSet<Id> = lowest.into_values().collect();
        items.into_iter().filter(|k| keep.contains(&k.id)).collect()
    }

    /// Removing a trusted key also removes its hidden duplicates, so the key
    /// is really no longer trusted.
    fn before_delete(ctx: &Ctx<'_>, id: Id) -> Result<(), VaultError> {
        let Some(this) = ctx.get::<KnownHost>(id)? else {
            return Ok(());
        };
        for row in ctx.store.list(&ctx.keys.vault_id, Kind::KnownHost)? {
            if row.id == id {
                continue;
            }
            let other: KnownHost = ctx.decode(&row)?;
            if same_key(&other, &this) {
                ctx.tombstone(other.id)?;
            }
        }
        Ok(())
    }
}

impl sealed::Hooks for Conflict {}

impl sealed::Hooks for Snippet {}

impl sealed::Hooks for Group {
    fn clear_dangling(&mut self, ctx: &Ctx<'_>) -> Result<(), VaultError> {
        self.parent_id = ctx.live_or_unset::<Group>(self.parent_id)?;
        self.settings.identity_id = ctx.live_or_unset::<Identity>(self.settings.identity_id)?;
        Ok(())
    }

    fn before_upsert(&mut self, ctx: &Ctx<'_>) -> Result<(), VaultError> {
        if self.parent_id == Some(self.id) {
            return Err(VaultError::Cycle);
        }
        ctx.require_opt::<Group>(self.parent_id)?;
        ctx.require_login(self.id, self.settings.identity_id)?;
        let mut groups: HashMap<Id, Group> = ctx
            .list::<Group>()?
            .into_iter()
            .map(|g| (g.id, g))
            .collect();
        groups.insert(self.id, self.clone());
        group_chain(&groups, Some(self.id)).map_err(|_| VaultError::Cycle)?;
        Ok(())
    }

    fn before_delete(ctx: &Ctx<'_>, id: Id) -> Result<(), VaultError> {
        ctx.clear_cascade(id)?;
        let mut children: Vec<Id> = ctx
            .list::<Group>()?
            .into_iter()
            .filter(|g| g.parent_id == Some(id))
            .map(|g| g.id)
            .collect();
        children.extend(
            ctx.list::<Host>()?
                .into_iter()
                .filter(|h| h.group_id == Some(id))
                .map(|h| h.id),
        );
        if children.is_empty() {
            Ok(())
        } else {
            Err(VaultError::Referenced(children))
        }
    }
}

impl sealed::Hooks for Host {
    /// `forward_rule_ids` needs nothing here: `before_upsert` rebuilds it
    /// from the rules that exist.
    fn clear_dangling(&mut self, ctx: &Ctx<'_>) -> Result<(), VaultError> {
        self.group_id = ctx.live_or_unset::<Group>(self.group_id)?;
        self.settings.identity_id = ctx.live_or_unset::<Identity>(self.settings.identity_id)?;
        let mut tags = Vec::with_capacity(self.tag_ids.len());
        for tag in &self.tag_ids {
            if ctx.row::<Tag>(*tag)?.is_some() {
                tags.push(*tag);
            }
        }
        self.tag_ids = tags;
        Ok(())
    }

    fn before_upsert(&mut self, ctx: &Ctx<'_>) -> Result<(), VaultError> {
        ctx.require_opt::<Group>(self.group_id)?;
        ctx.require_login(self.id, self.settings.identity_id)?;
        for tag in &self.tag_ids {
            ctx.require::<Tag>(*tag)?;
        }
        // Rebuild the rule list from the rules themselves, keeping the
        // caller's order for ids that are really this host's.
        let owned: Vec<Id> = ctx
            .list::<ForwardRule>()?
            .into_iter()
            .filter(|r| r.host_id == self.id)
            .map(|r| r.id)
            .collect();
        let mut seen = HashSet::new();
        let mut ids: Vec<Id> = self
            .forward_rule_ids
            .iter()
            .copied()
            .filter(|id| owned.contains(id) && seen.insert(*id))
            .collect();
        ids.extend(owned.into_iter().filter(|id| seen.insert(*id)));
        self.forward_rule_ids = ids;
        Ok(())
    }

    fn before_delete(ctx: &Ctx<'_>, id: Id) -> Result<(), VaultError> {
        ctx.delete_rules_of(id)
    }
}

impl sealed::Hooks for Tag {
    fn before_delete(ctx: &Ctx<'_>, id: Id) -> Result<(), VaultError> {
        for mut host in ctx.list::<Host>()? {
            if host.tag_ids.contains(&id) {
                host.tag_ids.retain(|t| *t != id);
                ctx.put(&host)?;
            }
        }
        Ok(())
    }
}

impl sealed::Hooks for Identity {
    fn clear_dangling(&mut self, ctx: &Ctx<'_>) -> Result<(), VaultError> {
        self.key_id = ctx.live_or_unset::<SshKey>(self.key_id)?;
        Ok(())
    }

    fn before_upsert(&mut self, ctx: &Ctx<'_>) -> Result<(), VaultError> {
        ctx.require_opt::<SshKey>(self.key_id)
    }

    fn before_delete(ctx: &Ctx<'_>, id: Id) -> Result<(), VaultError> {
        let mut users: Vec<Id> = ctx
            .list::<Host>()?
            .into_iter()
            .filter(|h| h.settings.identity_id == Some(id))
            .map(|h| h.id)
            .collect();
        users.extend(
            ctx.list::<Group>()?
                .into_iter()
                .filter(|g| g.settings.identity_id == Some(id))
                .map(|g| g.id),
        );
        if users.is_empty() {
            Ok(())
        } else {
            Err(VaultError::Referenced(users))
        }
    }
}

impl sealed::Hooks for SshKey {
    fn before_delete(ctx: &Ctx<'_>, id: Id) -> Result<(), VaultError> {
        let users: Vec<Id> = ctx
            .list::<Identity>()?
            .into_iter()
            .filter(|i| i.key_id == Some(id))
            .map(|i| i.id)
            .collect();
        if users.is_empty() {
            Ok(())
        } else {
            Err(VaultError::Referenced(users))
        }
    }
}

impl sealed::Hooks for ForwardRule {
    fn before_upsert(&mut self, ctx: &Ctx<'_>) -> Result<(), VaultError> {
        let mut host = ctx
            .get::<Host>(self.host_id)?
            .ok_or(VaultError::MissingRef(self.host_id))?;
        if let Some(previous) = ctx.get::<ForwardRule>(self.id)?
            && previous.host_id != self.host_id
            && let Some(mut old_host) = ctx.get::<Host>(previous.host_id)?
        {
            old_host.forward_rule_ids.retain(|r| *r != self.id);
            ctx.put(&old_host)?;
        }
        if !host.forward_rule_ids.contains(&self.id) {
            host.forward_rule_ids.push(self.id);
            ctx.put(&host)?;
        }
        Ok(())
    }

    fn before_delete(ctx: &Ctx<'_>, id: Id) -> Result<(), VaultError> {
        let rule = ctx.get::<ForwardRule>(id)?.ok_or_else(|| not_found(id))?;
        if let Some(mut host) = ctx.get::<Host>(rule.host_id)? {
            host.forward_rule_ids.retain(|r| *r != id);
            ctx.put(&host)?;
        }
        Ok(())
    }
}
