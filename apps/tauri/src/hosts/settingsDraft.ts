/**
 * The form side of `ConnSettings`: every field is edited as text, with
 * `null` meaning "inherit" (the backend's `None`). Converting back
 * validates and produces the typed settings.
 */

import type { ConnSettings } from "../api/types";
import type { SettingKey } from "./effective";

export type FieldKind = "int" | "identity" | "env" | "theme" | "font" | "choice";

export interface FieldDef {
  key: SettingKey;
  label: string;
  kind: FieldKind;
  min?: number;
  max?: number;
  /** A sensible value when nothing is set (the editors no longer use it). */
  fallback: string;
  help?: string;
  /** The usual values of a "choice" field; a stored value outside them stays as "(custom)". */
  choices?: readonly string[];
  /** Kept in the draft (so a save keeps the stored value) but not shown. */
  hidden?: boolean;
}

export const FIELDS: readonly FieldDef[] = [
  { key: "port", label: "Port", kind: "int", min: 1, max: 65535, fallback: "22" },
  { key: "identityId", label: "Identity", kind: "identity", fallback: "" },
  {
    key: "keepaliveSecs",
    label: "Keepalive (seconds)",
    kind: "int",
    min: 0,
    max: 86400,
    fallback: "30",
    help: "0 turns keepalives off.",
  },
  {
    key: "termType",
    label: "Terminal type",
    kind: "choice",
    choices: ["xterm-256color", "xterm", "screen-256color", "tmux-256color", "vt100"],
    fallback: "xterm-256color",
  },
  {
    key: "charset",
    label: "Charset",
    kind: "choice",
    hidden: true,
    fallback: "UTF-8",
    help: "Informational: terminals are always UTF-8.",
  },
  { key: "colorScheme", label: "Color scheme", kind: "theme", fallback: "" },
  { key: "fontFamily", label: "Font family", kind: "font", fallback: "" },
  { key: "fontSize", label: "Font size", kind: "int", min: 1, max: 200, fallback: "14" },
  {
    key: "env",
    label: "Environment variables",
    kind: "env",
    fallback: "",
    help: "One NAME=value per line. The server may refuse names its AcceptEnv does not allow.",
  },
];

export type SettingsDraft = Record<SettingKey, string | null>;

export type SettingsErrors = Partial<Record<SettingKey, string>>;

/** The text a value is edited as. */
export function valueToText(key: SettingKey, value: ConnSettings[SettingKey]): string | null {
  if (value === null || value === undefined) return null;
  if (key === "env") return (value as [string, string][]).map(([k, v]) => `${k}=${v}`).join("\n");
  return String(value);
}

export function draftFromSettings(s: ConnSettings): SettingsDraft {
  const draft = {} as SettingsDraft;
  for (const { key } of FIELDS) draft[key] = valueToText(key, s[key]);
  return draft;
}

function parseEnv(text: string): [string, string][] | string {
  const pairs: [string, string][] = [];
  for (const raw of text.split("\n")) {
    const line = raw.trim();
    if (!line) continue;
    const eq = line.indexOf("=");
    if (eq <= 0) return `"${line}" is not NAME=value`;
    pairs.push([line.slice(0, eq).trim(), line.slice(eq + 1)]);
  }
  return pairs;
}

export type DraftResult =
  | { ok: true; settings: ConnSettings }
  | { ok: false; errors: SettingsErrors };

/** Validates the draft; empty text counts as inheriting. */
export function settingsFromDraft(draft: SettingsDraft): DraftResult {
  const settings: Record<string, unknown> = {};
  const errors: SettingsErrors = {};
  for (const def of FIELDS) {
    const raw = draft[def.key];
    const text = raw === null ? "" : def.kind === "env" ? raw : raw.trim();
    if (text === "") {
      settings[def.key] = null;
      continue;
    }
    switch (def.kind) {
      case "int": {
        const n = Number(text);
        if (!/^\d+$/.test(text) || n < (def.min ?? 0) || n > (def.max ?? Number.MAX_SAFE_INTEGER)) {
          errors[def.key] = `${def.label} must be a whole number from ${def.min} to ${def.max}.`;
        } else {
          settings[def.key] = n;
        }
        break;
      }
      case "env": {
        const parsed = parseEnv(text);
        if (typeof parsed === "string") errors[def.key] = parsed;
        else settings[def.key] = parsed.length ? parsed : null;
        break;
      }
      default:
        settings[def.key] = text;
    }
  }
  return Object.keys(errors).length
    ? { ok: false, errors }
    : { ok: true, settings: settings as unknown as ConnSettings };
}
