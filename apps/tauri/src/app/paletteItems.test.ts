import { describe, expect, it } from "vitest";
import type { ConnSettings, Group, Host, Tag } from "../api/types";
import { EMPTY_WORKSPACE, reduce, type WorkspaceAction, type WorkspaceState } from "../workspace/model";
import { shortcutLabels } from "../workspace/shortcuts";
import { paletteGroups, type PaletteGroup, type PaletteSource } from "./paletteItems";

const none: ConnSettings = {
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
const host = (id: string, label: string, address: string, tagIds: string[] = [], groupId: string | null = null): Host => ({
  id,
  label,
  address,
  groupId,
  tagIds,
  settings: none,
  forwardRuleIds: [],
});
const tags: Tag[] = [{ id: "t-prod", name: "prod", color: "#fff" }];
const groups: Group[] = [{ id: "g1", name: "Lab", parentId: null, settings: none }];
const hosts = [
  host("h1", "web-1", "10.0.0.1", ["t-prod"], "g1"),
  host("h2", "db-1", "10.0.1.9"),
  host("h3", "api", "api.example.com"),
];
const routes = [
  { id: "hosts" as const, label: "Hosts" },
  { id: "keychain" as const, label: "Keychain" },
];

const WEB = { kind: "host", hostId: "h1" } as const;

function run(s: WorkspaceState, ...actions: WorkspaceAction[]): WorkspaceState {
  return actions.reduce(reduce, s);
}
/** One tab "web-1" with one pane. */
const oneTab = run(EMPTY_WORKSPACE, { type: "openTab", tabId: "t1", paneId: "p1", target: WEB, title: "web-1" });
/** One tab "web-1" split in two ("web-1", "web-1 (2)"). */
const splitTab = run(oneTab, { type: "split", orientation: "horizontal", paneId: "p2" });

function source(over: Partial<PaletteSource> = {}): PaletteSource {
  return {
    workspace: EMPTY_WORKSPACE,
    hosts,
    tags,
    groups,
    touch: false,
    phone: false,
    sidebarHidden: false,
    labels: shortcutLabels(true),
    routes,
    ...over,
  };
}
const all = { kind: "all" } as const;
const titles = (g: PaletteGroup[]) => g.map((x) => x.title);
const actionLabels = (g: PaletteGroup[]) => g.find((x) => x.title === "Actions")?.items.map((i) => (i.kind === "action" ? i.label : "")) ?? [];
const actionIds = (g: PaletteGroup[]) => g.find((x) => x.title === "Actions")?.items.map((i) => (i.kind === "action" ? i.id : "")) ?? [];

describe("palette items", () => {
  it("empty_query_groups_in_order", () => {
    const g = paletteGroups("", source({ workspace: oneTab }), all);
    expect(titles(g)).toEqual(["Open terminals", "Hosts", "Actions"]);
    // Hosts are sorted as everywhere else.
    expect(g[1].items.map((i) => (i.kind === "host" ? i.host.label : ""))).toEqual(["api", "db-1", "web-1"]);
    // No terminals open: that group is dropped.
    expect(titles(paletteGroups("", source(), all))).toEqual(["Hosts", "Actions"]);
    // Only the first 50 hosts.
    const many = Array.from({ length: 80 }, (_, i) => host(`m${i}`, `box-${String(i).padStart(2, "0")}`, "x"));
    const hostsGroup = paletteGroups("", source({ hosts: many }), all).find((x) => x.title === "Hosts")!;
    expect(hostsGroup.items).toHaveLength(50);
  });

  it("terminals_list_tabs_and_split_panes", () => {
    const g = paletteGroups("", source({ workspace: splitTab }), all);
    const terms = g[0].items.flatMap((i) => (i.kind === "terminal" ? [i] : []));
    expect(terms.map((t) => [t.label, t.tabId, t.paneId])).toEqual([
      // The tab is named after its focused pane.
      ["web-1 (2)", "t1", null],
      ["web-1", "t1", "p1"],
      ["web-1 (2)", "t1", "p2"],
    ]);
    expect(terms[2].detail).toBe('Pane in "web-1 (2)"');
    // A one-pane tab is just the tab.
    expect(paletteGroups("", source({ workspace: oneTab }), all)[0].items).toHaveLength(1);
    // Matched on the label, ignoring case.
    const q = paletteGroups("(2)", source({ workspace: splitTab }), all)[0];
    expect(q.items.map((i) => i.key)).toEqual(["tab:t1", "pane:p2"]);
    expect(paletteGroups("WEB", source({ workspace: oneTab }), all)[0].title).toBe("Open terminals");
  });

  it("terminals_carry_their_panes_target", () => {
    const terms = (ws: WorkspaceState) =>
      paletteGroups("", source({ workspace: ws }), all)[0].items.flatMap((i) => (i.kind === "terminal" ? [i] : []));
    // A tab row has its focused pane's target; the title is the pane's own (not numbered).
    expect(terms(splitTab).map((t) => [t.key, t.target, t.title])).toEqual([
      ["tab:t1", WEB, "web-1"],
      ["pane:p1", WEB, "web-1"],
      ["pane:p2", WEB, "web-1"],
    ]);
    const local = run(oneTab, { type: "split", orientation: "vertical", paneId: "p3", target: { kind: "local" }, title: "Local shell" });
    expect(terms(local).map((t) => t.target)).toEqual([{ kind: "local" }, WEB, { kind: "local" }]);
    // A launcher pane, or a launcher tab, has no target.
    const launcher = run(oneTab, { type: "split", orientation: "vertical", paneId: "p3", target: null });
    expect(terms(launcher).map((t) => t.target)).toEqual([null, WEB, null]);
    expect(terms(run(EMPTY_WORKSPACE, { type: "newTab", tabId: "t9" })).map((t) => t.target)).toEqual([null]);
  });

  it("host_matches_address_and_tag", () => {
    const hostLabels = (q: string) =>
      (paletteGroups(q, source(), all).find((x) => x.title === "Hosts")?.items ?? []).map((i) => (i.kind === "host" ? i.host.label : ""));
    expect(hostLabels("10.0.1")).toEqual(["db-1"]);
    expect(hostLabels("PROD")).toEqual(["web-1"]);
    expect(hostLabels("api.exa")).toEqual(["api"]);
    // The detail line is the group path and the tags.
    const web = paletteGroups("web", source(), all)[0].items[0];
    expect(web.kind === "host" && web.detail).toBe("Lab · prod");
  });

  it("touch_has_no_local_terminal_action", () => {
    expect(actionLabels(paletteGroups("", source(), all))).toContain("Local terminal");
    expect(actionLabels(paletteGroups("", source({ touch: true }), all))).not.toContain("Local terminal");
    expect(titles(paletteGroups("", source({ touch: true }), { kind: "pickHost", orientation: "horizontal" }))).toEqual(["Hosts"]);
  });

  it("phone_has_no_split_or_layout_actions", () => {
    const labels = actionLabels(paletteGroups("", source({ workspace: splitTab, phone: true }), all));
    expect(labels).toEqual(["New tab", "Local terminal", "Close pane", "Go to Hosts", "Go to Keychain", "Snippets", "Themes", "New host", "Lock vault"]);
  });

  it("actions_need_a_root_tab", () => {
    // Nothing open: no split, zoom, broadcast, close or layout.
    expect(actionLabels(paletteGroups("", source(), all))).toEqual([
      "New tab",
      "Local terminal",
      "Go to Hosts",
      "Go to Keychain",
      "Snippets",
      "Themes",
      "New host",
      "Hide sidebar",
      "Lock vault",
    ]);
    // A launcher tab takes a layout, nothing else of the tab's.
    const launcher = run(EMPTY_WORKSPACE, { type: "newTab", tabId: "t1" });
    expect(actionIds(paletteGroups("", source({ workspace: launcher }), all))).toEqual([
      "newTab",
      "localTerminal",
      "preset:columns2",
      "preset:rows2",
      "preset:columns3",
      "preset:grid4",
      "go:hosts",
      "go:keychain",
      "panel:snippets",
      "panel:themes",
      "newHost",
      "hideSidebar",
      "lock",
    ]);
    // One pane: splits, broadcast and close, but nothing to maximize.
    expect(actionLabels(paletteGroups("", source({ workspace: oneTab }), all))).toEqual([
      "New tab",
      "Local terminal",
      "Split right",
      "Split down",
      "Split right…",
      "Split down…",
      "Layout: 2 columns",
      "Layout: 2 rows",
      "Layout: 3 columns",
      "Layout: 2×2 grid",
      "Broadcast input: off",
      "Close pane",
      "Go to Hosts",
      "Go to Keychain",
      "Snippets",
      "Themes",
      "New host",
      "Hide sidebar",
      "Lock vault",
    ]);
    expect(actionLabels(paletteGroups("", source({ workspace: splitTab }), all))).toContain("Maximize pane");
    const zoomed = run(splitTab, { type: "toggleZoom" });
    expect(actionLabels(paletteGroups("", source({ workspace: zoomed }), all))).toContain("Restore layout");
    // Matched on the label, ignoring case; the shortcut rides along.
    const g = paletteGroups("LAYOUT: 2×", source({ workspace: oneTab }), all);
    expect(actionIds(g)).toEqual(["preset:grid4"]);
    const split = paletteGroups("split right", source({ workspace: oneTab }), all).find((x) => x.title === "Actions")!;
    expect(split.items[0]).toMatchObject({ id: "splitRight", hint: "⌘D" });
  });

  it("pick_mode_lists_only_hosts_and_local", () => {
    const pick = { kind: "pickHost", orientation: "vertical" } as const;
    const g = paletteGroups("", source({ workspace: splitTab }), pick);
    expect(titles(g)).toEqual(["Hosts", "Actions"]);
    expect(actionLabels(g)).toEqual(["Local terminal"]);
    // The query filters both.
    expect(titles(paletteGroups("db", source(), pick))).toEqual(["Hosts"]);
    expect(titles(paletteGroups("local", source(), pick))).toEqual(["Actions"]);
  });

  it("broadcast_label_follows_state", () => {
    const off = actionLabels(paletteGroups("broadcast", source({ workspace: oneTab }), all));
    expect(off).toEqual(["Broadcast input: off"]);
    const on = run(oneTab, { type: "toggleBroadcast", tabId: "t1" });
    expect(actionLabels(paletteGroups("broadcast", source({ workspace: on }), all))).toEqual(["Broadcast input: on"]);
  });
});
