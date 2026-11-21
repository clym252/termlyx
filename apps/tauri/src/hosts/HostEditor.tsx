import { useEffect, useId, useMemo, useRef, useState, type FormEvent, type KeyboardEvent } from "react";
import { api } from "../api";
import type { EffectiveSettings, Group, Host, Id, Identity, Tag } from "../api/types";
import { describeError } from "../ui/errors";
import { inheritedSettings, SETTING_KEYS, type SettingKey } from "./effective";
import { groupOptions, tagColor } from "./model";
import { useOwnLogin } from "./ownLogin";
import { SettingsFields } from "./SettingsFields";
import {
  draftFromSettings,
  settingsFromDraft,
  type SettingsDraft,
  type SettingsErrors,
} from "./settingsDraft";

interface HostEditorProps {
  /** The host being edited, or `null` for a new one. */
  host: Host | null;
  /** Group a new host starts in. */
  defaultGroupId: Id | null;
  groups: Group[];
  tags: Tag[];
  identities: Identity[];
  /** Called after saving (own login included), with the saved host. The caller closes the editor and reloads. */
  onSaved: (host: Host) => void;
  onCancel: () => void;
  onDelete: (host: Host) => void;
}

const NO_SETTINGS = Object.fromEntries(SETTING_KEYS.map((k) => [k, null])) as unknown as Host["settings"];

/**
 * Label, address, group, tags and connection settings of one host.
 * Remount it (`key`) to switch hosts.
 *
 * Inherited values: for a saved host whose group has not been changed, a
 * field it does not override shows what the backend reports
 * (`effectiveSettings(hostId)`); otherwise (a new host, a different group, a
 * field being un-overridden) they come from `inheritedSettings`, which
 * applies the same rules to the group chain in the form.
 */
export function HostEditor({
  host,
  defaultGroupId,
  groups,
  tags,
  identities,
  onSaved,
  onCancel,
  onDelete,
}: HostEditorProps) {
  const groupSelectId = useId();
  const [label, setLabel] = useState(host?.label ?? "");
  const [address, setAddress] = useState(host?.address ?? "");
  const [groupId, setGroupId] = useState<Id | null>(host ? host.groupId : defaultGroupId);
  const [tagIds, setTagIds] = useState<Id[]>(host?.tagIds ?? []);
  // Names typed into "New tag"; the tags are created when the host is saved.
  const [pendingTags, setPendingTags] = useState<string[]>([]);
  const [newTag, setNewTag] = useState("");
  const [draft, setDraft] = useState<SettingsDraft>(() => draftFromSettings(host?.settings ?? NO_SETTINGS));
  const [errors, setErrors] = useState<SettingsErrors>({});
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  // A new host stored by a save whose own-login step failed: the retry updates it instead of adding another.
  const storedId = useRef<Id | null>(null);
  const ownLogin = useOwnLogin(host?.id ?? null, host?.settings.identityId ?? null, identities);
  // The backend's answer for one version of the saved host (`for`), so a stale one is ignored.
  const [backend, setBackend] = useState<{ for: Host; eff: EffectiveSettings } | null>(null);

  useEffect(() => {
    if (!host) return;
    let live = true;
    api.effectiveSettings(host.id).then(
      (eff) => live && setBackend({ for: host, eff }),
      () => {}, // the local computation below stands in
    );
    return () => {
      live = false;
    };
  }, [host]);

  const inherited = useMemo<EffectiveSettings>(() => {
    const computed = inheritedSettings(groups, groupId);
    if (!host || backend?.for !== host || host.groupId !== groupId) return computed;
    const merged: Record<string, unknown> = { ...computed };
    for (const key of SETTING_KEYS) {
      if (host.settings[key] === null) merged[key] = backend.eff[key];
    }
    return merged as unknown as EffectiveSettings;
  }, [groups, groupId, host, backend]);

  const groupChoices = useMemo(() => groupOptions(groups), [groups]);

  // The chosen group was deleted while the editor was open: follow the host
  // (a delete that moved it up), else no group. Saving a gone group fails.
  useEffect(() => {
    if (groupId === null || groups.some((g) => g.id === groupId)) return;
    const hostGroup = host && groups.some((g) => g.id === host.groupId) ? host.groupId : null;
    setGroupId(hostGroup);
  }, [groups, groupId, host]);

  function addTag() {
    const name = newTag.trim();
    setNewTag("");
    if (!name) return;
    const existing = tags.find((t) => t.name.toLowerCase() === name.toLowerCase());
    if (existing) {
      setTagIds((ids) => (ids.includes(existing.id) ? ids : [...ids, existing.id]));
    } else {
      setPendingTags((names) =>
        names.some((n) => n.toLowerCase() === name.toLowerCase()) ? names : [...names, name],
      );
    }
  }

  function onTagKeyDown(e: KeyboardEvent<HTMLInputElement>) {
    if (e.key === "Enter") {
      e.preventDefault();
      addTag();
    }
  }

  async function submit(e: FormEvent) {
    e.preventDefault();
    if (!label.trim() || !address.trim()) {
      setError("A host needs a label and an address.");
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
      const created = await Promise.all(
        pendingTags.map((name) => api.upsertEntity("tag", { name, color: tagColor(name) })),
      );
      const id = host?.id ?? storedId.current;
      const stored = await api.upsertEntity("host", {
        ...(id ? { id } : {}),
        label: label.trim(),
        address: address.trim(),
        groupId,
        tagIds: [...tagIds, ...created.map((t) => t.id)],
        // An own login is linked once the host exists (below).
        settings: own ? { ...parsed.settings, identityId: ownLogin.owned?.id ?? null } : parsed.settings,
      });
      storedId.current = stored.id;
      // The new tags exist now: the host holds them, so a second save must not create them again.
      setTagIds(stored.tagIds);
      setPendingTags([]);
      if (own) {
        const identity = await ownLogin.save(stored.id);
        onSaved({ ...stored, settings: { ...stored.settings, identityId: identity.id } });
      } else {
        onSaved(stored);
      }
    } catch (err) {
      setError(describeError(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <form
      className="editor"
      aria-label="Host details"
      noValidate // number fields have min/max; the settings errors below say what is wrong
      data-busy={busy || undefined}
      onSubmit={(e) => void submit(e)}
    >
      <div className="editor-head">
        <h2>{host ? "Edit host" : "New host"}</h2>
        <button type="button" className="editor-close" aria-label="Close editor" title="Close" onClick={onCancel} disabled={busy}>
          ✕
        </button>
      </div>
      <div className="editor-body form">
        <label className="field">
          <span>Label</span>
          <input value={label} onChange={(e) => setLabel(e.target.value)} autoFocus={!host} />
        </label>
        <label className="field">
          <span>Address</span>
          <input
            value={address}
            onChange={(e) => setAddress(e.target.value)}
            spellCheck={false}
            autoCapitalize="off"
          />
        </label>
        <div className="field">
          <label htmlFor={groupSelectId}>Group</label>
          <select
            id={groupSelectId}
            value={groupId ?? ""}
            onChange={(e) => setGroupId(e.target.value || null)}
          >
            <option value="">None</option>
            {groupChoices.map((o) => (
              <option key={o.id} value={o.id}>
                {o.path}
              </option>
            ))}
          </select>
        </div>

        <fieldset className="tags">
          <legend>Tags</legend>
          <div className="tag-checks">
            {tags.map((t) => (
              <label key={t.id} className="tag-check">
                <input
                  type="checkbox"
                  checked={tagIds.includes(t.id)}
                  onChange={(e) =>
                    setTagIds((ids) => (e.target.checked ? [...ids, t.id] : ids.filter((id) => id !== t.id)))
                  }
                />
                <span>{t.name}</span>
              </label>
            ))}
            {pendingTags.map((name) => (
              <label key={name} className="tag-check">
                <input
                  type="checkbox"
                  checked
                  onChange={() => setPendingTags((names) => names.filter((n) => n !== name))}
                />
                <span>{name}</span>
              </label>
            ))}
          </div>
          <div className="row">
            <input
              className="grow"
              placeholder="New tag"
              aria-label="New tag"
              value={newTag}
              onChange={(e) => setNewTag(e.target.value)}
              onKeyDown={onTagKeyDown}
            />
            <button type="button" onClick={addTag}>
              Add tag
            </button>
          </div>
        </fieldset>

        <SettingsFields
          draft={draft}
          inherited={inherited}
          groups={groups}
          identities={identities}
          owner="host"
          ownerId={host?.id ?? null}
          ownLogin={ownLogin}
          errors={errors}
          onChange={(key: SettingKey, next) => setDraft((d) => ({ ...d, [key]: next }))}
        />
      </div>
      <div className="editor-foot">
        {error && (
          <p role="alert" className="form-error">
            {error}
          </p>
        )}
        <div className="editor-actions">
          <button type="submit" className="primary" disabled={busy}>
            Save host
          </button>
          <button type="button" onClick={onCancel} disabled={busy}>
            {host ? "Close" : "Cancel"}
          </button>
          {host && (
            <button type="button" className="danger-text" onClick={() => onDelete(host)} disabled={busy}>
              Delete host
            </button>
          )}
        </div>
      </div>
    </form>
  );
}
