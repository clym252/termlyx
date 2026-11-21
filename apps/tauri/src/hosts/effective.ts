/**
 * Inherited connection settings, computed in the UI: app defaults, then the
 * group chain root to leaf. This is the backend's `effective_settings`
 * without the host layer, used for what the editors show while a draft is
 * being edited (a new host, a changed group, an override toggled off). It is
 * checked against the backend's answer in `effective.test.ts`; saved hosts
 * that are unchanged show the backend's `effectiveSettings(hostId)` itself.
 */

import type { ConnSettings, EffectiveSettings, Group, Id, Resolved, Source } from "../api/types";

/** Mirrors `core_api::vault::app_defaults`. */
export const APP_DEFAULTS: ConnSettings = {
  port: 22,
  identityId: null,
  keepaliveSecs: 30,
  termType: "xterm-256color",
  charset: "UTF-8",
  colorScheme: null,
  fontFamily: null,
  fontSize: null,
  env: null,
};

export const SETTING_KEYS = [
  "port",
  "identityId",
  "keepaliveSecs",
  "termType",
  "charset",
  "colorScheme",
  "fontFamily",
  "fontSize",
  "env",
] as const satisfies readonly (keyof ConnSettings)[];

export type SettingKey = (typeof SETTING_KEYS)[number];

/** Group ids from the root down to `leaf`; stops at a missing parent or a cycle. */
export function groupChain(groups: Group[], leaf: Id | null): Group[] {
  const byId = new Map(groups.map((g) => [g.id, g]));
  const chain: Group[] = [];
  const seen = new Set<Id>();
  for (let cur = leaf; cur !== null && !seen.has(cur); ) {
    seen.add(cur);
    const g = byId.get(cur);
    if (!g) break;
    chain.unshift(g);
    cur = g.parentId;
  }
  return chain;
}

/** App defaults overlaid by the chain ending at `groupId`; every value says where it came from. */
export function inheritedSettings(groups: Group[], groupId: Id | null): EffectiveSettings {
  const layers: [Source, ConnSettings][] = [
    [{ kind: "default" }, APP_DEFAULTS],
    ...groupChain(groups, groupId).map((g): [Source, ConnSettings] => [
      { kind: "group", groupId: g.id },
      g.settings,
    ]),
  ];
  const pick = <K extends SettingKey>(key: K): Resolved<ConnSettings[K]> => {
    for (let i = layers.length - 1; i >= 0; i--) {
      const value = layers[i][1][key];
      if (value !== null && value !== undefined) return { value, source: layers[i][0] };
    }
    return { value: null as ConnSettings[K], source: { kind: "default" } };
  };
  return {
    port: { ...pick("port"), value: pick("port").value ?? 22 },
    identityId: pick("identityId"),
    keepaliveSecs: pick("keepaliveSecs"),
    termType: pick("termType"),
    charset: pick("charset"),
    colorScheme: pick("colorScheme"),
    fontFamily: pick("fontFamily"),
    fontSize: pick("fontSize"),
    env: pick("env"),
  };
}

/** "default", "from group Production" or "set on this host". */
export function sourceLabel(source: Source, groups: Group[], owner: "host" | "group"): string {
  switch (source.kind) {
    case "default":
      return "default";
    case "group": {
      const name = groups.find((g) => g.id === source.groupId)?.name;
      return name === undefined ? "from a parent group" : `from group ${name}`;
    }
    case "host":
      return `set on this ${owner}`;
  }
}
