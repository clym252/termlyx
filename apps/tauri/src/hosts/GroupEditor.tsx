import { useEffect, useId, useMemo, useRef, useState, type FormEvent } from "react";
import { api } from "../api";
import type { Group, Id, Identity } from "../api/types";
import { Dialog } from "../ui/Dialog";
import { describeError } from "../ui/errors";
import { inheritedSettings, type SettingKey } from "./effective";
import { groupOptions, subtreeIds } from "./model";
import { useOwnLogin } from "./ownLogin";
import { SettingsFields } from "./SettingsFields";
import {
  draftFromSettings,
  settingsFromDraft,
  type SettingsDraft,
  type SettingsErrors,
} from "./settingsDraft";

interface GroupEditorProps {
  /** The group being edited, or the parent a new group starts under. */
  target: { mode: "edit"; group: Group } | { mode: "new"; parentId: Id | null };
  groups: Group[];
  identities: Identity[];
  onClose: () => void;
  /** Called after a successful save with the saved group's id. */
  onSaved: (id: Id) => void;
}

/** Create or edit a group: name, parent, and the settings its hosts inherit. */
export function GroupEditor({ target, groups, identities, onClose, onSaved }: GroupEditorProps) {
  const parentSelectId = useId();
  // A failed own-login save leaves the group stored: a retry updates it instead of adding another.
  const storedId = useRef<Id | null>(null);
  const editing = target.mode === "edit" ? target.group : null;
  const [name, setName] = useState(editing?.name ?? "");
  const [parentId, setParentId] = useState<Id | null>(
    editing ? editing.parentId : target.mode === "new" ? target.parentId : null,
  );
  const [draft, setDraft] = useState<SettingsDraft>(() => draftFromSettings(editing?.settings ?? emptySettings()));
  const [errors, setErrors] = useState<SettingsErrors>({});
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const ownLogin = useOwnLogin(editing?.id ?? null, editing?.settings.identityId ?? null, identities);

  // A group cannot move under itself or its own descendants.
  const parents = useMemo(
    () => groupOptions(groups, editing ? subtreeIds(groups, editing.id) : new Set()),
    [groups, editing],
  );
  // What this group would inherit: defaults plus the chain above it.
  const inherited = useMemo(() => inheritedSettings(groups, parentId), [groups, parentId]);

  // The chosen parent was deleted while the editor was open: follow the group (a delete that
  // moved it up), else top level. Saving a gone parent fails.
  useEffect(() => {
    if (parentId === null || groups.some((g) => g.id === parentId)) return;
    setParentId(editing && groups.some((g) => g.id === editing.parentId) ? editing.parentId : null);
  }, [groups, parentId, editing]);

  async function submit(e: FormEvent) {
    e.preventDefault();
    if (!name.trim()) {
      setError("Enter a name for the group.");
      return;
    }
    const parsed = settingsFromDraft(draft);
    if (!parsed.ok) {
      setErrors(parsed.errors);
      setError("Some settings are not valid.");
      return;
    }
    setErrors({});
    setError(null);
    setBusy(true);
    const own = ownLogin.isOwn(draft.identityId);
    try {
      const saved = await api.upsertEntity("group", {
        ...((editing?.id ?? storedId.current) ? { id: (editing?.id ?? storedId.current)! } : {}),
        name: name.trim(),
        parentId,
        // An own login is linked once the group exists (below).
        settings: own ? { ...parsed.settings, identityId: ownLogin.owned?.id ?? null } : parsed.settings,
      });
      storedId.current = saved.id;
      if (own) await ownLogin.save(saved.id);
      onSaved(saved.id);
    } catch (err) {
      setError(describeError(err));
      setBusy(false);
    }
  }

  const title = editing ? `Edit group ${editing.name}` : "New group";

  return (
    <Dialog title={title} onClose={busy ? undefined : onClose} wide>
      <form className="form" noValidate onSubmit={(e) => void submit(e)}>
        <div className="dialog-content">
          <label className="field">
            <span>Name</span>
            <input value={name} onChange={(e) => setName(e.target.value)} autoFocus />
          </label>
          <div className="field">
            <label htmlFor={parentSelectId}>Parent group</label>
            <select
              id={parentSelectId}
              value={parentId ?? ""}
              onChange={(e) => setParentId(e.target.value || null)}
            >
              <option value="">None (top level)</option>
              {parents.map((o) => (
                <option key={o.id} value={o.id}>
                  {o.path}
                </option>
              ))}
            </select>
          </div>
          <SettingsFields
            draft={draft}
            inherited={inherited}
            groups={groups}
            identities={identities}
            owner="group"
            ownerId={editing?.id ?? null}
            ownLogin={ownLogin}
            errors={errors}
            onChange={(key: SettingKey, next) => setDraft((d) => ({ ...d, [key]: next }))}
          />
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
          <button type="submit" className="primary" disabled={busy}>
            Save group
          </button>
        </div>
      </form>
    </Dialog>
  );
}

function emptySettings() {
  return {
    port: null,
    identityId: null,
    keepaliveSecs: null,
    termType: null,
    charset: null,
    colorScheme: null,
    fontFamily: null,
    fontSize: null,
    env: null,
  };
}
