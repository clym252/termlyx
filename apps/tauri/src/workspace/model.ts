/**
 * The terminal workspace as data: tabs, each a tree of split panes (or a
 * launcher when it has none yet). Pure; ids come in with the actions so
 * the reducer is deterministic (React may run it twice).
 */

import type { SessionTarget } from "../api/types";

export interface PaneSpec {
  id: string;
  /** `null`: a launcher pane, waiting for the user to pick what it opens. */
  target: SessionTarget | null;
  title: string;
  /**
   * Tells same-title panes apart: 1 for the first, then the lowest number
   * no open pane with that title uses. Fixed for the pane's life.
   */
  ordinal: number;
}

/** A pane that opens something (what `TerminalPane` renders). */
export type TargetedPane = PaneSpec & { target: SessionTarget };

/** The title of a launcher pane (one with no target yet). */
export const NEW_PANE_TITLE = "New pane";

/** A layout a tab can be rearranged into. */
export type Preset = "columns2" | "rows2" | "columns3" | "grid4";

export const PRESETS: { id: Preset; label: string }[] = [
  { id: "columns2", label: "2 columns" },
  { id: "rows2", label: "2 rows" },
  { id: "columns3", label: "3 columns" },
  { id: "grid4", label: "2×2 grid" },
];

/** "horizontal": side by side (split right); "vertical": stacked (split down). */
export type Orientation = "horizontal" | "vertical";

export type LayoutNode =
  | { kind: "pane"; pane: PaneSpec }
  | { kind: "split"; id: string; orientation: Orientation; children: LayoutNode[] };

export interface Tab {
  id: string;
  /** `null`: a launcher (new tab) that has not opened anything yet. */
  root: LayoutNode | null;
  activePane: string | null;
  /** A name the user gave the tab; `null` follows the focused pane. */
  customTitle: string | null;
  /** The pane filling the tab, the others hidden (not closed); `null`: none. */
  zoomed: string | null;
  /** Typing in the active pane also goes to every other open pane of the tab. Never persisted. */
  broadcast: boolean;
}

export interface WorkspaceState {
  tabs: Tab[];
  activeTab: string | null;
}

export type WorkspaceAction =
  | { type: "openTab"; tabId: string; paneId: string; target: SessionTarget; title: string }
  | { type: "newTab"; tabId: string }
  | { type: "launch"; tabId: string; paneId: string; target: SessionTarget; title: string }
  /**
   * Splits the active pane of the active tab. The new pane opens `target`
   * (`undefined`: the active pane's target and title; `null`: a launcher pane).
   */
  | { type: "split"; orientation: Orientation; paneId: string; target?: SessionTarget | null; title?: string }
  /** Fills a launcher pane in place (a no-op on a pane that already has a target). */
  | { type: "launchPane"; paneId: string; target: SessionTarget; title: string }
  /** Zooms a pane (default: the active pane of the active tab) or clears the zoom; a no-op in a one-pane tab. */
  | { type: "toggleZoom"; paneId?: string }
  | { type: "toggleBroadcast"; tabId: string }
  /** The two panes trade places (same tab only); focus stays on its pane. */
  | { type: "swapPanes"; a: string; b: string }
  /** Takes a pane out of its tab into a new tab `tabId` right after it; a no-op for a lone pane. */
  | { type: "movePaneToTab"; paneId: string; tabId: string }
  /**
   * Rearranges the active tab: its panes fill the preset's slots in order,
   * empty slots get launcher panes (ids from `paneIds`), extra panes pile
   * into the last slot.
   */
  | { type: "applyPreset"; preset: Preset; paneIds: string[] }
  /** Closes a pane (default: the active pane of the active tab; a launcher tab closes). */
  | { type: "closePane"; paneId?: string }
  | { type: "closeTab"; tabId: string }
  | { type: "selectTab"; tabId: string }
  /** Names a tab; a blank title goes back to the automatic one. */
  | { type: "renameTab"; tabId: string; title: string }
  /** Moves a tab so it sits at `toIndex` of the resulting order (clamped); the active tab stays. */
  | { type: "moveTab"; tabId: string; toIndex: number }
  | { type: "focusPane"; paneId: string };

export const EMPTY_WORKSPACE: WorkspaceState = { tabs: [], activeTab: null };

export function panesOf(node: LayoutNode | null): PaneSpec[] {
  if (!node) return [];
  return node.kind === "pane" ? [node.pane] : node.children.flatMap(panesOf);
}

export function isLauncherPane(p: PaneSpec): boolean {
  return p.target === null;
}

/** Narrows to a pane that opens something (the inverse of `isLauncherPane`). */
export function hasTarget(p: PaneSpec): p is TargetedPane {
  return p.target !== null;
}

/** The pane to show alone: the zoomed one, or on a phone a split tab's focused pane. */
export function effectiveZoom(tab: Tab, phone: boolean): string | null {
  // A tab that collapsed to one pane can still carry `zoomed`; there is nothing to zoom then.
  if (tab.root?.kind !== "split") return null;
  return phone ? tab.activePane : tab.zoomed;
}

export function activeTab(state: WorkspaceState): Tab | undefined {
  return state.tabs.find((t) => t.id === state.activeTab);
}

/** A pane's name on screen: its title, numbered from the second same-title pane on ("web (2)"). */
export function paneLabel(pane: PaneSpec): string {
  return pane.ordinal > 1 ? `${pane.title} (${pane.ordinal})` : pane.title;
}

/** The tab title: the user's name for it, else its active pane's label, or "New tab" for a launcher. */
export function tabTitle(tab: Tab): string {
  if (tab.customTitle) return tab.customTitle;
  const panes = panesOf(tab.root);
  const pane = panes.find((p) => p.id === tab.activePane) ?? panes[0];
  return pane ? paneLabel(pane) : "New tab";
}

/** A new pane spec with the lowest number no open pane titled `title` uses. */
function newPane(
  state: WorkspaceState,
  id: string,
  target: SessionTarget | null,
  title: string,
  /** Panes made in the same step, not in `state` yet. */
  also: PaneSpec[] = [],
): PaneSpec {
  const used = new Set(
    [...state.tabs.flatMap((t) => panesOf(t.root)), ...also].filter((p) => p.title === title).map((p) => p.ordinal),
  );
  let ordinal = 1;
  while (used.has(ordinal)) ordinal++;
  return { id, target, title, ordinal };
}

function findTabOfPane(state: WorkspaceState, paneId: string): Tab | undefined {
  return state.tabs.find((t) => panesOf(t.root).some((p) => p.id === paneId));
}

function replaceTab(state: WorkspaceState, tab: Tab): WorkspaceState {
  return { ...state, tabs: state.tabs.map((t) => (t.id === tab.id ? tab : t)) };
}

/** Splits the pane `paneId` with `added` after it. */
function splitNode(node: LayoutNode, paneId: string, orientation: Orientation, added: PaneSpec): LayoutNode {
  const leaf: LayoutNode = { kind: "pane", pane: added };
  if (node.kind === "pane") {
    if (node.pane.id !== paneId) return node;
    return { kind: "split", id: `split-${added.id}`, orientation, children: [node, leaf] };
  }
  const i = node.children.findIndex((c) => c.kind === "pane" && c.pane.id === paneId);
  if (i >= 0 && node.orientation === orientation) {
    // Same direction: a sibling right after it, not a nested split.
    const children = [...node.children];
    children.splice(i + 1, 0, leaf);
    return { ...node, children };
  }
  return { ...node, children: node.children.map((c) => splitNode(c, paneId, orientation, added)) };
}

/** Trades the leaves for panes `a` and `b`. */
function swapNodes(node: LayoutNode, a: PaneSpec, b: PaneSpec): LayoutNode {
  if (node.kind === "pane") {
    if (node.pane.id === a.id) return { kind: "pane", pane: b };
    if (node.pane.id === b.id) return { kind: "pane", pane: a };
    return node;
  }
  return { ...node, children: node.children.map((c) => swapNodes(c, a, b)) };
}

/** The pane to focus once `paneId` leaves the tab: stays put unless it was the focused one. */
function activeAfterRemoving(tab: Tab, paneId: string): string | null {
  if (tab.activePane !== paneId || !tab.root) return tab.activePane;
  const order = panesOf(tab.root).map((p) => p.id);
  const i = order.indexOf(paneId);
  const left = order.filter((id) => id !== paneId);
  return left[Math.max(0, i - 1)] ?? null;
}

/** The slots of a preset: an orientation and the number of panes in it. */
const PRESET_SHAPES: Record<Preset, { orientation: Orientation; rows: number[] }> = {
  columns2: { orientation: "horizontal", rows: [2] },
  rows2: { orientation: "vertical", rows: [2] },
  columns3: { orientation: "horizontal", rows: [3] },
  grid4: { orientation: "vertical", rows: [2, 2] },
};

/** Swaps the leaf for the pane `paneId` with `pane`. */
function mapPane(node: LayoutNode, paneId: string, pane: PaneSpec): LayoutNode {
  if (node.kind === "pane") return node.pane.id === paneId ? { kind: "pane", pane } : node;
  return { ...node, children: node.children.map((c) => mapPane(c, paneId, pane)) };
}

/** Removes the pane; a split left with one child collapses into it. */
function removeNode(node: LayoutNode, paneId: string): LayoutNode | null {
  if (node.kind === "pane") return node.pane.id === paneId ? null : node;
  const children = node.children.map((c) => removeNode(c, paneId)).filter((c): c is LayoutNode => c !== null);
  if (children.length === 0) return null;
  if (children.length === 1) return children[0];
  return { ...node, children };
}

function closeTab(state: WorkspaceState, tabId: string): WorkspaceState {
  const i = state.tabs.findIndex((t) => t.id === tabId);
  if (i < 0) return state;
  const tabs = state.tabs.filter((t) => t.id !== tabId);
  let activeTab = state.activeTab;
  if (activeTab === tabId) {
    // The tab to its right, else to its left.
    activeTab = (tabs[i] ?? tabs[i - 1])?.id ?? null;
  }
  return { tabs, activeTab };
}

/**
 * `ids` in the order `prev` had them, ids not in `prev` last (in `ids` order).
 * Keeps tab panels in one DOM order however the tab strip is arranged, so a
 * reorder never moves a panel (moving an element reloads what it holds).
 */
export function stableOrder(prev: readonly string[], ids: readonly string[]): string[] {
  const present = new Set(ids);
  const kept = prev.filter((id) => present.has(id));
  const seen = new Set(kept);
  return [...kept, ...ids.filter((id) => !seen.has(id))];
}

export function reduce(state: WorkspaceState, action: WorkspaceAction): WorkspaceState {
  switch (action.type) {
    case "openTab": {
      const pane = newPane(state, action.paneId, action.target, action.title);
      const tab: Tab = {
        id: action.tabId,
        root: { kind: "pane", pane },
        activePane: pane.id,
        customTitle: null,
        zoomed: null,
        broadcast: false,
      };
      return { tabs: [...state.tabs, tab], activeTab: tab.id };
    }
    case "newTab":
      return {
        tabs: [
          ...state.tabs,
          { id: action.tabId, root: null, activePane: null, customTitle: null, zoomed: null, broadcast: false },
        ],
        activeTab: action.tabId,
      };
    case "launch": {
      const tab = state.tabs.find((t) => t.id === action.tabId);
      if (!tab || tab.root) return state;
      const pane = newPane(state, action.paneId, action.target, action.title);
      return replaceTab(state, { ...tab, root: { kind: "pane", pane }, activePane: pane.id, zoomed: null, broadcast: false });
    }
    case "split": {
      const tab = activeTab(state);
      const current = panesOf(tab?.root ?? null).find((p) => p.id === tab?.activePane);
      if (!tab?.root || !current) return state;
      const added =
        action.target === undefined
          ? newPane(state, action.paneId, current.target, current.title)
          : newPane(state, action.paneId, action.target, action.title ?? (action.target ? current.title : NEW_PANE_TITLE));
      return replaceTab(state, {
        ...tab,
        root: splitNode(tab.root, current.id, action.orientation, added),
        activePane: added.id,
        // The new pane takes focus, which ends a zoom (as focusPane does).
        zoomed: null,
      });
    }
    case "launchPane": {
      const tab = findTabOfPane(state, action.paneId);
      const current = panesOf(tab?.root ?? null).find((p) => p.id === action.paneId);
      if (!tab?.root || !current || current.target) return state;
      // Number it among the panes already titled so (the launcher it replaces has another title, so never counts).
      const filled = newPane(state, current.id, action.target, action.title);
      return replaceTab(state, { ...tab, root: mapPane(tab.root, current.id, filled) });
    }
    case "toggleZoom": {
      const tab = action.paneId ? findTabOfPane(state, action.paneId) : activeTab(state);
      const paneId = action.paneId ?? tab?.activePane;
      if (!tab?.root || !paneId || panesOf(tab.root).length < 2) return state;
      if (!panesOf(tab.root).some((p) => p.id === paneId)) return state;
      if (tab.zoomed === paneId) return replaceTab(state, { ...tab, zoomed: null });
      return replaceTab(state, { ...tab, zoomed: paneId, activePane: paneId });
    }
    case "swapPanes": {
      const tab = findTabOfPane(state, action.a);
      if (!tab?.root || action.a === action.b) return state;
      const panes = panesOf(tab.root);
      const a = panes.find((p) => p.id === action.a);
      const b = panes.find((p) => p.id === action.b);
      if (!a || !b) return state;
      return replaceTab(state, { ...tab, root: swapNodes(tab.root, a, b) });
    }
    case "movePaneToTab": {
      const tab = findTabOfPane(state, action.paneId);
      if (!tab?.root || state.tabs.some((t) => t.id === action.tabId)) return state;
      const pane = panesOf(tab.root).find((p) => p.id === action.paneId);
      const root = removeNode(tab.root, action.paneId);
      if (!pane || !root) return state; // a lone pane has nowhere to leave
      const source: Tab = {
        ...tab,
        root,
        activePane: activeAfterRemoving(tab, action.paneId),
        zoomed: tab.zoomed === action.paneId ? null : tab.zoomed,
      };
      const moved: Tab = {
        id: action.tabId,
        root: { kind: "pane", pane },
        activePane: pane.id,
        customTitle: null,
        zoomed: null,
        broadcast: false,
      };
      const tabs = state.tabs.flatMap((t) => (t.id === tab.id ? [source, moved] : [t]));
      return { tabs, activeTab: moved.id };
    }
    case "applyPreset": {
      const tab = activeTab(state);
      if (!tab) return state;
      // A launcher tab (no panes yet) is filled with launcher panes.
      const panes = panesOf(tab.root);
      const { orientation, rows } = PRESET_SHAPES[action.preset];
      const slots = rows.reduce((n, r) => n + r, 0);
      if (action.paneIds.length < slots - panes.length) return state;
      const leaves: PaneSpec[] = panes.slice(0, slots);
      const added: PaneSpec[] = [];
      for (const id of action.paneIds) {
        if (leaves.length >= slots) break;
        const launcher = newPane(state, id, null, NEW_PANE_TITLE, added);
        added.push(launcher);
        leaves.push(launcher);
      }
      const leaf = (pane: PaneSpec): LayoutNode => ({ kind: "pane", pane });
      // Sizes are keyed by split id: ids new to each application start the preset equal, not at the old dragged sizes.
      const uid = action.paneIds[0] ?? "";
      // One row: the preset's own direction. A grid: rows side by side inside a stack.
      const rowOrientation: Orientation = rows.length === 1 ? orientation : "horizontal";
      // Extra panes share the last slot, split in the direction of its row.
      const extras = panes.slice(slots);
      const slotNode = (pane: PaneSpec, i: number): LayoutNode =>
        i < slots - 1 || extras.length === 0
          ? leaf(pane)
          : { kind: "split", id: `preset-extra-${uid}`, orientation: rowOrientation, children: [pane, ...extras].map(leaf) };
      let n = 0;
      const rowNodes = rows.map((count, r): LayoutNode => {
        const children = leaves.slice(n, n + count).map((p, k) => slotNode(p, n + k));
        n += count;
        return { kind: "split", id: `preset-row-${r}-${uid}`, orientation: rowOrientation, children };
      });
      const root: LayoutNode =
        rows.length === 1 ? rowNodes[0] : { kind: "split", id: `preset-root-${uid}`, orientation, children: rowNodes };
      return replaceTab(state, { ...tab, root, activePane: tab.activePane ?? leaves[0].id, zoomed: null });
    }
    case "toggleBroadcast": {
      const tab = state.tabs.find((t) => t.id === action.tabId);
      return tab ? replaceTab(state, { ...tab, broadcast: !tab.broadcast }) : state;
    }
    case "closePane": {
      const tab = action.paneId ? findTabOfPane(state, action.paneId) : activeTab(state);
      if (!tab) return state;
      const paneId = action.paneId ?? tab.activePane;
      if (!tab.root || !paneId) return closeTab(state, tab.id);
      const root = removeNode(tab.root, paneId);
      if (!root) return closeTab(state, tab.id);
      const activePane = activeAfterRemoving(tab, paneId);
      return replaceTab(state, { ...tab, root, activePane, zoomed: tab.zoomed === paneId ? null : tab.zoomed });
    }
    case "closeTab":
      return closeTab(state, action.tabId);
    case "selectTab":
      return state.tabs.some((t) => t.id === action.tabId) ? { ...state, activeTab: action.tabId } : state;
    case "renameTab": {
      const tab = state.tabs.find((t) => t.id === action.tabId);
      if (!tab) return state;
      return replaceTab(state, { ...tab, customTitle: action.title.trim() || null });
    }
    case "moveTab": {
      const from = state.tabs.findIndex((t) => t.id === action.tabId);
      if (from < 0) return state;
      const to = Math.min(Math.max(action.toIndex, 0), state.tabs.length - 1);
      if (to === from) return state;
      const tabs = [...state.tabs];
      const [tab] = tabs.splice(from, 1);
      tabs.splice(to, 0, tab);
      return { ...state, tabs };
    }
    case "focusPane": {
      const tab = findTabOfPane(state, action.paneId);
      if (!tab) return state;
      const keepsZoom = tab.zoomed === null || tab.zoomed === action.paneId;
      if (tab.activePane === action.paneId && state.activeTab === tab.id && keepsZoom) return state;
      const zoomed = keepsZoom ? tab.zoomed : null;
      return { ...replaceTab(state, { ...tab, activePane: action.paneId, zoomed }), activeTab: tab.id };
    }
  }
}
