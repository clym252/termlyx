//! The vault's side of sync: the outbox, the inbound queue and the repairs.
//!
//! Only this crate holds the vault key, so it is where pulled rows are
//! merged ([`merge`](super::merge)); the network engine moves rows between
//! these methods and the server. Rows travel as opaque envelopes: nothing
//! here decrypts except where a rule needs the content (same-content check,
//! group repair).

use std::collections::{HashMap, HashSet};
use std::ops::AddAssign;

use core_model::{Group, Hlc, Id, Kind};
use core_store::{InboundRow, RecordRow};

use super::{VaultError, VaultService, merge};

/// Inbound rows applied per store transaction.
const APPLY_BATCH: usize = 200;

/// What applying remote rows did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MergeReport {
    /// Rows of a known kind that were processed (taken, kept local, or
    /// ignored as already seen).
    pub applied: usize,
    /// Records whose visible state changed: a remote row taken over a
    /// different (or missing) local version, an orphaned login adopted, or
    /// a dependent revived. Echoes of this device's own pushes and rows
    /// kept local do not count; conflict records are in `conflicts`.
    pub changed: usize,
    /// Conflict records created.
    pub conflicts: usize,
    /// Rows of a kind this build does not know, skipped.
    pub skipped_unknown: usize,
}

impl AddAssign for MergeReport {
    fn add_assign(&mut self, o: Self) {
        self.applied += o.applied;
        self.changed += o.changed;
        self.conflicts += o.conflicts;
        self.skipped_unknown += o.skipped_unknown;
    }
}

impl MergeReport {
    /// Whether anything a list could show changed (records or conflicts).
    pub fn touched(&self) -> bool {
        self.changed > 0 || self.conflicts > 0
    }
}

impl VaultService {
    /// Up to `limit` rows with unpushed changes (tombstones and conflict
    /// records included), oldest HLC first. Each carries its base `rev`
    /// (`None` for a record the server has never acknowledged).
    pub fn outbox(&self, limit: usize) -> Result<Vec<RecordRow>, VaultError> {
        self.ctx()?;
        Ok(self.store.dirty_rows(limit)?)
    }

    /// Records that the server accepted the version of `id` stamped `hlc`
    /// as revision `rev`. A newer local edit made during the push stays
    /// unpushed. Works while locked.
    pub fn ack_pushed(&mut self, id: Id, hlc: Hlc, rev: i64) -> Result<(), VaultError> {
        let store = &self.store;
        store.atomically(|| -> Result<(), VaultError> {
            store.mark_synced(&id, &hlc, rev)?;
            // An owner's delete the server accepted stands: its cascade
            // stash is not needed any more.
            if let Some(row) = store.get(&id)?
                && row.deleted
                && row.hlc == hlc
                && matches!(row.kind, Kind::Host | Kind::Group)
            {
                super::cascade::forget_cascade(store, id)?;
            }
            Ok(())
        })
    }

    /// Appends pulled rows to the inbound queue (a re-pulled `seq` is
    /// ignored). Works while locked: nothing is decrypted.
    pub fn queue_inbound(&mut self, rows: Vec<InboundRow>) -> Result<(), VaultError> {
        Ok(self.store.inbound_push(&rows)?)
    }

    /// Drains the inbound queue through the merge rules, in ascending `seq`
    /// (within a batch, owner kinds first; see merge rule 6).
    /// Each batch is one transaction that also removes its rows from the
    /// queue, so a failure keeps the unapplied rows queued (earlier batches
    /// stay applied). Rows of unknown kinds are skipped and leave the queue.
    pub fn apply_inbound(&mut self) -> Result<MergeReport, VaultError> {
        let ctx = self.ctx()?;
        let mut report = MergeReport::default();
        loop {
            let mut batch = ctx.store.inbound_take(APPLY_BATCH)?;
            let Some(last) = batch.last().map(|r| r.seq) else {
                break;
            };
            // Owners before dependents (merge rule 6); stable, so rows of one
            // record keep their seq order.
            batch.sort_by_key(|r| merge::apply_rank(&r.kind));
            report += ctx.atomically(|| {
                let mut part = MergeReport::default();
                for row in &batch {
                    ctx.merge_row(row.id, row, &mut part)?;
                }
                ctx.store.inbound_delete_through(last)?;
                Ok(part)
            })?;
        }
        // Owners restored in this queue get their cascade back now that
        // every row is merged (R33).
        report.changed += ctx.atomically(|| ctx.revive_restored())?;
        Ok(report)
    }

    /// Applies the server's `current` row after a push of record `local_id`
    /// was refused as a conflict, with the same rules as a pulled row
    /// (`current.seq` is not used). If the local version wins it stays in
    /// the outbox, now based on `current.rev`. Call
    /// [`revive_restored_owners`](Self::revive_restored_owners) once the
    /// whole push's results are applied.
    pub fn apply_conflict(
        &mut self,
        local_id: Id,
        current: InboundRow,
    ) -> Result<MergeReport, VaultError> {
        let ctx = self.ctx()?;
        ctx.atomically(|| {
            let mut report = MergeReport::default();
            ctx.merge_row(local_id, &current, &mut report)?;
            Ok(report)
        })
    }

    /// Repairs what concurrent edits can leave behind. Call only when fully
    /// caught up (last page pulled and applied, outbox empty), so every
    /// device decides on the same rows.
    ///
    /// Group cycles: on each cycle, the group whose row has the lowest HLC
    /// (ties by id) moves to the top level, an ordinary write (new HLC,
    /// pushed), so devices that repair independently write the same change.
    /// Never deletes anything and does not run
    /// [`tidy_own_logins`](Self::tidy_own_logins). Returns how many groups
    /// were moved.
    pub fn repair(&mut self) -> Result<usize, VaultError> {
        let ctx = self.ctx()?;
        ctx.atomically(|| {
            let mut groups: HashMap<Id, (Group, Hlc)> = HashMap::new();
            for row in ctx.store.list(&ctx.keys.vault_id, Kind::Group)? {
                let group: Group = ctx.decode(&row)?;
                groups.insert(group.id, (group, row.hlc));
            }
            let mut moved = 0;
            for cycle in group_cycles(&groups) {
                let pick = cycle
                    .iter()
                    .min_by_key(|id| (groups[*id].1, **id))
                    .copied()
                    .expect("a cycle is never empty");
                let mut group = groups[&pick].0.clone();
                group.parent_id = None;
                ctx.put(&group)?;
                moved += 1;
            }
            Ok(moved)
        })
    }

    /// Number of records with unpushed changes (progress totals).
    pub fn outbox_len(&self) -> Result<usize, VaultError> {
        Ok(self.store.dirty_count()?)
    }

    /// Number of pulled rows waiting to be applied.
    pub fn inbound_len(&self) -> Result<usize, VaultError> {
        Ok(self.store.inbound_len()?)
    }

    /// Full resync, step one: forgets every server revision, keeps the
    /// dirty flags and empties the inbound queue. Works locked.
    pub fn reset_revs(&mut self) -> Result<(), VaultError> {
        let store = &self.store;
        store.atomically(|| -> Result<(), VaultError> {
            store.reset_revs()?;
            store.inbound_clear()?;
            Ok(())
        })
    }

    /// Full resync, last step (and enabling sync): queues every live record
    /// the server does not have (no revision) for upload as new.
    pub fn mark_unsynced_dirty(&mut self) -> Result<(), VaultError> {
        Ok(self.store.mark_unsynced_dirty()?)
    }

    /// Starts a full resync in one transaction: [`reset_revs`](Self::reset_revs)
    /// (revs forgotten, dirty flags kept, queue emptied) together with the
    /// engine's bookkeeping (`set` written, `delete` removed from
    /// `sync_state`), so a crash can never leave revs forgotten without the
    /// resync recorded. Works locked.
    pub fn begin_resync(
        &mut self,
        set: &[(&str, &[u8])],
        delete: &[&str],
    ) -> Result<(), VaultError> {
        let store = &self.store;
        store.atomically(|| -> Result<(), VaultError> {
            store.reset_revs()?;
            store.inbound_clear()?;
            Self::write_state(store, set, delete)
        })
    }

    /// Completes a full resync in one transaction:
    /// [`mark_unsynced_dirty`](Self::mark_unsynced_dirty) plus the engine's
    /// bookkeeping (`set` / `delete` in `sync_state`).
    pub fn finish_resync(
        &mut self,
        set: &[(&str, &[u8])],
        delete: &[&str],
    ) -> Result<(), VaultError> {
        let store = &self.store;
        store.atomically(|| -> Result<(), VaultError> {
            store.mark_unsynced_dirty()?;
            Self::write_state(store, set, delete)
        })
    }

    fn write_state(
        store: &core_store::Store,
        set: &[(&str, &[u8])],
        delete: &[&str],
    ) -> Result<(), VaultError> {
        for (k, v) in set {
            store.sync_put(k, v)?;
        }
        for k in delete {
            store.sync_delete(k)?;
        }
        Ok(())
    }

    /// The sync engine's bookkeeping value under `k` (`sync_state`). Not
    /// encrypted: never store anything secret here except the documented
    /// credential fallback.
    pub fn sync_get(&self, k: &str) -> Result<Option<Vec<u8>>, VaultError> {
        Ok(self.store.sync_get(k)?)
    }

    /// Stores a `sync_state` value.
    pub fn sync_put(&mut self, k: &str, v: &[u8]) -> Result<(), VaultError> {
        Ok(self.store.sync_put(k, v)?)
    }

    /// Stores several `sync_state` values in one transaction.
    pub fn sync_put_all(&mut self, kv: &[(&str, &[u8])]) -> Result<(), VaultError> {
        let store = &self.store;
        store.atomically(|| -> Result<(), VaultError> {
            for (k, v) in kv {
                store.sync_put(k, v)?;
            }
            Ok(())
        })
    }

    /// Removes a `sync_state` value.
    pub fn sync_delete(&mut self, k: &str) -> Result<(), VaultError> {
        Ok(self.store.sync_delete(k)?)
    }

    /// Turns sync bookkeeping off: empties `sync_state` and the inbound
    /// queue, and drops unproven server wraps (`pending.*`). Records and the
    /// account's own rows are untouched.
    pub fn sync_clear(&mut self) -> Result<(), VaultError> {
        let store = &self.store;
        store.atomically(|| -> Result<(), VaultError> {
            super::account_sync::drop_pending(store)?;
            Ok(store.sync_clear()?)
        })
    }

    /// Test-only: a second device's vault file at `path` with the same
    /// account (vault id, keys, password and KDF parameters, recovery wrap),
    /// HLC node `node` and no records. Device-specific state (quick unlock)
    /// is not copied. The result is locked: unlock it with the same master
    /// password. Only built with the `test-util` feature.
    #[cfg(feature = "test-util")]
    #[doc(hidden)]
    pub fn clone_for_device(
        &self,
        path: &std::path::Path,
        node: u16,
    ) -> Result<VaultService, VaultError> {
        use super::MemoryKeyStore;
        use super::service::keys;

        const SHARED: [&str; 7] = [
            keys::VAULT_ID,
            keys::KEY_VERSION,
            keys::KDF,
            keys::ACCOUNT_PUBLIC,
            keys::SECRET_BY_KEK,
            keys::SECRET_BY_RECOVERY,
            keys::SEALED_VAULT_KEY,
        ];
        let target = VaultService::open_for_tests(path, Box::new(MemoryKeyStore::default()))?;
        if target.is_initialized() {
            return Err(VaultError::AlreadyInitialized);
        }
        let store = &target.store;
        store.atomically(|| -> Result<(), VaultError> {
            for k in SHARED {
                let v = self
                    .store
                    .account_get(k)?
                    .ok_or(VaultError::NotInitialized)?;
                store.account_put(k, &v)?;
            }
            store.account_put(keys::HLC_NODE, &node.to_le_bytes())?;
            Ok(())
        })?;
        drop(target);
        VaultService::open_for_tests(path, Box::new(MemoryKeyStore::default()))
    }
}

/// Every cycle in the group parent graph, each as its member ids. A parent
/// that is not a live group ends the chain (it reads as top level). Each
/// group has one parent, so cycles are disjoint and fixing one cannot
/// create or break another.
fn group_cycles<T>(groups: &HashMap<Id, (Group, T)>) -> Vec<Vec<Id>> {
    let mut done: HashSet<Id> = HashSet::new();
    let mut cycles = Vec::new();
    let mut ids: Vec<Id> = groups.keys().copied().collect();
    ids.sort();
    for start in ids {
        let mut path: Vec<Id> = Vec::new();
        let mut next = Some(start);
        while let Some(id) = next {
            if done.contains(&id) {
                break;
            }
            if let Some(pos) = path.iter().position(|p| *p == id) {
                cycles.push(path[pos..].to_vec());
                break;
            }
            let Some((group, _)) = groups.get(&id) else {
                break;
            };
            path.push(id);
            next = group.parent_id;
        }
        done.extend(path);
    }
    cycles
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_model::ConnSettings;

    fn g(id: u128, parent: Option<u128>) -> (Id, (Group, ())) {
        let group = Group {
            id: Id::from_u128(id),
            parent_id: parent.map(Id::from_u128),
            name: String::new(),
            settings: ConnSettings::default(),
            extra: Default::default(),
        };
        (group.id, (group, ()))
    }

    fn sorted(mut cycles: Vec<Vec<Id>>) -> Vec<Vec<Id>> {
        for c in &mut cycles {
            c.sort();
        }
        cycles.sort();
        cycles
    }

    #[test]
    fn finds_disjoint_cycles_and_ignores_tails_and_missing_parents() {
        let groups: HashMap<_, _> = [
            g(1, Some(2)),
            g(2, Some(1)),
            g(3, Some(1)),  // tail into the 1-2 cycle
            g(4, Some(99)), // missing parent: top level
            g(5, Some(5)),  // self loop
            g(6, Some(7)),
            g(7, Some(8)),
            g(8, Some(6)),
            g(9, None),
        ]
        .into_iter()
        .collect();
        let id = Id::from_u128;
        assert_eq!(
            sorted(group_cycles(&groups)),
            vec![vec![id(1), id(2)], vec![id(5)], vec![id(6), id(7), id(8)]]
        );
    }
}
