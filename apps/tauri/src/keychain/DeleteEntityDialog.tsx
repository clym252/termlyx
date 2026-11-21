import { useState } from "react";
import { api } from "../api";
import { isApiError } from "../api/types";
import type { Id } from "../api/types";
import { ConfirmDialog, Dialog } from "../ui/Dialog";
import { describeError } from "../ui/errors";
import { describeRefs, type RefSources } from "./refs";

interface DeleteEntityDialogProps {
  kind: "ssh_key" | "identity";
  id: Id;
  label: string;
  /** Used to name the users if they cannot be read afresh. */
  fallback: RefSources;
  onClose: () => void;
  onDeleted: () => void;
}

/** The records that can reference a key or identity as they are now (the page's lists may be stale). */
async function currentSources(fallback: RefSources): Promise<RefSources> {
  try {
    const [hosts, groups, identities] = await Promise.all([
      api.listEntities("host"),
      api.listEntities("group"),
      api.listEntities("identity"),
    ]);
    return { hosts, groups, identities };
  } catch {
    return fallback;
  }
}

/**
 * Deletes a key or an identity after a confirmation. A record that other
 * records still use is not deleted: the backend refuses (`referenced`) and
 * the dialog lists the users, which must be changed first.
 */
export function DeleteEntityDialog({ kind, id, label, fallback, onClose, onDeleted }: DeleteEntityDialogProps) {
  const noun = kind === "ssh_key" ? "key" : "identity";
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [users, setUsers] = useState<string[] | null>(null);

  async function confirm() {
    setBusy(true);
    setError(null);
    try {
      await api.deleteEntity(kind, id);
      onDeleted();
    } catch (e) {
      if (isApiError(e) && e.code === "referenced") {
        setUsers(describeRefs(e.refs, await currentSources(fallback)));
      } else {
        setError(describeError(e));
      }
      setBusy(false);
    }
  }

  if (users) {
    return (
      <Dialog title={`Cannot delete ${label}`} onClose={onClose}>
        <div className="dialog-content">
          <p>
            The {noun} <strong>{label}</strong> is still in use. Change these first, then delete it:
          </p>
          <ul aria-label="Still used by" className="ref-list">
            {users.map((u, i) => (
              <li key={`${i}-${u}`}>{u}</li>
            ))}
          </ul>
        </div>
        <div className="dialog-actions">
          <button type="button" className="primary" onClick={onClose}>
            Close
          </button>
        </div>
      </Dialog>
    );
  }

  return (
    <ConfirmDialog
      title={`Delete ${noun} ${label}`}
      confirmLabel="Delete"
      danger
      busy={busy}
      error={error}
      onConfirm={() => void confirm()}
      onCancel={onClose}
    >
      <p>
        Delete the {noun} <strong>{label}</strong>? This cannot be undone.
      </p>
    </ConfirmDialog>
  );
}
