import { useState } from "react";
import { api } from "../api";
import type { ChildPolicy, Group, Host } from "../api/types";
import { Dialog } from "../ui/Dialog";
import { describeError } from "../ui/errors";
import { groupContents } from "./model";

const plural = (n: number, one: string) => `${n} ${one}${n === 1 ? "" : "s"}`;

interface DeleteGroupDialogProps {
  group: Group;
  groups: Group[];
  hosts: Host[];
  onClose: () => void;
  onDeleted: () => void;
}

/**
 * A group with children is never deleted without a choice: move its
 * contents to the parent, or delete everything below it.
 */
export function DeleteGroupDialog({ group, groups, hosts, onClose, onDeleted }: DeleteGroupDialogProps) {
  const contents = groupContents(groups, hosts, group.id);
  const hasChildren = contents.subgroups > 0 || contents.hosts > 0;
  const [policy, setPolicy] = useState<ChildPolicy>("moveToParent");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const parts = [
    contents.subgroups > 0 ? plural(contents.subgroups, "subgroup") : null,
    contents.hosts > 0 ? plural(contents.hosts, "host") : null,
  ].filter(Boolean);

  async function confirm() {
    setBusy(true);
    setError(null);
    try {
      await api.deleteGroup(group.id, policy);
      onDeleted();
    } catch (e) {
      setError(describeError(e));
      setBusy(false);
    }
  }

  return (
    <Dialog title={`Delete group ${group.name}`} onClose={busy ? undefined : onClose}>
      <div className="dialog-content">
        {hasChildren ? (
          <>
            <p>
              This group contains {parts.join(" and ")}. What should happen to{" "}
              {parts.length === 1 && contents.subgroups + contents.hosts === 1 ? "it" : "them"}?
            </p>
            <fieldset className="choices">
              <legend className="visually-hidden">What happens to the contents</legend>
              <label className="choice">
                <input
                  type="radio"
                  name="policy"
                  checked={policy === "moveToParent"}
                  onChange={() => setPolicy("moveToParent")}
                />
                <span>Move contents to the parent group</span>
              </label>
              <label className="choice">
                <input
                  type="radio"
                  name="policy"
                  checked={policy === "deleteRecursive"}
                  onChange={() => setPolicy("deleteRecursive")}
                />
                <span>Delete the group and everything in it</span>
              </label>
            </fieldset>
            {policy === "deleteRecursive" && (
              <p className="form-error">
                This permanently deletes {parts.join(" and ")} along with the group.
              </p>
            )}
          </>
        ) : (
          <p>This group is empty. Deleting it cannot affect any host.</p>
        )}
      </div>
      {error && (
        <p role="alert" className="form-error">
          {error}
        </p>
      )}
      <div className="dialog-actions">
        <button type="button" onClick={onClose} disabled={busy}>
          Cancel
        </button>
        <button type="button" className="danger" onClick={() => void confirm()} disabled={busy}>
          Delete
        </button>
      </div>
    </Dialog>
  );
}
