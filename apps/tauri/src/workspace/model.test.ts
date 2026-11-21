import { describe, expect, it } from "vitest";
import {
  EMPTY_WORKSPACE,
  NEW_PANE_TITLE,
  PRESETS,
  activeTab,
  effectiveZoom,
  isLauncherPane,
  paneLabel,
  panesOf,
  reduce,
  stableOrder,
  tabTitle,
  type LayoutNode,
  type WorkspaceState,
} from "./model";

const LOCAL = { kind: "local" } as const;

/** The layout as nested arrays of pane ids, with "h"/"v" for split orientation. */
function shape(node: LayoutNode | null): unknown {
  if (!node) return null;
  if (node.kind === "pane") return node.pane.id;
  return [node.orientation === "horizontal" ? "h" : "v", ...node.children.map(shape)];
}

function open(state: WorkspaceState, tabId: string, paneId: string): WorkspaceState {
  return reduce(state, { type: "openTab", tabId, paneId, target: LOCAL, title: "Local shell" });
}

describe("workspace model", () => {
  it("opens_tabs_and_splits_the_active_pane", () => {
    let s = open(EMPTY_WORKSPACE, "t1", "p1");
    expect(s.activeTab).toBe("t1");
    expect(activeTab(s)?.activePane).toBe("p1");

    s = reduce(s, { type: "split", orientation: "horizontal", paneId: "p2" });
    expect(shape(activeTab(s)!.root)).toEqual(["h", "p1", "p2"]);
    expect(activeTab(s)!.activePane).toBe("p2");
    // The new pane connects to the same target.
    expect(panesOf(activeTab(s)!.root).map((p) => p.target)).toEqual([LOCAL, LOCAL]);

    // Same direction: a sibling, not a nested split.
    s = reduce(s, { type: "split", orientation: "horizontal", paneId: "p3" });
    expect(shape(activeTab(s)!.root)).toEqual(["h", "p1", "p2", "p3"]);

    // Other direction: the active pane is split in place.
    s = reduce(s, { type: "focusPane", paneId: "p2" });
    s = reduce(s, { type: "split", orientation: "vertical", paneId: "p4" });
    expect(shape(activeTab(s)!.root)).toEqual(["h", "p1", ["v", "p2", "p4"], "p3"]);
  });

  it("closing_panes_collapses_splits_and_then_the_tab", () => {
    let s = open(EMPTY_WORKSPACE, "t1", "p1");
    s = reduce(s, { type: "split", orientation: "horizontal", paneId: "p2" });
    s = reduce(s, { type: "split", orientation: "vertical", paneId: "p3" });
    expect(shape(activeTab(s)!.root)).toEqual(["h", "p1", ["v", "p2", "p3"]]);

    s = reduce(s, { type: "closePane" });
    expect(shape(activeTab(s)!.root)).toEqual(["h", "p1", "p2"]);
    expect(activeTab(s)!.activePane).toBe("p2");

    s = reduce(s, { type: "closePane", paneId: "p1" });
    expect(shape(activeTab(s)!.root)).toBe("p2");

    s = reduce(s, { type: "closePane" });
    expect(s.tabs).toEqual([]);
    expect(s.activeTab).toBeNull();
  });

  it("launcher_tabs_become_terminals_and_close_to_a_neighbour", () => {
    let s = open(EMPTY_WORKSPACE, "t1", "p1");
    s = reduce(s, { type: "newTab", tabId: "t2" });
    expect(s.activeTab).toBe("t2");
    expect(activeTab(s)!.root).toBeNull();
    // Splitting a launcher does nothing.
    expect(reduce(s, { type: "split", orientation: "horizontal", paneId: "x" })).toBe(s);

    s = reduce(s, { type: "launch", tabId: "t2", paneId: "p2", target: LOCAL, title: "Local shell" });
    expect(shape(activeTab(s)!.root)).toBe("p2");

    s = reduce(s, { type: "newTab", tabId: "t3" });
    s = reduce(s, { type: "selectTab", tabId: "t2" });
    s = reduce(s, { type: "closeTab", tabId: "t2" });
    expect(s.tabs.map((t) => t.id)).toEqual(["t1", "t3"]);
    expect(s.activeTab).toBe("t3");
    s = reduce(s, { type: "closePane" }); // the launcher: closes its tab
    expect(s.tabs.map((t) => t.id)).toEqual(["t1"]);
    expect(s.activeTab).toBe("t1");
  });

  describe("moveTab", () => {
    const three = () => {
      let s = open(EMPTY_WORKSPACE, "a", "pa");
      s = open(s, "b", "pb");
      s = open(s, "c", "pc");
      return reduce(s, { type: "selectTab", tabId: "b" });
    };
    const ids = (s: WorkspaceState) => s.tabs.map((t) => t.id);

    it("moves_a_tab_to_an_index_of_the_result", () => {
      const s = three();
      expect(ids(reduce(s, { type: "moveTab", tabId: "c", toIndex: 0 }))).toEqual(["c", "a", "b"]);
      expect(ids(reduce(s, { type: "moveTab", tabId: "a", toIndex: 2 }))).toEqual(["b", "c", "a"]);
      expect(ids(reduce(s, { type: "moveTab", tabId: "a", toIndex: 99 }))).toEqual(["b", "c", "a"]);
      expect(ids(reduce(s, { type: "moveTab", tabId: "c", toIndex: -5 }))).toEqual(["c", "a", "b"]);
    });

    it("keeps_the_active_tab", () => {
      const s = reduce(three(), { type: "moveTab", tabId: "b", toIndex: 0 });
      expect(s.activeTab).toBe("b");
      expect(reduce(three(), { type: "moveTab", tabId: "c", toIndex: 0 }).activeTab).toBe("b");
    });

    it("no_move_returns_the_same_state", () => {
      const s = three();
      expect(reduce(s, { type: "moveTab", tabId: "b", toIndex: 1 })).toBe(s);
      expect(reduce(s, { type: "moveTab", tabId: "nope", toIndex: 0 })).toBe(s);
      expect(reduce(s, { type: "moveTab", tabId: "c", toIndex: 99 })).toBe(s);
    });
  });

  describe("stableOrder", () => {
    it("keeps_the_previous_order_and_appends_new_ids", () => {
      expect(stableOrder(["a", "b"], ["b", "c", "a"])).toEqual(["a", "b", "c"]);
    });

    it("drops_ids_that_are_gone", () => {
      expect(stableOrder(["a", "b", "c"], ["c", "a"])).toEqual(["a", "c"]);
      expect(stableOrder([], ["x", "y"])).toEqual(["x", "y"]);
    });
  });

  describe("same-name panes", () => {
    const WEB = { kind: "host", hostId: "h-web" } as const;
    const openWeb = (state: WorkspaceState, tabId: string, paneId: string, title = "web") =>
      reduce(state, { type: "openTab", tabId, paneId, target: WEB, title });
    const labels = (s: WorkspaceState) => s.tabs.flatMap((t) => panesOf(t.root).map(paneLabel));

    it("numbers_duplicate_titles", () => {
      let s = openWeb(EMPTY_WORKSPACE, "t1", "p1");
      s = openWeb(s, "t2", "p2");
      s = openWeb(s, "t3", "p3", "db");
      expect(labels(s)).toEqual(["web", "web (2)", "db"]);
      expect(s.tabs.map(tabTitle)).toEqual(["web", "web (2)", "db"]);
    });

    it("numbers_reuse_lowest_free", () => {
      let s = openWeb(EMPTY_WORKSPACE, "t1", "p1");
      s = openWeb(s, "t2", "p2");
      s = openWeb(s, "t3", "p3");
      s = reduce(s, { type: "closeTab", tabId: "t2" });
      expect(labels(s)).toEqual(["web", "web (3)"]);
      s = openWeb(s, "t4", "p4");
      expect(labels(s)).toEqual(["web", "web (3)", "web (2)"]);
      // A launcher tab gets its number when it launches.
      s = reduce(s, { type: "newTab", tabId: "t5" });
      s = reduce(s, { type: "launch", tabId: "t5", paneId: "p5", target: WEB, title: "web" });
      expect(labels(s)).toEqual(["web", "web (3)", "web (2)", "web (4)"]);
    });

    it("split_gets_next_number", () => {
      let s = openWeb(EMPTY_WORKSPACE, "t1", "p1");
      s = reduce(s, { type: "split", orientation: "horizontal", paneId: "p2" });
      expect(labels(s)).toEqual(["web", "web (2)"]);
      // The tab is titled by its focused pane.
      expect(tabTitle(activeTab(s)!)).toBe("web (2)");
      s = reduce(s, { type: "focusPane", paneId: "p1" });
      expect(tabTitle(activeTab(s)!)).toBe("web");
    });

    it("rename_and_clear", () => {
      let s = openWeb(EMPTY_WORKSPACE, "t1", "p1");
      s = reduce(s, { type: "renameTab", tabId: "t1", title: "  prod logs " });
      expect(tabTitle(activeTab(s)!)).toBe("prod logs");
      // The name stays when focus moves between its panes.
      s = reduce(s, { type: "split", orientation: "vertical", paneId: "p2" });
      expect(tabTitle(activeTab(s)!)).toBe("prod logs");
      s = reduce(s, { type: "renameTab", tabId: "t1", title: "   " });
      expect(tabTitle(activeTab(s)!)).toBe("web (2)");
    });
  });

  describe("launcher panes, targeted splits, zoom and broadcast", () => {
    const WEB = { kind: "host", hostId: "h-web" } as const;
    const DB = { kind: "host", hostId: "h-db" } as const;
    const two = () => reduce(open(EMPTY_WORKSPACE, "t1", "p1"), { type: "split", orientation: "horizontal", paneId: "p2" });
    const pane = (s: WorkspaceState, id: string) => s.tabs.flatMap((t) => panesOf(t.root)).find((p) => p.id === id)!;

    it("split_onto_target_opens_that_target", () => {
      let s = open(EMPTY_WORKSPACE, "t1", "p1");
      s = reduce(s, { type: "split", orientation: "horizontal", paneId: "p2", target: DB, title: "b" });
      expect(pane(s, "p2").target).toBe(DB);
      expect(pane(s, "p2").title).toBe("b");
      expect(activeTab(s)!.activePane).toBe("p2");
    });

    it("split_with_null_target_makes_launcher_pane", () => {
      let s = open(EMPTY_WORKSPACE, "t1", "p1");
      s = reduce(s, { type: "split", orientation: "vertical", paneId: "p2", target: null });
      expect(pane(s, "p2").target).toBeNull();
      expect(pane(s, "p2").title).toBe("New pane");
      expect(NEW_PANE_TITLE).toBe("New pane");
      expect(isLauncherPane(pane(s, "p2"))).toBe(true);
      expect(isLauncherPane(pane(s, "p1"))).toBe(false);
      expect(shape(activeTab(s)!.root)).toEqual(["v", "p1", "p2"]);
    });

    it("split_without_target_clones", () => {
      const s = two();
      expect(pane(s, "p2").target).toBe(LOCAL);
      expect(pane(s, "p2").title).toBe("Local shell");
    });

    it("launch_pane_fills_launcher_in_place", () => {
      let s = openWebTab();
      s = reduce(s, { type: "split", orientation: "horizontal", paneId: "p2", target: null });
      s = reduce(s, { type: "split", orientation: "vertical", paneId: "p3", target: null });
      const before = shape(activeTab(s)!.root);
      // "web" is taken by p1, so the filled pane is "web (2)"; the second launcher keeps its own number.
      s = reduce(s, { type: "launchPane", paneId: "p2", target: WEB, title: "web" });
      expect(shape(activeTab(s)!.root)).toEqual(before);
      expect(pane(s, "p2")).toMatchObject({ id: "p2", target: WEB, title: "web", ordinal: 2 });
      expect(isLauncherPane(pane(s, "p2"))).toBe(false);
      expect(pane(s, "p3").target).toBeNull();
      // Already has a target: no-op.
      expect(reduce(s, { type: "launchPane", paneId: "p2", target: DB, title: "db" })).toBe(s);
      expect(reduce(s, { type: "launchPane", paneId: "nope", target: DB, title: "db" })).toBe(s);
    });

    function openWebTab() {
      return reduce(EMPTY_WORKSPACE, { type: "openTab", tabId: "t1", paneId: "p1", target: WEB, title: "web" });
    }

    it("toggle_zoom_sets_and_clears", () => {
      let s = two();
      s = reduce(s, { type: "toggleZoom" });
      expect(activeTab(s)!.zoomed).toBe("p2");
      s = reduce(s, { type: "toggleZoom" });
      expect(activeTab(s)!.zoomed).toBeNull();
      // Another pane: zoom it and focus it.
      s = reduce(s, { type: "toggleZoom", paneId: "p1" });
      expect(activeTab(s)!.zoomed).toBe("p1");
      expect(activeTab(s)!.activePane).toBe("p1");
      // Zooming a different pane while zoomed moves the zoom.
      s = reduce(s, { type: "toggleZoom", paneId: "p2" });
      expect(activeTab(s)!.zoomed).toBe("p2");
      expect(activeTab(s)!.activePane).toBe("p2");
    });

    it("toggle_zoom_single_pane_is_noop", () => {
      const s = open(EMPTY_WORKSPACE, "t1", "p1");
      expect(reduce(s, { type: "toggleZoom" })).toBe(s);
      // A launcher tab has nothing to zoom either.
      const l = reduce(s, { type: "newTab", tabId: "t2" });
      expect(reduce(l, { type: "toggleZoom" })).toBe(l);
    });

    it("focus_other_pane_clears_zoom", () => {
      let s = reduce(two(), { type: "toggleZoom" });
      s = reduce(s, { type: "focusPane", paneId: "p2" });
      expect(activeTab(s)!.zoomed).toBe("p2");
      s = reduce(s, { type: "focusPane", paneId: "p1" });
      expect(activeTab(s)!.zoomed).toBeNull();
      expect(activeTab(s)!.activePane).toBe("p1");
    });

    it("close_zoomed_pane_clears_zoom", () => {
      let s = reduce(two(), { type: "toggleZoom" });
      s = reduce(s, { type: "closePane", paneId: "p2" });
      expect(activeTab(s)!.zoomed).toBeNull();
      // Closing another pane leaves the zoom alone.
      s = reduce(two(), { type: "toggleZoom" });
      s = reduce(s, { type: "closePane", paneId: "p1" });
      expect(activeTab(s)!.zoomed).toBe("p2");
    });

    it("close_active_pane_keeps_broadcast", () => {
      let s = reduce(two(), { type: "toggleBroadcast", tabId: "t1" });
      s = reduce(s, { type: "closePane" });
      expect(activeTab(s)!.broadcast).toBe(true);
    });

    it("toggle_broadcast_flips_only_that_tab", () => {
      let s = open(open(EMPTY_WORKSPACE, "t1", "p1"), "t2", "p2");
      expect(s.tabs.map((t) => t.broadcast)).toEqual([false, false]);
      s = reduce(s, { type: "toggleBroadcast", tabId: "t1" });
      expect(s.tabs.map((t) => t.broadcast)).toEqual([true, false]);
      s = reduce(s, { type: "toggleBroadcast", tabId: "t1" });
      expect(s.tabs.map((t) => t.broadcast)).toEqual([false, false]);
      expect(reduce(s, { type: "toggleBroadcast", tabId: "nope" })).toBe(s);
    });

    it("new_tabs_start_unzoomed_without_broadcast", () => {
      const s = reduce(open(EMPTY_WORKSPACE, "t1", "p1"), { type: "newTab", tabId: "t2" });
      expect(s.tabs.map((t) => [t.zoomed, t.broadcast])).toEqual([[null, false], [null, false]]);
    });

    it("effective_zoom_on_phone_is_active_pane_of_split", () => {
      const s = two();
      const tab = activeTab(s)!;
      expect(effectiveZoom(tab, true)).toBe("p2");
      expect(effectiveZoom(tab, false)).toBeNull();
      expect(effectiveZoom(reduce(s, { type: "toggleZoom" }).tabs[0], false)).toBe("p2");
      const single = activeTab(open(EMPTY_WORKSPACE, "t1", "p1"))!;
      expect(effectiveZoom(single, true)).toBeNull();
      expect(effectiveZoom(single, false)).toBeNull();
    });

    it("effective_zoom_ignores_zoom_on_single_pane_tab", () => {
      // Closing a sibling leaves `zoomed` set on the survivor; nothing is zoomed then.
      const tab = activeTab(reduce(reduce(two(), { type: "toggleZoom" }), { type: "closePane", paneId: "p1" }))!;
      expect(tab.zoomed).toBe("p2");
      expect(tab.root?.kind).toBe("pane");
      expect(effectiveZoom(tab, false)).toBeNull();
      expect(effectiveZoom(tab, true)).toBeNull();
    });

    it("split_while_zoomed_clears_zoom", () => {
      const s = reduce(reduce(two(), { type: "toggleZoom" }), { type: "split", orientation: "vertical", paneId: "p3" });
      expect(activeTab(s)).toMatchObject({ zoomed: null, activePane: "p3" });
    });
  });

  describe("swap, move to a new tab and presets", () => {
    /** One tab "t1" with panes p1..pN split side by side; the last one is focused. */
    function row(n: number): WorkspaceState {
      let s = open(EMPTY_WORKSPACE, "t1", "p1");
      for (let i = 2; i <= n; i++) s = reduce(s, { type: "split", orientation: "horizontal", paneId: `p${i}` });
      return s;
    }
    const ids = (s: WorkspaceState, tabId = "t1") => panesOf(s.tabs.find((t) => t.id === tabId)!.root).map((p) => p.id);

    it("swap_trades_places_and_keeps_focus", () => {
      let s = row(3); // [p1, p2, p3], focus p3
      s = reduce(s, { type: "split", orientation: "vertical", paneId: "p4" }); // h[p1,p2,v[p3,p4]], focus p4
      s = reduce(s, { type: "swapPanes", a: "p1", b: "p4" });
      expect(shape(activeTab(s)!.root)).toEqual(["h", "p4", "p2", ["v", "p3", "p1"]]);
      expect(activeTab(s)!.activePane).toBe("p4");
      // The specs travel with their ids.
      expect(panesOf(activeTab(s)!.root).map((p) => p.ordinal)).toEqual([4, 2, 3, 1]);
    });

    it("swap_across_tabs_is_noop", () => {
      const s = open(row(2), "t2", "q1");
      expect(reduce(s, { type: "swapPanes", a: "p1", b: "q1" })).toBe(s);
      expect(reduce(s, { type: "swapPanes", a: "p1", b: "p1" })).toBe(s);
      expect(reduce(s, { type: "swapPanes", a: "p1", b: "nope" })).toBe(s);
    });

    it("move_pane_to_tab_opens_after_source", () => {
      let s = open(row(3), "t2", "q1"); // t1[p1 p2 p3], t2
      s = reduce(s, { type: "selectTab", tabId: "t1" });
      s = reduce(s, { type: "toggleZoom", paneId: "p3" });
      s = reduce(s, { type: "movePaneToTab", paneId: "p3", tabId: "tn" });
      expect(s.tabs.map((t) => t.id)).toEqual(["t1", "tn", "t2"]);
      expect(s.activeTab).toBe("tn");
      expect(ids(s, "t1")).toEqual(["p1", "p2"]);
      expect(ids(s, "tn")).toEqual(["p3"]);
      const moved = s.tabs[1];
      expect(moved).toMatchObject({ customTitle: null, zoomed: null, broadcast: false, activePane: "p3" });
      // The source follows closePane's focus rule and drops its zoom (p3 was zoomed).
      expect(s.tabs[0]).toMatchObject({ activePane: "p2", zoomed: null });
    });

    it("move_pane_to_tab_keeps_source_zoom_of_another_pane", () => {
      let s = reduce(row(3), { type: "toggleZoom", paneId: "p1" });
      s = reduce(s, { type: "movePaneToTab", paneId: "p3", tabId: "tn" });
      expect(s.tabs[0]).toMatchObject({ zoomed: "p1", activePane: "p1" });
    });

    it("move_lone_pane_is_noop", () => {
      const s = open(row(2), "t2", "q1");
      expect(reduce(s, { type: "movePaneToTab", paneId: "q1", tabId: "tn" })).toBe(s);
      expect(reduce(s, { type: "movePaneToTab", paneId: "nope", tabId: "tn" })).toBe(s);
    });

    it("presets_list_matches_the_spec", () => {
      expect(PRESETS).toEqual([
        { id: "columns2", label: "2 columns" },
        { id: "rows2", label: "2 rows" },
        { id: "columns3", label: "3 columns" },
        { id: "grid4", label: "2×2 grid" },
      ]);
    });

    it("preset_grid4_from_one_pane", () => {
      let s = open(EMPTY_WORKSPACE, "t1", "p1");
      s = reduce(s, { type: "applyPreset", preset: "grid4", paneIds: ["n1", "n2", "n3", "n4"] });
      expect(shape(activeTab(s)!.root)).toEqual(["v", ["h", "p1", "n1"], ["h", "n2", "n3"]]);
      const panes = panesOf(activeTab(s)!.root);
      expect(panes.map(isLauncherPane)).toEqual([false, true, true, true]);
      expect(panes.slice(1).map((p) => p.title)).toEqual([NEW_PANE_TITLE, NEW_PANE_TITLE, NEW_PANE_TITLE]);
      // Several launchers at once still get distinct numbers.
      expect(panes.slice(1).map(paneLabel)).toEqual(["New pane", "New pane (2)", "New pane (3)"]);
      expect(activeTab(s)!.activePane).toBe("p1");
    });

    it("preset_other_shapes", () => {
      const one = open(EMPTY_WORKSPACE, "t1", "p1");
      const shapeOf = (preset: "columns2" | "rows2" | "columns3") =>
        shape(activeTab(reduce(one, { type: "applyPreset", preset, paneIds: ["n1", "n2", "n3", "n4"] }))!.root);
      expect(shapeOf("columns2")).toEqual(["h", "p1", "n1"]);
      expect(shapeOf("rows2")).toEqual(["v", "p1", "n1"]);
      expect(shapeOf("columns3")).toEqual(["h", "p1", "n1", "n2"]);
    });

    it("preset_columns2_from_two_panes_adds_nothing", () => {
      let s = reduce(open(EMPTY_WORKSPACE, "t1", "p1"), { type: "split", orientation: "vertical", paneId: "p2" });
      s = reduce(s, { type: "applyPreset", preset: "columns2", paneIds: ["n1", "n2", "n3", "n4"] });
      expect(shape(activeTab(s)!.root)).toEqual(["h", "p1", "p2"]);
      expect(ids(s)).toEqual(["p1", "p2"]);
    });

    it("preset_keeps_extra_panes_in_last_slot", () => {
      const s = reduce(row(5), { type: "applyPreset", preset: "grid4", paneIds: ["n1", "n2", "n3", "n4"] });
      expect(shape(activeTab(s)!.root)).toEqual(["v", ["h", "p1", "p2"], ["h", "p3", ["h", "p4", "p5"]]]);
      expect(ids(s)).toEqual(["p1", "p2", "p3", "p4", "p5"]);
      const r = reduce(row(3), { type: "applyPreset", preset: "rows2", paneIds: ["n1", "n2", "n3", "n4"] });
      expect(shape(activeTab(r)!.root)).toEqual(["v", "p1", ["v", "p2", "p3"]]);
    });

    it("preset_split_ids_are_unique", () => {
      const splitIds = (node: LayoutNode | null): string[] =>
        !node || node.kind === "pane" ? [] : [node.id, ...node.children.flatMap(splitIds)];
      for (const n of [1, 2, 5, 7]) {
        for (const { id } of PRESETS) {
          const s = reduce(row(n), { type: "applyPreset", preset: id, paneIds: ["n1", "n2", "n3", "n4"] });
          const all = splitIds(activeTab(s)!.root);
          expect(new Set(all).size).toBe(all.length);
        }
      }
    });

    it("preset_split_ids_differ_between_applications", () => {
      // Sizes are keyed by split id: a new preset must not inherit the last one's dragged sizes.
      const splitIds = (node: LayoutNode | null): string[] =>
        !node || node.kind === "pane" ? [] : [node.id, ...node.children.flatMap(splitIds)];
      for (const n of [1, 3, 5]) {
        for (const { id } of PRESETS) {
          const a = reduce(row(n), { type: "applyPreset", preset: id, paneIds: ["a1", "a2", "a3", "a4"] });
          const b = reduce(a, { type: "applyPreset", preset: id, paneIds: ["b1", "b2", "b3", "b4"] });
          const before = splitIds(activeTab(a)!.root);
          const after = splitIds(activeTab(b)!.root);
          expect(after.some((s) => before.includes(s))).toBe(false);
        }
      }
    });

    it("preset_clears_zoom_keeps_active", () => {
      let s = reduce(row(3), { type: "toggleZoom", paneId: "p2" });
      s = reduce(s, { type: "toggleBroadcast", tabId: "t1" });
      s = reduce(s, { type: "applyPreset", preset: "columns3", paneIds: ["n1", "n2", "n3", "n4"] });
      expect(activeTab(s)).toMatchObject({ zoomed: null, activePane: "p2", broadcast: true });
    });

    it("preset_on_launcher_tab_fills_with_launcher_panes", () => {
      const s = reduce(EMPTY_WORKSPACE, { type: "newTab", tabId: "t1" });
      const ids4 = ["n1", "n2", "n3", "n4"];
      const grid = activeTab(reduce(s, { type: "applyPreset", preset: "grid4", paneIds: ids4 }))!;
      expect(shape(grid.root)).toEqual(["v", ["h", "n1", "n2"], ["h", "n3", "n4"]]);
      const cols = activeTab(reduce(s, { type: "applyPreset", preset: "columns2", paneIds: ids4 }))!;
      expect(shape(cols.root)).toEqual(["h", "n1", "n2"]);
      for (const tab of [grid, cols]) {
        expect(panesOf(tab.root).every(isLauncherPane)).toBe(true);
        expect(tab.activePane).toBe("n1");
        expect(tab.zoomed).toBeNull();
      }
      // Split ids are unique within the tab.
      const splitIds: string[] = [];
      const walk = (n: LayoutNode | null) => {
        if (n?.kind === "split") {
          splitIds.push(n.id);
          n.children.forEach(walk);
        }
      };
      walk(grid.root);
      expect(new Set(splitIds).size).toBe(splitIds.length);
    });
  });
});
