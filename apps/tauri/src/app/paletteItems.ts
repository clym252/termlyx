import type { Group, Host, SessionTarget, Tag } from "../api/types";
import { matchesQuery } from "../hosts/filter";
import { groupPath, sortHosts, tagNames } from "../hosts/model";
import {
  PRESETS,
  activeTab,
  effectiveZoom,
  paneLabel,
  panesOf,
  tabTitle,
  type Orientation,
  type Preset,
  type WorkspaceState,
} from "../workspace/model";
import type { ShortcutLabels } from "../workspace/shortcuts";
import type { RouteId } from "./routes";

export type ActionId =
  | "newTab"
  | "localTerminal"
  | "splitRight"
  | "splitDown"
  | "splitRightPick"
  | "splitDownPick"
  | `preset:${Preset}`
  | "zoom"
  | "broadcast"
  | "closePane"
  | `go:${RouteId}`
  | "newHost"
  | "hideSidebar"
  | "showSidebar"
  | "panel:snippets"
  | "panel:themes"
  | "lock";

/**
 * An open tab (`paneId` null) or one pane of a split tab. `target` and `title` are what its pane opened (a tab's:
 * its focused pane's), for a split or details on the same host; null `target` for a launcher with nothing open yet.
 */
export interface TerminalItem {
  kind: "terminal";
  key: string;
  label: string;
  detail: string;
  tabId: string;
  paneId: string | null;
  target: SessionTarget | null;
  title: string;
}

export type PaletteItem =
  | TerminalItem
  | { kind: "host"; key: string; host: Host; detail: string }
  | { kind: "action"; key: string; id: ActionId; label: string; hint?: string };

export interface PaletteGroup {
  title: "Open terminals" | "Hosts" | "Actions";
  items: PaletteItem[];
}

export interface PaletteSource {
  workspace: WorkspaceState;
  hosts: Host[];
  tags: Tag[];
  groups: Group[];
  /** A phone or tablet (iOS, Android): no local terminal. */
  touch: boolean;
  /** The phone layout: one pane at a time, so no split, layout, zoom or broadcast actions, and no sidebar to toggle. */
  phone: boolean;
  /** The sidebar is hidden (desktop, tablet): the palette offers to show it instead of hide it. */
  sidebarHidden: boolean;
  labels: ShortcutLabels;
  /** The pages Go to lists: the sidebar's, in its order. */
  routes: { id: RouteId; label: string }[];
}

/** "pickHost": choosing the host a new pane opens (from "Split right…"); `orientation` is the split's. */
export type PaletteMode = { kind: "all" } | { kind: "pickHost"; orientation: Orientation };

const MAX_HOSTS = 50;

/** Every word of the query appears in the text, ignoring case; an empty query matches all. */
function matchesText(text: string, query: string): boolean {
  const lower = text.toLowerCase();
  return query
    .toLowerCase()
    .split(/\s+/)
    .filter(Boolean)
    .every((w) => lower.includes(w));
}

function terminalItems(ws: WorkspaceState, query: string): PaletteItem[] {
  const items: PaletteItem[] = [];
  for (const tab of ws.tabs) {
    const title = tabTitle(tab);
    const panes = panesOf(tab.root);
    const split = tab.root?.kind === "split";
    if (matchesText(title, query)) {
      const focused = panes.find((p) => p.id === tab.activePane);
      items.push({
        kind: "terminal",
        key: `tab:${tab.id}`,
        label: title,
        detail: split ? `Tab, ${panes.length} panes` : "Tab",
        tabId: tab.id,
        paneId: null,
        target: focused?.target ?? null,
        title: focused?.title ?? title,
      });
    }
    if (!split) continue;
    for (const pane of panes) {
      const label = paneLabel(pane);
      if (!matchesText(label, query)) continue;
      items.push({
        kind: "terminal",
        key: `pane:${pane.id}`,
        label,
        detail: `Pane in "${title}"`,
        tabId: tab.id,
        paneId: pane.id,
        target: pane.target,
        title: pane.title,
      });
    }
  }
  return items;
}

function hostItems(src: PaletteSource, query: string): PaletteItem[] {
  return sortHosts(src.hosts.filter((h) => matchesQuery(h, src.tags, query)))
    .slice(0, MAX_HOSTS)
    .map((host) => ({
      kind: "host",
      key: `host:${host.id}`,
      host,
      detail: [groupPath(src.groups, host.groupId), ...tagNames(host, src.tags)].filter(Boolean).join(" · "),
    }));
}

function actions(src: PaletteSource): Extract<PaletteItem, { kind: "action" }>[] {
  const { labels, phone } = src;
  const tab = activeTab(src.workspace);
  const hasRoot = !!tab?.root;
  const paneCount = panesOf(tab?.root ?? null).length;
  const list: { id: ActionId; label: string; hint?: string; when?: boolean }[] = [
    { id: "newTab", label: "New tab", hint: labels.newTab },
    { id: "localTerminal", label: "Local terminal", hint: labels.localTerminal, when: !src.touch },
    { id: "splitRight", label: "Split right", hint: labels.splitRight, when: !phone && hasRoot },
    { id: "splitDown", label: "Split down", hint: labels.splitDown, when: !phone && hasRoot },
    { id: "splitRightPick", label: "Split right…", when: !phone && hasRoot },
    { id: "splitDownPick", label: "Split down…", when: !phone && hasRoot },
    // A launcher tab (no panes yet) takes a preset too: it fills with launcher panes.
    ...PRESETS.map((p) => ({
      id: `preset:${p.id}` as const,
      label: `Layout: ${p.label}`,
      when: !phone && !!tab,
    })),
    {
      id: "zoom",
      label: tab && effectiveZoom(tab, false) !== null ? "Restore layout" : "Maximize pane",
      hint: labels.zoom,
      when: !phone && paneCount > 1,
    },
    {
      id: "broadcast",
      label: tab?.broadcast ? "Broadcast input: on" : "Broadcast input: off",
      hint: labels.broadcast,
      when: !phone && hasRoot,
    },
    { id: "closePane", label: "Close pane", hint: labels.close, when: hasRoot },
    ...src.routes.map((r) => ({ id: `go:${r.id}` as const, label: `Go to ${r.label}` })),
    { id: "panel:snippets", label: "Snippets" },
    { id: "panel:themes", label: "Themes" },
    { id: "newHost", label: "New host" },
    { id: "hideSidebar", label: "Hide sidebar", hint: labels.sidebar, when: !phone && !src.sidebarHidden },
    { id: "showSidebar", label: "Show sidebar", hint: labels.sidebar, when: !phone && src.sidebarHidden },
    { id: "lock", label: "Lock vault" },
  ];
  return list
    .filter((a) => a.when !== false)
    .map((a) => ({ kind: "action", key: `action:${a.id}`, id: a.id, label: a.label, hint: a.hint }));
}

/**
 * What the palette lists for a query, in order: open terminals, hosts, actions.
 * Empty groups are dropped. Picking a host (`pickHost`) lists only hosts and,
 * on desktop, the local terminal.
 */
export function paletteGroups(query: string, src: PaletteSource, mode: PaletteMode): PaletteGroup[] {
  const groups: PaletteGroup[] = [];
  const add = (title: PaletteGroup["title"], items: PaletteItem[]) => {
    if (items.length > 0) groups.push({ title, items });
  };
  if (mode.kind === "pickHost") {
    add("Hosts", hostItems(src, query));
    add(
      "Actions",
      actions(src)
        .filter((a) => a.id === "localTerminal" && matchesText(a.label, query))
        .map(({ hint: _hint, ...a }) => a),
    );
    return groups;
  }
  add("Open terminals", terminalItems(src.workspace, query));
  add("Hosts", hostItems(src, query));
  add(
    "Actions",
    actions(src).filter((a) => matchesText(a.label, query)),
  );
  return groups;
}
