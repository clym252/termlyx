import { useId } from "react";
import type { Id, Identity } from "../api/types";
import { useTerminalSettings } from "../settings/store";
import { findTheme, THEMES } from "../themes";
import { familyOf } from "../ui/fonts";
import { FontSelect } from "../ui/FontSelect";
import { identityOptionLabel, OWN_LOGIN } from "./ownLogin";
import type { FieldDef } from "./settingsDraft";

/** A number field's draft while the browser holds text that is not a number: fails validation on save. */
const NOT_A_NUMBER = "not a number";

interface InheritedFieldProps {
  def: FieldDef;
  /** The field's own value as text, or `null` when it inherits. */
  own: string | null;
  /** The inherited value as text (empty when nothing is set). */
  inheritedText: string;
  /** Where the inherited value comes from: "default" or "from group X". */
  inheritedSource: string;
  /** What the source is when the field has its own value: "set on this host". */
  ownSource: string;
  identities: Identity[];
  /** The record being edited (`null` while new), whose own login the select offers. */
  ownerId?: Id | null;
  error?: string;
  onChange: (next: string | null) => void;
}

/**
 * One connection setting, always editable. A select's first option ("Same as
 * group Prod (Dracula)") and an empty number or text field mean inheriting.
 */
export function InheritedField(p: InheritedFieldProps) {
  const { def, own } = p;
  const id = useId();
  const sourceId = `${id}-source`;
  const errorId = `${id}-error`;
  // Keychain identities, plus this record's own login and whichever own login is linked.
  const owned = p.identities.find((i) => i.owner !== null && i.owner === p.ownerId);
  const describedBy = [sourceId, p.error ? errorId : null].filter(Boolean).join(" ");
  const empty = (v: string) => p.onChange(v === "" ? null : v);
  const app = useTerminalSettings();

  const inherited = p.inheritedText;
  const inheritedLogin = def.kind === "identity" ? p.identities.find((i) => i.id === inherited) : undefined;
  const shown = inheritedLogin
    ? identityOptionLabel(inheritedLogin)
    : def.kind === "theme"
      ? (findTheme(inherited)?.name ?? inherited)
      : def.kind === "font" && inherited
        ? familyOf(inherited)
        : inherited;
  // Theme and font left unset everywhere follow the app's terminal settings.
  const fromApp = (def.kind === "theme" || def.kind === "font") && p.inheritedSource === "default" && !inherited;
  const sameAs = fromApp
    ? `Same as app (${def.kind === "theme" ? findTheme(app.themeId)?.name : familyOf(app.fontFamily)})`
    : `Same as ${p.inheritedSource.replace(/^from /, "")} (${shown || "none"})`;

  const common = {
    id,
    "aria-describedby": describedBy,
    "aria-invalid": p.error ? true : undefined,
  };

  let control;
  switch (def.kind) {
    case "identity": {
      // Another record's own login is inherited, never linked: picking own login starts this record's own.
      const choices = p.identities.filter((i) => i.owner === null || (i.id === own && i !== owned));
      control = (
        <select {...common} value={own ?? ""} onChange={(e) => empty(e.target.value)}>
          <option value="">{sameAs}</option>
          <option value={owned?.id ?? OWN_LOGIN}>Own username &amp; password</option>
          {choices.map((i) => (
            <option key={i.id} value={i.id}>
              {identityOptionLabel(i)}
            </option>
          ))}
        </select>
      );
      break;
    }
    case "theme": {
      // A theme stored by name (older settings) stays selected under its name and is saved unchanged.
      const stored = own === null ? undefined : findTheme(own);
      control = (
        <select {...common} value={own ?? ""} onChange={(e) => empty(e.target.value)}>
          <option value="">{sameAs}</option>
          {own !== null && !stored && <option value={own}>{own} (custom)</option>}
          {THEMES.map((t) => (
            <option key={t.id} value={t === stored ? own! : t.id}>
              {t.name}
            </option>
          ))}
        </select>
      );
      break;
    }
    case "font":
      control = <FontSelect {...common} value={own ?? ""} onChange={empty} inherit={{ label: sameAs }} />;
      break;
    case "choice": {
      const choices = def.choices ?? [];
      control = (
        <select {...common} value={own ?? ""} onChange={(e) => empty(e.target.value)}>
          <option value="">{sameAs}</option>
          {own !== null && !choices.includes(own) && <option value={own}>{own} (custom)</option>}
          {choices.map((c) => (
            <option key={c} value={c}>
              {c}
            </option>
          ))}
        </select>
      );
      break;
    }
    case "env":
      control = (
        <textarea
          {...common}
          rows={3}
          spellCheck={false}
          value={own ?? ""}
          placeholder={inherited || "NAME=value"}
          onChange={(e) => empty(e.target.value)}
        />
      );
      break;
    case "int":
      control = (
        <input
          {...common}
          type="number"
          min={def.min}
          max={def.max}
          step={1}
          onWheel={(e) => e.currentTarget.blur()}
          value={own === NOT_A_NUMBER ? "" : (own ?? "")}
          placeholder={inherited}
          // Typed text that is not a number reads as "": keep it an error, not a silent inherit.
          onChange={(e) => (e.target.validity.badInput ? p.onChange(NOT_A_NUMBER) : empty(e.target.value))}
        />
      );
      break;
  }

  return (
    <div className="inherited-field">
      <label htmlFor={id}>{def.label}</label>
      {control}
      <p id={sourceId} className="source muted small">
        {own !== null ? p.ownSource : p.inheritedSource}
      </p>
      {def.help && <p className="muted small">{def.help}</p>}
      {p.error && (
        <p id={errorId} role="alert" className="form-error small">
          {p.error}
        </p>
      )}
    </div>
  );
}
