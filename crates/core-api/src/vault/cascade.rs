//! The cascade stash (ruling R28): what a host or group delete took with it,
//! so that the dependents come back if the delete loses to an edit.
//!
//! Deleting a host tombstones its forward rules, and `tidy_own_logins` then
//! removes the own login of a deleted host or group. If another device edited
//! that owner meanwhile, the edit wins (merge rule 3) and the owner is
//! restored on every device. Merge rule 6 keeps the dependents alive on the
//! devices that still hold them, but the deleting device's own tombstones may
//! already have reached the server, and if every other copy is gone too
//! nothing would bring them back. So, with sync on, each cascaded dependent's
//! last live row (sealed envelope, kind, key version, HLC, rev: nothing is
//! decrypted) and the HLC of the cascade's tombstone are kept in `sync_state`
//! under `cascade/<owner>/<dependent>`, in the same transaction as the
//! tombstone. When this device's owner tombstone loses edit-beats-delete,
//! the dependents are revived from it: written live again with a new HLC
//! (dirty, based on their stored rev), but only if the dependent's row is
//! still exactly that cascade tombstone (R31; anything else is a later write
//! or delete, made by someone who saw newer content) and the owner as merged
//! still links it (R30, the same link test as merge rule 6), and no newer
//! remote version of it waits in the inbound queue (R32: owners apply
//! before dependents, so a dependent edited along with its owner is still
//! queued when the owner's merge revives; it then applies as usual and
//! beats the cascade delete). A dependent's
//! entry is also dropped as soon as the dependent itself is written locally
//! (an upsert or delete) or a remote row for it is applied (R31).
//! Revival waits until the whole batch is merged (R33): during
//! `apply_inbound` or a push's `apply_conflict` calls, an owner whose delete
//! lost is only recorded (`restored/<owner>` in `sync_state`, so a crash
//! cannot lose it); [`VaultService::revive_restored_owners`] then revives
//! against the final state, in one transaction that also clears the record.
//! So a dependent whose own newer row arrives later in the same batch (a
//! pulled row, or its own push result) is no longer the cascade tombstone
//! by then and is left to that row.
//! The owner's stash is dropped once revived, once the
//! server accepts the owner's tombstone (the delete stands), when the owner
//! is deleted again (replaced), and when sync is turned off (`sync_clear`).
//! It survives a full resync.

use core_model::{Hlc, Id, Kind};
use core_store::{RecordRow, Store};

use super::VaultError;
use super::crud::Ctx;

/// `sync_state` flag the sync engine sets while sync is on.
const SYNC_ENABLED: &str = "enabled";

/// Format version of a stashed row (1, without the tombstone HLC, is
/// ignored).
const STASH_V2: u8 = 2;

/// Key prefix of every stash entry.
const CASCADE: &str = "cascade/";

fn owner_prefix(owner: Id) -> String {
    format!("{CASCADE}{owner}/")
}

/// Whether sync is on for this vault (the engine's `enabled` flag).
fn sync_enabled(store: &Store) -> Result<bool, VaultError> {
    Ok(store.sync_get(SYNC_ENABLED)?.as_deref() == Some(b"1"))
}

/// `[2][kind len][kind][key_version u32 LE][hlc len][hlc][rev flag][rev i64
/// LE][tombstone hlc len][tombstone hlc][envelope…]`.
fn encode(row: &RecordRow, tombstone: Hlc) -> Vec<u8> {
    let kind = row.kind.as_str().as_bytes();
    let (hlc, tomb) = (row.hlc.to_string(), tombstone.to_string());
    let mut out = Vec::with_capacity(row.envelope.len() + 96);
    out.push(STASH_V2);
    out.push(kind.len() as u8);
    out.extend_from_slice(kind);
    out.extend_from_slice(&row.key_version.to_le_bytes());
    out.push(hlc.len() as u8);
    out.extend_from_slice(hlc.as_bytes());
    out.push(u8::from(row.rev.is_some()));
    out.extend_from_slice(&row.rev.unwrap_or(0).to_le_bytes());
    out.push(tomb.len() as u8);
    out.extend_from_slice(tomb.as_bytes());
    out.extend_from_slice(&row.envelope);
    out
}

/// One stash entry: the dependent's last live row and the HLC of the
/// cascade's tombstone over it.
struct Stashed {
    row: RecordRow,
    tombstone: Hlc,
}

fn hlc_field(rest: &[u8]) -> Option<(Hlc, &[u8])> {
    let (&len, rest) = rest.split_first()?;
    let (text, rest) = rest.split_at_checked(len as usize)?;
    Some((std::str::from_utf8(text).ok()?.parse().ok()?, rest))
}

/// The stash entry of record `id`; `None` if malformed (it is then ignored).
fn decode(id: Id, vault_id: Id, bytes: &[u8]) -> Option<Stashed> {
    let (&version, rest) = bytes.split_first()?;
    if version != STASH_V2 {
        return None;
    }
    let (&kind_len, rest) = rest.split_first()?;
    let (kind, rest) = rest.split_at_checked(kind_len as usize)?;
    let kind: Kind = std::str::from_utf8(kind).ok()?.parse().ok()?;
    let (key_version, rest) = rest.split_at_checked(4)?;
    let key_version = u32::from_le_bytes(key_version.try_into().ok()?);
    let (hlc, rest) = hlc_field(rest)?;
    let (&has_rev, rest) = rest.split_first()?;
    let (rev, rest) = rest.split_at_checked(8)?;
    let rev = i64::from_le_bytes(rev.try_into().ok()?);
    let (tombstone, envelope) = hlc_field(rest)?;
    Some(Stashed {
        row: RecordRow {
            id,
            vault_id,
            kind,
            key_version,
            hlc,
            rev: (has_rev == 1).then_some(rev),
            deleted: false,
            envelope: envelope.to_vec(),
        },
        tombstone,
    })
}

impl Ctx<'_> {
    /// Dependent `live.id` of `owner` was just tombstoned by a cascade (in
    /// this transaction): keeps its last live row `live` and the tombstone's
    /// HLC (sync on only).
    pub(super) fn stash_cascaded(&self, owner: Id, live: &RecordRow) -> Result<(), VaultError> {
        if live.deleted || !sync_enabled(self.store)? {
            return Ok(());
        }
        if let Some(tomb) = self.store.get(&live.id)?.filter(|r| r.deleted) {
            let key = format!("{}{}", owner_prefix(owner), live.id);
            self.store.sync_put(&key, &encode(live, tomb.hlc))?;
        }
        Ok(())
    }

    /// Record `id` is being written (locally, or a remote row for it is
    /// applied): a stashed copy of it is stale now (R31).
    pub(super) fn forget_stashed(&self, id: Id) -> Result<(), VaultError> {
        Ok(self
            .store
            .sync_delete_matching(CASCADE, &format!("/{id}"))?)
    }

    /// Drops what an earlier delete of `owner` stashed (it is being deleted
    /// again; its dependents are stashed anew).
    pub(super) fn clear_cascade(&self, owner: Id) -> Result<(), VaultError> {
        Ok(self.store.sync_delete_prefix(&owner_prefix(owner))?)
    }

    /// This device's tombstone of `owner` lost to a remote edit: writes back
    /// as live each stashed dependent whose row is still the cascade's
    /// tombstone and that the owner (as merged) still links, then drops the
    /// stash. Needs an open transaction. Returns how many came back.
    pub(super) fn revive_cascade(&self, owner: Id) -> Result<usize, VaultError> {
        let prefix = owner_prefix(owner);
        let mut revived = 0;
        for (key, bytes) in self.store.sync_prefix(&prefix)? {
            let Some(id) = key[prefix.len()..].parse::<Id>().ok() else {
                continue;
            };
            let Some(Stashed { mut row, tombstone }) = decode(id, self.keys.vault_id, &bytes)
            else {
                continue;
            };
            let Some(current) = self
                .store
                .get(&id)?
                .filter(|r| r.deleted && r.hlc == tombstone)
            else {
                continue; // written or deleted again since (R31)
            };
            if self
                .store
                .inbound_latest(&id)?
                .is_some_and(|q| current.rev.is_none_or(|base| q.rev > base))
            {
                // A newer remote version is queued (in this batch or a later
                // one): it applies as usual, and beats our cascade delete
                // (R32). Reviving the older stashed copy would bury it.
                continue;
            }
            if !self.still_linked(&row)? {
                continue; // the winning owner dropped it (R30)
            }
            row.hlc = self.next_hlc();
            // A dirty write keeps the stored rev (the tombstone's).
            row.rev = None;
            self.store.put(&row)?;
            revived += 1;
        }
        self.store.sync_delete_prefix(&prefix)?;
        Ok(revived)
    }
}

/// Key prefix of the owners whose delete lost, waiting for their revival.
const RESTORED: &str = "restored/";

impl Ctx<'_> {
    /// Owner `owner`'s delete lost to a remote edit: revive its cascade once
    /// the batch is merged.
    pub(super) fn mark_restored(&self, owner: Id) -> Result<(), VaultError> {
        Ok(self.store.sync_put(&format!("{RESTORED}{owner}"), b"1")?)
    }

    /// Revives the cascades of the owners recorded by [`Self::mark_restored`]
    /// and clears the records. Needs an open transaction.
    pub(super) fn revive_restored(&self) -> Result<usize, VaultError> {
        let mut revived = 0;
        for (key, _) in self.store.sync_prefix(RESTORED)? {
            if let Ok(owner) = key[RESTORED.len()..].parse::<Id>() {
                revived += self.revive_cascade(owner)?;
            }
        }
        self.store.sync_delete_prefix(RESTORED)?;
        Ok(revived)
    }
}

impl super::VaultService {
    /// Revives what the deletes of owners restored in the last batch
    /// cascaded (R28, R33), against the merged state, in one transaction.
    /// [`apply_inbound`](Self::apply_inbound) calls it once the queue is
    /// drained; the sync engine calls it after a push's results are applied
    /// (acks and [`apply_conflict`](Self::apply_conflict)). Returns how many
    /// dependents came back.
    pub fn revive_restored_owners(&mut self) -> Result<usize, VaultError> {
        let ctx = self.ctx()?;
        ctx.atomically(|| ctx.revive_restored())
    }
}

/// The server accepted `owner`'s tombstone: the delete stands, so its stash
/// is no longer needed.
pub(super) fn forget_cascade(store: &Store, owner: Id) -> Result<(), VaultError> {
    Ok(store.sync_delete_prefix(&owner_prefix(owner))?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stash_round_trips_and_rejects_garbage() {
        let row = RecordRow {
            id: Id::now_v7(),
            vault_id: Id::now_v7(),
            kind: Kind::ForwardRule,
            key_version: 3,
            hlc: Hlc {
                wall_ms: 42,
                counter: 7,
                node: 9,
            },
            rev: Some(12),
            deleted: false,
            envelope: vec![1, 2, 3, 4, 5],
        };
        let tomb = Hlc {
            wall_ms: 43,
            counter: 0,
            node: 9,
        };
        let bytes = encode(&row, tomb);
        let back = decode(row.id, row.vault_id, &bytes).unwrap();
        assert_eq!((back.row, back.tombstone), (row.clone(), tomb));
        let unsynced = RecordRow { rev: None, ..row };
        let back = decode(unsynced.id, unsynced.vault_id, &encode(&unsynced, tomb)).unwrap();
        assert_eq!(back.row, unsynced);
        let (id, vault) = (back.row.id, back.row.vault_id);
        for n in 0..bytes.len() - 5 {
            // Truncated before the envelope: rejected, never a panic.
            assert!(decode(id, vault, &bytes[..n]).is_none(), "{n}");
        }
        assert!(decode(id, vault, &[1, 0]).is_none());
    }
}
