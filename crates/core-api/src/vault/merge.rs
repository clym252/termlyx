//! Merge rules for a row that came from the server (spec "Merge rules").
//!
//! For a remote row R and the local row L with the same id:
//!
//! 0. R is live but its envelope does not open under the vault key: rejected
//!    (counted), nothing stored; see `Ctx::reject`. Tombstones carry no
//!    envelope and cannot be checked (format v1, A-M1).
//! 1. R's kind is unknown to this build: skipped (counted), nothing stored.
//! 2. No L, or L has no unpushed change: R is taken as synced. So is R when
//!    it carries L's own version (same HLC: our push came back).
//! 3. R is not newer than what L is based on (`R.rev <= L.rev`): a
//!    re-delivered row, ignored. A full resync clears every `rev` first
//!    (`VaultService::reset_revs`), so this never hides a rewound server.
//!    During a resync a clean L without a revision is a version some server
//!    history accepted; if it is newer than R (higher HLC: the server was
//!    restored from an older backup) it is kept and pushed, based on
//!    `R.rev`, instead of being overwritten. No conflict: R is its ancestor.
//! 4. Both changed:
//!    * delete vs delete: R is taken, no conflict;
//!    * edit vs delete (either way round): the edit wins (a local tombstone
//!      is replaced by R, restoring the record) and the deletion is recorded
//!      as the conflict's other side;
//!    * edit vs edit with the same content: R is taken, no conflict (two
//!      devices made the same change, e.g. the same repair);
//!    * edit vs edit: the higher HLC wins and the loser's envelope is kept,
//!      byte for byte, in a [`Conflict`] record.
//!
//!    When L wins it stays dirty with `rev = R.rev`, so its next push is
//!    based on the server's current revision and goes through.
//! 5. Conflict records themselves never produce conflicts: a dismissal
//!    (tombstone) beats a live copy, otherwise R is taken (two devices that
//!    detect the same conflict write the same id, see [`conflict_id`]).
//!
//! 6. Cascaded deletes do not take a restored owner's dependents with them
//!    (rulings R20, R27, R28). Deleting a host tombstones its forward rules
//!    and, through `tidy_own_logins`, its own login; a concurrent edit
//!    restores the host, so those tombstones must not win either:
//!    * a remote tombstone for a clean, live forward rule whose host still
//!      lists it, or for an own login whose owner still links it, is not
//!      applied: the local version is kept, re-stamped with a new HLC (after
//!      the tombstone's), based on the tombstone's `rev` and left dirty, so
//!      the next push restores it on the other devices too. "Still" means
//!      as the owner will be once merged: a dirty live local owner, else the
//!      newest queued row for it if newer than the local one, else the local
//!      row. Not when a newer row for the dependent waits in the queue
//!      (R34): the tombstone then applies and that row follows (keeping the
//!      older local copy, re-stamped, would beat it). Otherwise the
//!      deletion is recorded as a conflict (its losing side, R27),
//!      so the user sees it was deleted elsewhere and can delete again. This
//!      matters beyond cascades: a rule deleted on one device while another
//!      edits its host (from a copy that still lists the rule) survives when
//!      that edit wins, and without the conflict the deletion would vanish.
//!      A rule removed from its host, or a login its owner stopped using,
//!      whose unlinking write is the one merged still goes away as usual;
//!    * the deleting device revives its own cascade when its owner tombstone
//!      loses edit-beats-delete (R28, R30–R33), after the whole batch (the
//!      pull queue, or a push's results) is merged: the dependents' last live rows,
//!      kept in the cascade stash ([`cascade`](super::cascade)) when they
//!      were tombstoned, are written back live (new HLC, dirty) if the owner
//!      as merged still links them and their row is still exactly the
//!      cascade's tombstone (R31: any later write or delete of the dependent
//!      drops its stash entry). Without it, a dependent whose tombstone already reached
//!      the server, and which no other device still holds live, is lost
//!      while its owner comes back;
//!    * an own login kept alive by edit-beats-delete whose owner no longer
//!      links it (the host is gone) becomes a Keychain identity
//!      (`owner = None`, an ordinary write), so its password stays visible
//!      and `tidy_own_logins` leaves it alone.
//!
//!    `apply_inbound` applies owner kinds (groups, hosts) before the rest of
//!    a batch and dependents (identities, forward rules) last.
//!
//! Every remote row's HLC is observed first (rule 5 of the spec), whatever
//! happens to it.

use std::time::{SystemTime, UNIX_EPOCH};

use core_model::{Conflict, ForwardRule, Group, Hlc, Host, Id, Identity, Kind};
use core_store::{InboundRow, RecordRow, open_row};
use secrecy::ExposeSecret;
use sha2::{Digest, Sha256};

use super::VaultError;
use super::crud::Ctx;
use super::sync_local::MergeReport;

/// Domain label for [`conflict_id`].
const CONFLICT_ID_LABEL: &[u8] = b"tether/conflict-id/v1";

/// Id of the conflict record for the losing version `losing_hlc` of record
/// `record_id`, the same on every device so that two devices detecting the
/// same conflict write one record, not two.
///
/// Derivation: the first 16 bytes of
/// `SHA-256("tether/conflict-id/v1" || 0x00 || record_id (16 bytes, big-endian
/// UUID bytes) || losing_hlc as its 30-character text form)`, stamped as a
/// UUIDv8 (RFC 9562 version and variant bits). Both inputs are routing
/// metadata the server already sees, so the id reveals nothing new.
pub(super) fn conflict_id(record_id: Id, losing_hlc: Hlc) -> Id {
    let mut h = Sha256::new();
    h.update(CONFLICT_ID_LABEL);
    h.update([0u8]);
    h.update(record_id.as_bytes());
    h.update(losing_hlc.to_string().as_bytes());
    let digest = h.finalize();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    uuid::Builder::from_custom_bytes(bytes).into_uuid()
}

/// Order in which a batch applies kinds: owners, then the rest, then the
/// records that hang off an owner (rule 6).
pub(super) fn apply_rank(kind: &str) -> u8 {
    match kind.parse::<Kind>() {
        Ok(Kind::Group | Kind::Host) => 0,
        Ok(Kind::Identity | Kind::ForwardRule) => 2,
        _ => 1,
    }
}

/// A host or group, as the owner of a login or (hosts) of forward rules.
enum Owner {
    Host(Host),
    Group(Group),
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

impl Ctx<'_> {
    /// Advances the local clock past `seen`.
    pub(super) fn observe(&self, seen: Hlc) {
        let last = self.keys.last_hlc.get();
        self.keys
            .last_hlc
            .set(Hlc::observe_now(&last, &seen, self.keys.node));
    }

    /// Applies one remote row (see the module docs). Needs an open
    /// transaction. `id` names the record (for a push conflict, the record
    /// that was pushed).
    pub(super) fn merge_row(
        &self,
        id: Id,
        r: &InboundRow,
        report: &mut MergeReport,
    ) -> Result<(), VaultError> {
        self.observe(r.hlc);
        let Ok(kind) = r.kind.parse::<Kind>() else {
            report.skipped_unknown += 1;
            return Ok(());
        };
        let remote = RecordRow {
            id,
            vault_id: self.keys.vault_id,
            kind,
            key_version: r.key_version,
            hlc: r.hlc,
            rev: Some(r.rev),
            deleted: r.deleted,
            envelope: if r.deleted {
                Vec::new()
            } else {
                r.envelope.clone()
            },
        };
        let store = self.store;
        if !remote.deleted && open_row(self.keys.vault_key.expose_secret(), &remote).is_err() {
            return self.reject(id, r.rev, report);
        }
        report.applied += 1;
        let (Some(local), Some(dirty)) = (store.get(&id)?, store.is_dirty(&id)?) else {
            return self.take_remote(&remote, report);
        };
        if local.rev.is_some_and(|base| r.rev <= base) {
            return Ok(());
        }
        if local.hlc == remote.hlc {
            // Our own version, acknowledged by the pull.
            return Ok(store.mark_synced(&id, &remote.hlc, r.rev)?);
        }
        if !dirty {
            if local.rev.is_none() && local.hlc > remote.hlc {
                // Rule 3, resync: our clean copy is newer than the server's.
                return self.keep_local(local, r.rev);
            }
            if remote.deleted
                && !local.deleted
                && !self.newer_queued(id, r)?
                && self.still_linked(&local)?
            {
                // Rule 6: the owner (as merged) still links it. The deletion
                // is kept as a conflict so the user can delete again (R27).
                self.record_conflict(&remote, report)?;
                let mut kept = local;
                kept.hlc = self.next_hlc();
                kept.rev = Some(r.rev);
                return Ok(store.put(&kept)?);
            }
            return self.take_remote(&remote, report);
        }

        if local.kind == Kind::Conflict || remote.kind == Kind::Conflict {
            return if local.deleted && !remote.deleted {
                self.keep_local(local, r.rev)
            } else {
                self.take_remote(&remote, report)
            };
        }
        match (local.deleted, remote.deleted) {
            (true, true) => self.take_remote(&remote, report)?,
            (false, true) => {
                self.record_conflict(&remote, report)?;
                self.keep_local(local, r.rev)?;
                self.adopt_orphan_login(id, report)?;
            }
            (true, false) => {
                self.record_conflict(&local, report)?;
                self.take_remote(&remote, report)?;
                if matches!(remote.kind, Kind::Host | Kind::Group) {
                    // Our delete of this owner lost: what it cascaded comes
                    // back too (R28), once the whole batch is merged (R33).
                    self.mark_restored(id)?;
                }
                self.adopt_orphan_login(id, report)?;
            }
            (false, false) => {
                if self.same_content(&local, &remote) {
                    self.take_remote(&remote, report)?;
                } else if local.hlc > remote.hlc {
                    self.record_conflict(&remote, report)?;
                    self.keep_local(local, r.rev)?;
                } else {
                    self.record_conflict(&local, report)?;
                    self.take_remote(&remote, report)?;
                }
            }
        }
        Ok(())
    }

    /// Rule 0: a live remote row whose envelope does not open under the vault
    /// key (altered, garbage, or another record's) is never stored, or it
    /// would replace a good copy and break every list of its kind. An
    /// unpushed local change is rebased on it, so the next push replaces it
    /// on the server; a clean local copy is left as it is.
    fn reject(&self, id: Id, rev: i64, report: &mut MergeReport) -> Result<(), VaultError> {
        tracing::warn!(record = %id, "remote row does not open; not stored");
        report.rejected += 1;
        if let (Some(local), Some(true)) = (self.store.get(&id)?, self.store.is_dirty(&id)?)
            && local.rev.is_none_or(|base| rev > base)
        {
            self.keep_local(local, rev)?;
        }
        Ok(())
    }

    /// Owner `id` as it will be once merged (rule 6): a dirty live local row
    /// (its edit beats any delete), else the newest queued row for it when
    /// newer than the local one, else the local row. `None` if that is a
    /// tombstone, missing, or not a host or group.
    fn effective_owner(&self, id: Id) -> Result<Option<Owner>, VaultError> {
        let store = self.store;
        let local = store.get(&id)?;
        let dirty_live =
            local.as_ref().is_some_and(|l| !l.deleted) && store.is_dirty(&id)? == Some(true);
        let row = if dirty_live {
            local
        } else {
            let base = local.as_ref().and_then(|l| l.rev);
            match store.inbound_latest(&id)? {
                Some(q) if base.is_none_or(|b| q.rev > b) => {
                    q.kind.parse::<Kind>().ok().map(|kind| RecordRow {
                        id,
                        vault_id: self.keys.vault_id,
                        kind,
                        key_version: q.key_version,
                        hlc: q.hlc,
                        rev: Some(q.rev),
                        deleted: q.deleted,
                        envelope: q.envelope,
                    })
                }
                _ => local,
            }
        };
        let Some(row) = row.filter(|r| !r.deleted && r.vault_id == self.keys.vault_id) else {
            return Ok(None);
        };
        // An owner that does not decode cannot be shown to link anything.
        Ok(match row.kind {
            Kind::Host => self.decode::<Host>(&row).ok().map(Owner::Host),
            Kind::Group => self.decode::<Group>(&row).ok().map(Owner::Group),
            _ => None,
        })
    }

    /// Whether `owner` (as it will be once merged) still links the own
    /// login `identity`.
    fn links_login(&self, owner: Id, identity: Id) -> Result<bool, VaultError> {
        Ok(match self.effective_owner(owner)? {
            Some(Owner::Host(h)) => h.settings.identity_id == Some(identity),
            Some(Owner::Group(g)) => g.settings.identity_id == Some(identity),
            None => false,
        })
    }

    /// Rule 6: whether live dependent row `local` (an own login or a forward
    /// rule) is still linked by its owner. Other kinds, and rows that do not
    /// decode, are not.
    pub(super) fn still_linked(&self, local: &RecordRow) -> Result<bool, VaultError> {
        match local.kind {
            Kind::Identity => match self.decode::<Identity>(local).ok().and_then(|i| i.owner) {
                Some(owner) => self.links_login(owner, local.id),
                None => Ok(false),
            },
            Kind::ForwardRule => match self.decode::<ForwardRule>(local) {
                Ok(rule) => Ok(matches!(
                    self.effective_owner(rule.host_id)?,
                    Some(Owner::Host(h)) if h.forward_rule_ids.contains(&local.id)
                )),
                Err(_) => Ok(false),
            },
            _ => Ok(false),
        }
    }

    /// Rule 6: after edit-beats-delete kept record `id` live, an own login
    /// whose owner no longer links it becomes a Keychain identity.
    fn adopt_orphan_login(&self, id: Id, report: &mut MergeReport) -> Result<(), VaultError> {
        let Some(row) = self.row::<Identity>(id)? else {
            return Ok(());
        };
        let Ok(mut identity) = self.decode::<Identity>(&row) else {
            return Ok(());
        };
        let Some(owner) = identity.owner else {
            return Ok(());
        };
        if !self.links_login(owner, id)? {
            identity.owner = None;
            self.put(&identity)?;
            report.changed += 1;
        }
        Ok(())
    }

    /// Whether the inbound queue holds a row for `id` later than `r` (R34):
    /// then rule 6 must not keep the local version against `r`'s tombstone,
    /// or the re-stamped (older) copy would beat that newer row. The newer
    /// row's writer already kept the deletion as a conflict.
    fn newer_queued(&self, id: Id, r: &InboundRow) -> Result<bool, VaultError> {
        Ok(self
            .store
            .inbound_latest(&id)?
            .is_some_and(|q| q.seq > r.seq && q.rev > r.rev))
    }

    /// Takes the remote row as synced; a stashed copy of the record is
    /// stale from now on (R31). Counts it in `report.changed` unless it
    /// shows nothing new (our own version again, or a tombstone over a
    /// tombstone or a record never seen).
    fn take_remote(&self, remote: &RecordRow, report: &mut MergeReport) -> Result<(), VaultError> {
        self.forget_stashed(remote.id)?;
        let shows_new = match self.store.get(&remote.id)? {
            Some(local) => {
                !(local.hlc == remote.hlc && local.deleted == remote.deleted)
                    && !(local.deleted && remote.deleted)
            }
            None => !remote.deleted,
        };
        self.store.put_synced(remote)?;
        if shows_new {
            report.changed += 1;
        }
        Ok(())
    }

    /// The local version wins: it stays dirty, now based on `rev`.
    fn keep_local(&self, mut local: RecordRow, rev: i64) -> Result<(), VaultError> {
        local.rev = Some(rev);
        Ok(self.store.put(&local)?)
    }

    /// Whether both rows decrypt to the same record. Anything that does not
    /// open or parse counts as different, so a conflict is recorded.
    fn same_content(&self, a: &RecordRow, b: &RecordRow) -> bool {
        if a.kind != b.kind {
            return false;
        }
        let key = self.keys.vault_key.expose_secret();
        let parse = |row: &RecordRow| {
            open_row(key, row)
                .ok()
                .and_then(|p| serde_json::from_slice::<serde_json::Value>(&p).ok())
        };
        match (parse(a), parse(b)) {
            (Some(x), Some(y)) => x == y,
            _ => false,
        }
    }

    /// Stores `loser` in a conflict record, unless the conflict for that
    /// version is already known (live, or dismissed by the user).
    fn record_conflict(
        &self,
        loser: &RecordRow,
        report: &mut MergeReport,
    ) -> Result<(), VaultError> {
        let id = conflict_id(loser.id, loser.hlc);
        if self.store.get(&id)?.is_some() {
            return Ok(());
        }
        let conflict = Conflict {
            id,
            record_id: loser.id,
            record_kind: loser.kind,
            key_version: loser.key_version,
            hlc: loser.hlc,
            deleted: loser.deleted,
            envelope: if loser.deleted {
                Vec::new()
            } else {
                loser.envelope.clone()
            },
            detected_at: unix_now(),
            extra: Default::default(),
        };
        self.put(&conflict)?;
        report.conflicts += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conflict_id_is_deterministic_and_distinct() {
        let record = Id::parse_str("0192f3a4-0000-7000-8000-000000000001").unwrap();
        let hlc = Hlc {
            wall_ms: 0x1234,
            counter: 5,
            node: 0xab,
        };
        let id = conflict_id(record, hlc);
        assert_eq!(id, conflict_id(record, hlc));
        assert_eq!(id.get_version_num(), 8);
        assert_eq!(id.get_variant(), uuid::Variant::RFC4122);
        assert_ne!(id, conflict_id(record, Hlc { counter: 6, ..hlc }));
        assert_ne!(id, conflict_id(Id::now_v7(), hlc));
        // Golden value: the derivation is part of the sync format.
        assert_eq!(id.to_string(), GOLDEN);
    }

    // Computed independently (Python hashlib) from the derivation above.
    const GOLDEN: &str = "2a8744df-4ba9-8911-b476-eb349f912bd7";
}
