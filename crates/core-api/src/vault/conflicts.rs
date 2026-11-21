//! Listing and resolving conflict records (the Conflicts view).
//!
//! A [`Conflict`] holds the losing version's envelope still sealed under its
//! own AAD (record id, kind, key version); it is opened here only to show or
//! restore it. Views carry both versions as JSON with every secret field
//! (see `secret_fields`) replaced by `{"hasSecret": true}`; the values come
//! out only through [`VaultService::reveal_conflict_field`].

use std::collections::HashSet;

use core_model::{Conflict, ForwardRule, Group, Host, Id, Identity, Kind, KnownHost, Snippet, SshKey, Tag};
use core_store::{RecordRow, StoreError, open_row};
use secrecy::{ExposeSecret, SecretString};
use serde_json::{Map, Value};
use zeroize::Zeroize;

use super::crud::{Ctx, Entity, secret_fields};
use super::{VaultError, VaultService};

/// How to settle a conflict. Every choice also dismisses it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// Keep the current version; the other one is dropped.
    KeepCurrent,
    /// Replace the record with the other version (deleting it again if the
    /// other side was a deletion).
    UseOther,
    /// Take these top-level fields (JSON names, as in [`ConflictView`]) from
    /// the other version into the current one. Secret fields may be named.
    Fields(Vec<String>),
}

/// One side of a conflict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictSide {
    /// The record as it is now (the version that won).
    Current,
    /// The losing version kept in the conflict.
    Other,
}

/// A conflict as shown to the user. Holds no secret: secret fields read
/// `{"hasSecret": true}` (or `null` when unset).
#[derive(Debug, Clone, PartialEq)]
pub struct ConflictView {
    /// Id of the conflict record.
    pub id: Id,
    /// The record both versions belong to.
    pub record_id: Id,
    /// Its kind.
    pub kind: Kind,
    /// The record now; `None` if it has been deleted since.
    pub current: Option<Value>,
    /// The losing version; `None` when it was a deletion.
    pub other: Option<Value>,
    /// Top-level fields whose values differ (secrets compared by value),
    /// sorted. Every field of the present side when one side is missing.
    pub changed_fields: Vec<String>,
    /// Unix seconds when the conflict was detected.
    pub detected_at: i64,
}

fn invalid(what: &'static str) -> VaultError {
    VaultError::Invalid(what)
}

fn not_found(id: Id) -> VaultError {
    VaultError::Store(StoreError::NotFound { id })
}

/// Replaces the secret fields of a record value with the marker (or leaves
/// `null`), wiping the strings they held.
fn mask(kind: Kind, value: &mut Value) {
    let Value::Object(map) = value else { return };
    for field in secret_fields(kind) {
        if let Some(slot) = map.get_mut(*field)
            && !slot.is_null()
        {
            let old = std::mem::replace(slot, serde_json::json!({"hasSecret": true}));
            wipe(old);
        }
    }
}

/// Zeroizes every string in `value` before freeing it.
fn wipe(value: Value) {
    match value {
        Value::String(mut s) => s.zeroize(),
        Value::Array(items) => items.into_iter().for_each(wipe),
        Value::Object(map) => map.into_iter().for_each(|(_, v)| wipe(v)),
        _ => {}
    }
}

fn as_object<'a>(v: Option<&'a Value>, empty: &'a Map<String, Value>) -> &'a Map<String, Value> {
    match v {
        Some(Value::Object(m)) => m,
        _ => empty,
    }
}

fn changed_fields(current: Option<&Value>, other: Option<&Value>) -> Vec<String> {
    let empty = Map::new();
    let (a, b) = (as_object(current, &empty), as_object(other, &empty));
    let mut keys: Vec<String> = a.keys().chain(b.keys()).cloned().collect();
    keys.sort();
    keys.dedup();
    keys.retain(|k| a.get(k) != b.get(k));
    keys
}

impl Ctx<'_> {
    fn parse(&self, row: &RecordRow, conflict: Id) -> Result<Value, VaultError> {
        let plaintext = open_row(self.keys.vault_key.expose_secret(), row)?;
        serde_json::from_slice(&plaintext).map_err(|_| VaultError::BadRecord(conflict))
    }

    /// The record as it is now, if it is live and still of the conflict's kind.
    fn current_value(&self, c: &Conflict) -> Result<Option<Value>, VaultError> {
        match self.store.get(&c.record_id)? {
            Some(row)
                if !row.deleted
                    && row.kind == c.record_kind
                    && row.vault_id == self.keys.vault_id =>
            {
                Ok(Some(self.parse(&row, c.id)?))
            }
            _ => Ok(None),
        }
    }

    /// The losing version, opened under its original AAD; `None` for a deletion.
    fn other_value(&self, c: &Conflict) -> Result<Option<Value>, VaultError> {
        if c.deleted {
            return Ok(None);
        }
        let row = RecordRow {
            id: c.record_id,
            vault_id: self.keys.vault_id,
            kind: c.record_kind,
            key_version: c.key_version,
            hlc: c.hlc,
            rev: None,
            deleted: false,
            envelope: c.envelope.clone(),
        };
        Ok(Some(self.parse(&row, c.id)?))
    }

    fn side_value(&self, c: &Conflict, side: ConflictSide) -> Result<Option<Value>, VaultError> {
        match side {
            ConflictSide::Current => self.current_value(c),
            ConflictSide::Other => self.other_value(c),
        }
    }

    fn conflict_view(&self, c: &Conflict) -> Result<ConflictView, VaultError> {
        let mut current = self.current_value(c)?;
        let mut other = self.other_value(c)?;
        let changed_fields = changed_fields(current.as_ref(), other.as_ref());
        for v in [&mut current, &mut other].into_iter().flatten() {
            mask(c.record_kind, v);
        }
        Ok(ConflictView {
            id: c.id,
            record_id: c.record_id,
            kind: c.record_kind,
            current,
            other,
            changed_fields,
            detected_at: c.detected_at,
        })
    }

    fn conflict(&self, id: Id) -> Result<Conflict, VaultError> {
        self.get::<Conflict>(id)?.ok_or_else(|| not_found(id))
    }
}

/// Runs `$f::<T>(args)` for the entity type of `$kind`. Kinds without an
/// entity (preferences, conflicts) cannot be resolved: `$err`.
macro_rules! by_kind {
    ($kind:expr, $err:expr, $f:ident($($arg:expr),*)) => {
        match $kind {
            Kind::Group => $f::<Group>($($arg),*),
            Kind::Host => $f::<Host>($($arg),*),
            Kind::Tag => $f::<Tag>($($arg),*),
            Kind::Identity => $f::<Identity>($($arg),*),
            Kind::SshKey => $f::<SshKey>($($arg),*),
            Kind::KnownHost => $f::<KnownHost>($($arg),*),
            Kind::ForwardRule => $f::<ForwardRule>($($arg),*),
            Kind::Snippet => $f::<Snippet>($($arg),*),
            Kind::Preference | Kind::Conflict => Err($err),
        }
    };
}

/// Writes `value` as record `id` (it must decode as a `T` with that id).
fn write_value<T: Entity>(ctx: &Ctx<'_>, id: Id, value: Value) -> Result<(), VaultError> {
    let entity: T = serde_json::from_value(value).map_err(|_| VaultError::BadRecord(id))?;
    if entity.id() != id {
        return Err(VaultError::BadRecord(id));
    }
    ctx.restore(entity)
}

fn delete_value<T: Entity>(ctx: &Ctx<'_>, id: Id) -> Result<(), VaultError> {
    ctx.delete_checked::<T>(id)
}

impl VaultService {
    /// Ids of the records that open conflicts are about (conflicts this
    /// build cannot decode are left out, as in `list_conflicts`).
    pub(super) fn conflicted_records(&self) -> Result<HashSet<Id>, VaultError> {
        let ctx = self.ctx()?;
        let mut ids = HashSet::new();
        for row in ctx.store.list(&ctx.keys.vault_id, Kind::Conflict)? {
            match ctx.decode::<Conflict>(&row) {
                Ok(c) => {
                    ids.insert(c.record_id);
                }
                Err(VaultError::BadRecord(_)) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(ids)
    }

    /// Every open conflict, oldest first. A conflict about a record kind
    /// this build does not know (written by a newer version) is left out,
    /// not an error.
    pub fn list_conflicts(&self) -> Result<Vec<ConflictView>, VaultError> {
        let ctx = self.ctx()?;
        let mut views = Vec::new();
        for row in ctx.store.list(&ctx.keys.vault_id, Kind::Conflict)? {
            let conflict: Conflict = match ctx.decode(&row) {
                Ok(c) => c,
                Err(VaultError::BadRecord(_)) => continue,
                Err(e) => return Err(e),
            };
            views.push(ctx.conflict_view(&conflict)?);
        }
        views.sort_by_key(|v| (v.detected_at, v.id));
        Ok(views)
    }

    /// Settles conflict `id` (see [`Resolution`]) and dismisses it, in one
    /// transaction. References in the written version to records that no
    /// longer exist are unset (a forward rule's required `host_id` cannot
    /// be, so a rule whose host is gone stays `MissingRef`); the other
    /// checks (cycles, foreign own logins) still apply. `Fields` needs both
    /// versions to exist ([`VaultError::Invalid`] otherwise).
    pub fn resolve_conflict(&mut self, id: Id, resolution: Resolution) -> Result<(), VaultError> {
        let ctx = self.ctx()?;
        ctx.atomically(|| {
            let c = ctx.conflict(id)?;
            let (kind, record) = (c.record_kind, c.record_id);
            let unsupported = invalid("this kind of record cannot be resolved");
            match resolution {
                Resolution::KeepCurrent => {}
                Resolution::UseOther => match ctx.other_value(&c)? {
                    None => by_kind!(kind, unsupported, delete_value(&ctx, record))?,
                    Some(other) => by_kind!(kind, unsupported, write_value(&ctx, record, other))?,
                },
                Resolution::Fields(names) => {
                    let current = ctx.current_value(&c)?;
                    let other = ctx.other_value(&c)?;
                    let (Some(Value::Object(mut merged)), Some(Value::Object(other))) =
                        (current, other)
                    else {
                        return Err(invalid("both versions must exist to pick fields"));
                    };
                    for name in names {
                        match other.get(&name) {
                            Some(v) => {
                                if let Some(old) = merged.insert(name, v.clone()) {
                                    wipe(old);
                                }
                            }
                            None => {
                                if let Some(old) = merged.remove(&name) {
                                    wipe(old);
                                }
                            }
                        }
                    }
                    wipe(Value::Object(other));
                    by_kind!(
                        kind,
                        unsupported,
                        write_value(&ctx, record, Value::Object(merged))
                    )?;
                }
            }
            ctx.tombstone(id)
        })
    }

    /// The value of secret field `field` (e.g. `password`) in one side of
    /// conflict `id`, for the view's reveal button. [`VaultError::Invalid`]
    /// for a field that is not a secret of that kind, or holds none.
    pub fn reveal_conflict_field(
        &self,
        id: Id,
        side: ConflictSide,
        field: &str,
    ) -> Result<SecretString, VaultError> {
        let ctx = self.ctx()?;
        let c = ctx.conflict(id)?;
        if !secret_fields(c.record_kind).contains(&field) {
            return Err(invalid("not a secret field"));
        }
        let Some(mut value) = ctx.side_value(&c, side)? else {
            return Err(invalid("that version is a deletion"));
        };
        let secret = match value.get_mut(field).map(Value::take) {
            Some(Value::String(s)) => Ok(SecretString::from(s)),
            Some(other) => {
                wipe(other);
                Err(invalid("no secret is stored in that field"))
            }
            None => Err(invalid("no secret is stored in that field")),
        };
        wipe(value);
        secret
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_model::Extra;

    const CANARY: &str = "CANARY-conflict-7f3";

    /// Every string that carries the canary must sit in a listed secret field.
    fn assert_masked<T: Entity>(entity: &T, kind: Kind) {
        let mut value = serde_json::to_value(entity).unwrap();
        assert!(value.to_string().contains(CANARY));
        mask(kind, &mut value);
        assert!(!value.to_string().contains(CANARY), "{kind:?}: {value}");
    }

    #[test]
    fn secret_fields_cover_every_secret() {
        let identity = Identity {
            id: Id::now_v7(),
            label: "l".into(),
            username: "u".into(),
            password: Some(SecretString::from(CANARY)),
            key_id: None,
            owner: None,
            extra: Extra::default(),
        };
        assert_masked(&identity, Kind::Identity);
        let key = SshKey {
            id: Id::now_v7(),
            label: "l".into(),
            algorithm: "ssh-ed25519".into(),
            private_openssh: SecretString::from(format!("{CANARY}-private")),
            passphrase: Some(SecretString::from(format!("{CANARY}-passphrase"))),
            public_openssh: "ssh-ed25519 AAAA".into(),
            certificate: None,
            extra: Extra::default(),
        };
        assert_masked(&key, Kind::SshKey);
    }

    #[test]
    fn mask_leaves_unset_secrets_null() {
        let mut v = serde_json::json!({"password": null, "username": "root"});
        mask(Kind::Identity, &mut v);
        assert_eq!(v, serde_json::json!({"password": null, "username": "root"}));
    }

    #[test]
    fn changed_fields_compare_by_value_and_cover_missing_sides() {
        let a = serde_json::json!({"id": 1, "label": "x", "password": "p1"});
        let b = serde_json::json!({"id": 1, "label": "x", "password": "p2"});
        assert_eq!(changed_fields(Some(&a), Some(&b)), vec!["password"]);
        assert_eq!(
            changed_fields(Some(&a), None),
            vec!["id", "label", "password"]
        );
    }
}
