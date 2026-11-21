import type { EffectiveSettings, Group, Id, Identity } from "../api/types";
import { sourceLabel } from "./effective";
import { InheritedField } from "./InheritedField";
import { OwnLoginFields, type OwnLogin } from "./ownLogin";
import { FIELDS, valueToText, type SettingsDraft, type SettingsErrors } from "./settingsDraft";
import type { SettingKey } from "./effective";

interface SettingsFieldsProps {
  draft: SettingsDraft;
  /** What each field would be without this level's own value. */
  inherited: EffectiveSettings;
  groups: Group[];
  identities: Identity[];
  owner: "host" | "group";
  /** The record being edited (`null` while new). */
  ownerId: Id | null;
  /** Its own login, shown under Identity when chosen there. */
  ownLogin: OwnLogin;
  errors: SettingsErrors;
  onChange: (key: SettingKey, next: string | null) => void;
}

/** All connection settings of a host or group, each with its inherited value and source. */
export function SettingsFields(p: SettingsFieldsProps) {
  return (
    <fieldset className="settings">
      <legend>Connection settings</legend>
      {FIELDS.filter((def) => !def.hidden).map((def) => {
        const resolved = p.inherited[def.key];
        const field = (
          <InheritedField
            key={def.key}
            def={def}
            own={p.draft[def.key]}
            inheritedText={valueToText(def.key, resolved.value) ?? ""}
            inheritedSource={sourceLabel(resolved.source, p.groups, p.owner)}
            ownSource={`set on this ${p.owner}`}
            identities={p.identities}
            ownerId={p.ownerId}
            error={p.errors[def.key]}
            onChange={(next) => {
              // A new own login starts from the username it replaces.
              if (def.key === "identityId" && next !== null && p.ownLogin.isOwn(next) && !p.ownLogin.username) {
                const replaced = p.identities.find((i) => i.id === p.inherited.identityId.value);
                if (replaced) p.ownLogin.setUsername(replaced.username);
              }
              p.onChange(def.key, next);
            }}
          />
        );
        if (def.key !== "identityId" || !p.ownLogin.isOwn(p.draft.identityId)) return field;
        return (
          <div key={def.key}>
            {field}
            <OwnLoginFields login={p.ownLogin} />
          </div>
        );
      })}
    </fieldset>
  );
}
