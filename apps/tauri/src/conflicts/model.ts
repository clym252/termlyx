/**
 * Pure helpers for the Conflicts view: one row per top-level field of the
 * two versions of a record, with human labels, references shown by name and
 * secrets as markers (their values only ever come from `conflicts.reveal`).
 *
 * Field names are the vault's own JSON names (snake_case), the names
 * `changedFields` and a `{fields}` resolution use.
 */

import type { Conflict, EntityKind } from "../api/types";

/** What one side of a row holds. */
export type Cell =
  /** Unset, or the whole version is a deletion. */
  | { kind: "absent" }
  /** A secret that is set; the value stays in the backend until revealed. */
  | { kind: "secret" }
  | { kind: "value"; lines: string[] };

export interface Row {
  /** The vault's field name; what Pick fields sends. */
  field: string;
  label: string;
  current: Cell;
  other: Cell;
  /** In the conflict's `changedFields`. */
  changed: boolean;
  /** Either side holds a secret to reveal. */
  secret: boolean;
}

/** A record's display name by kind and id; undefined when it is not in the vault. */
export type NameOf = (kind: EntityKind, id: string) => string | undefined;

export interface RowOptions {
  /** Selects the field labels and order; unknown kinds show raw names. */
  kind?: EntityKind;
  /** Resolves referenced records (group_id, key_id, ...). */
  name?: NameOf;
}

const MISSING = "(missing)";
const UNSET = "—";

/** Labels in display order, per kind. Fields not listed follow, by raw name. */
const FIELDS: Record<EntityKind, readonly (readonly [string, string])[]> = {
  snippet: [
    ["label", "Label"],
    ["text", "Command"],
    ["run", "Run after sending"],
  ],
  host: [
    ["label", "Label"],
    ["address", "Address"],
    ["group_id", "Group"],
    ["tag_ids", "Tags"],
    ["settings", "Settings"],
    ["forward_rule_ids", "Forwarding rules"],
  ],
  group: [
    ["name", "Name"],
    ["parent_id", "Parent group"],
    ["settings", "Settings"],
  ],
  tag: [
    ["name", "Name"],
    ["color", "Color"],
  ],
  identity: [
    ["label", "Label"],
    ["username", "Username"],
    ["password", "Password"],
    ["key_id", "Key"],
    ["owner", "Owner"],
  ],
  ssh_key: [
    ["label", "Label"],
    ["algorithm", "Algorithm"],
    ["private_openssh", "Private key"],
    ["passphrase", "Passphrase"],
    ["public_openssh", "Public key"],
    ["certificate", "Certificate"],
  ],
  known_host: [
    ["host_pattern", "Host"],
    ["port", "Port"],
    ["algorithm", "Algorithm"],
    ["public_key", "Public key"],
    ["added_at", "Added"],
    ["source", "Source"],
  ],
  forward_rule: [
    ["host_id", "Host"],
    ["label", "Label"],
    ["kind", "Type"],
    ["bind_addr", "Bind address"],
    ["bind_port", "Bind port"],
    ["dest_host", "Destination host"],
    ["dest_port", "Destination port"],
    ["autostart", "Start automatically"],
  ],
};

/** The kinds a reference field points at (`owner` is a host or a group). */
const REFERENCES: Record<string, readonly EntityKind[]> = {
  group_id: ["group"],
  parent_id: ["group"],
  identity_id: ["identity"],
  key_id: ["ssh_key"],
  tag_ids: ["tag"],
  forward_rule_ids: ["forward_rule"],
  host_id: ["host"],
  owner: ["host", "group"],
};

/** The connection settings a row `settings` is made of, in display order. */
const SETTINGS: readonly (readonly [string, string])[] = [
  ["port", "Port"],
  ["identity_id", "Login"],
  ["keepalive_secs", "Keepalive"],
  ["term_type", "Terminal type"],
  ["charset", "Character set"],
  ["color_scheme", "Color scheme"],
  ["font_family", "Font"],
  ["font_size", "Font size"],
  ["env", "Environment"],
];

const KIND_LABELS: Record<EntityKind, string> = {
  host: "Host",
  group: "Group",
  tag: "Tag",
  identity: "Identity",
  ssh_key: "SSH key",
  known_host: "Known host",
  forward_rule: "Forward rule",
  snippet: "Snippet",
};

type Json = Record<string, unknown>;

const isObject = (v: unknown): v is Json => typeof v === "object" && v !== null && !Array.isArray(v);
const isUnset = (v: unknown) => v === null || v === undefined;
const isSecretMarker = (v: unknown) => isObject(v) && v.hasSecret === true;
/** Compares values with unset (null/undefined) equal, like the backend does for options. */
const sameValue = (a: unknown, b: unknown) => JSON.stringify(a ?? null) === JSON.stringify(b ?? null);

function refName(field: string, id: string, name: NameOf | undefined): string {
  for (const kind of REFERENCES[field] ?? []) {
    const found = name?.(kind, id);
    if (found !== undefined) return found;
  }
  return MISSING;
}

/** One value as text; `field` selects reference lookups and special formats. */
function formatValue(field: string, value: unknown, name: NameOf | undefined): string {
  if (isUnset(value)) return UNSET;
  if (field in REFERENCES) {
    if (typeof value === "string") return refName(field, value, name);
    if (Array.isArray(value)) {
      return value.length === 0 ? UNSET : value.map((id) => refName(field, String(id), name)).join(", ");
    }
  }
  if (field === "env" && Array.isArray(value)) {
    const pairs = value.map((p) => (Array.isArray(p) ? `${String(p[0])}=${String(p[1])}` : String(p)));
    return pairs.length === 0 ? UNSET : pairs.join(", ");
  }
  if (field === "keepalive_secs" && typeof value === "number") return `${value} s`;
  if (field === "added_at" && typeof value === "number") {
    return value > 0 ? new Date(value * 1000).toLocaleString() : UNSET;
  }
  if (typeof value === "boolean") return value ? "Yes" : "No";
  if (typeof value === "string") return value === "" ? "(empty)" : value;
  if (typeof value === "number") return String(value);
  if (Array.isArray(value) && value.every((v) => typeof v === "string")) {
    return value.length === 0 ? UNSET : value.join(", ");
  }
  return JSON.stringify(value);
}

/** `settings`: the sub-fields that differ (all that are set when none do), labelled. */
function settingsCells(
  current: unknown,
  other: unknown,
  name: NameOf | undefined,
): [Cell, Cell] {
  const a = isObject(current) ? current : null;
  const b = isObject(other) ? other : null;
  const keys = [...new Set([...Object.keys(a ?? {}), ...Object.keys(b ?? {})])];
  const ordered = [
    ...SETTINGS.map(([k]) => k).filter((k) => keys.includes(k)),
    ...keys.filter((k) => !SETTINGS.some(([s]) => s === k)).sort(),
  ];
  const differing = ordered.filter((k) => !sameValue(a?.[k], b?.[k]));
  const pick = differing.length > 0 ? differing : ordered;
  const labelOf = (k: string) => SETTINGS.find(([s]) => s === k)?.[1] ?? k;
  const side = (s: Json | null): Cell => {
    if (!s) return { kind: "absent" };
    // Unchanged settings list only what is set; changed ones line up both sides.
    const lines = pick
      .filter((k) => differing.length > 0 || !isUnset(s[k]))
      .map((k) => `${labelOf(k)}: ${formatValue(k, s[k], name)}`);
    return lines.length === 0 ? { kind: "absent" } : { kind: "value", lines };
  };
  return [side(a), side(b)];
}

function cell(field: string, value: unknown, name: NameOf | undefined): Cell {
  if (isSecretMarker(value)) return { kind: "secret" };
  if (isUnset(value)) return { kind: "absent" };
  const text = formatValue(field, value, name);
  return text === UNSET ? { kind: "absent" } : { kind: "value", lines: [text] };
}

/**
 * One row per top-level field of either version (the id is not a field),
 * known fields first in their display order, then the rest by name. A `null`
 * version (a deletion) has only absent cells. `changedFields` comes from the
 * backend; secrets are compared there, by value.
 */
export function fieldRows(
  current: Json | null,
  other: Json | null,
  changedFields: readonly string[],
  options: RowOptions = {},
): Row[] {
  const known = options.kind ? (FIELDS[options.kind] ?? []) : [];
  const present = new Set([...Object.keys(current ?? {}), ...Object.keys(other ?? {})]);
  present.delete("id");
  const knownNames = known.map(([f]) => f);
  const ordered = [
    ...knownNames.filter((f) => present.has(f)),
    ...[...present].filter((f) => !knownNames.includes(f)),
  ];
  const changed = new Set(changedFields);
  return ordered.map((field) => {
    const label = known.find(([f]) => f === field)?.[1] ?? field;
    const [a, b] =
      field === "settings"
        ? settingsCells(current?.[field], other?.[field], options.name)
        : [cell(field, current?.[field], options.name), cell(field, other?.[field], options.name)];
    return {
      field,
      label,
      current: a,
      other: b,
      changed: changed.has(field),
      secret: a.kind === "secret" || b.kind === "secret",
    };
  });
}

/** "Host · prod-db"; a hidden own login reads "Login of prod-db". */
export function conflictTitle(conflict: Conflict, name: NameOf): string {
  const record = conflict.current ?? conflict.other;
  const kind = KIND_LABELS[conflict.kind] ?? conflict.kind;
  if (conflict.kind === "identity" && typeof record?.owner === "string") {
    return `Login of ${refName("owner", record.owner, name)}`;
  }
  const label = recordLabel(conflict.kind, record);
  return label === undefined ? kind : `${kind} · ${label}`;
}

function recordLabel(kind: EntityKind, record: Json | null): string | undefined {
  if (!record) return undefined;
  const str = (k: string) => (typeof record[k] === "string" && record[k] !== "" ? (record[k] as string) : undefined);
  switch (kind) {
    case "group":
    case "tag":
      return str("name");
    case "known_host": {
      const host = str("host_pattern");
      return host !== undefined && typeof record.port === "number" && record.port !== 22
        ? `${host}:${record.port}`
        : host;
    }
    default:
      return str("label");
  }
}

/** "1 conflict" / "3 conflicts". */
export function conflictCountLabel(n: number): string {
  return n === 1 ? "1 conflict" : `${n} conflicts`;
}

/**
 * The secrets a resolution would lose: the changed secret fields (lowercase
 * labels: "password", "private key", ...) of the version that is discarded
 * (`side`), less the `taken` fields a pick copies over. An unchanged secret is
 * the same on both sides, so nothing is lost with it.
 */
export function discardedSecrets(conflict: Conflict, side: "current" | "other", taken: ReadonlySet<string> = new Set()): string[] {
  return fieldRows(conflict.current, conflict.other, conflict.changedFields, { kind: conflict.kind })
    .filter((r) => r.changed && r[side].kind === "secret" && !taken.has(r.field))
    .map((r) => r.label.toLowerCase());
}

/**
 * The current version's secrets a pick overwrites: the changed secret fields
 * taken from the other version (`taken`) where the current version holds a
 * secret too. Once the conflict is settled those are gone.
 */
export function replacedSecrets(conflict: Conflict, taken: ReadonlySet<string>): string[] {
  return fieldRows(conflict.current, conflict.other, conflict.changedFields, { kind: conflict.kind })
    .filter((r) => r.changed && taken.has(r.field) && r.current.kind === "secret")
    .map((r) => r.label.toLowerCase());
}

/** "password", "password and passphrase", "a, b and c". */
export function joinLabels(labels: readonly string[]): string {
  return labels.length <= 1 ? (labels[0] ?? "") : `${labels.slice(0, -1).join(", ")} and ${labels[labels.length - 1]}`;
}
