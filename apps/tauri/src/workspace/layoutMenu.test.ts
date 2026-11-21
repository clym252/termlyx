import { describe, expect, it, vi } from "vitest";
import { layoutMenu, type LayoutMenuActions, type LayoutMenuContext } from "./layoutMenu";
import { shortcutLabels } from "./shortcuts";

const ctx: LayoutMenuContext = { hasRoot: true, paneCount: 2, zoomed: false, broadcast: false, labels: shortcutLabels(true) };
const act = (): LayoutMenuActions => ({
  split: vi.fn(),
  splitPick: vi.fn(),
  preset: vi.fn(),
  zoom: vi.fn(),
  broadcast: vi.fn(),
  moveToTab: vi.fn(),
  close: vi.fn(),
});

type Entry = Exclude<ReturnType<typeof layoutMenu>[number], "separator" | [string, () => void]>;
/** The menu as `label` (or "|" for a separator), with disabled entries marked. */
const shape = (items: ReturnType<typeof layoutMenu>) =>
  items.map((e) => (e === "separator" ? "|" : Array.isArray(e) ? e[0] : `${e.label}${e.disabled ? " (off)" : ""}`));
const entry = (items: ReturnType<typeof layoutMenu>, label: string) =>
  items.find((e): e is Entry => typeof e === "object" && !Array.isArray(e) && e.label === label)!;

describe("layoutMenu", () => {
  it("tab_scope_lists_presets_and_no_move", () => {
    expect(shape(layoutMenu(ctx, act(), "tab"))).toEqual([
      "Split right",
      "Split down",
      "Split right…",
      "Split down…",
      "|",
      "2 columns",
      "2 rows",
      "3 columns",
      "2×2 grid",
      "|",
      "Maximize pane",
      "Broadcast input",
      "|",
      "Close pane",
    ]);
  });

  it("pane_scope_has_move_and_no_presets", () => {
    expect(shape(layoutMenu(ctx, act(), "pane"))).toEqual([
      "Split right",
      "Split down",
      "Split right…",
      "Split down…",
      "|",
      "Maximize pane",
      "Broadcast input",
      "Move to new tab",
      "|",
      "Close pane",
    ]);
  });

  it("single_pane_disables_maximize_and_move", () => {
    const items = layoutMenu({ ...ctx, paneCount: 1 }, act(), "pane");
    expect(shape(items).filter((s) => s.endsWith("(off)"))).toEqual(["Maximize pane (off)", "Move to new tab (off)"]);
    expect(entry(items, "Split right").disabled).toBeFalsy();
  });

  it("launcher_tab_enables_only_presets", () => {
    const a = act();
    const items = layoutMenu({ ...ctx, hasRoot: false, paneCount: 0 }, a, "tab");
    const on = items.flatMap((e) => (typeof e === "object" && !Array.isArray(e) && !e.disabled ? [e.label] : []));
    expect(on).toEqual(["2 columns", "2 rows", "3 columns", "2×2 grid"]);
    entry(items, "2×2 grid").run();
    expect(a.preset).toHaveBeenCalledWith("grid4");
  });

  it("broadcast_entry_is_checked_when_on", () => {
    expect(entry(layoutMenu(ctx, act(), "tab"), "Broadcast input").checked).toBe(false);
    expect(entry(layoutMenu({ ...ctx, broadcast: true }, act(), "tab"), "Broadcast input").checked).toBe(true);
  });

  it("zoomed_swaps_maximize_for_restore_and_entries_run_their_actions", () => {
    const a = act();
    const items = layoutMenu({ ...ctx, zoomed: true }, a, "pane");
    expect(shape(items)).toContain("Restore layout");
    entry(items, "Restore layout").run();
    entry(items, "Split down…").run();
    entry(items, "Split right").run();
    entry(items, "Move to new tab").run();
    entry(items, "Close pane").run();
    expect(a.zoom).toHaveBeenCalled();
    expect(a.splitPick).toHaveBeenCalledWith("vertical");
    expect(a.split).toHaveBeenCalledWith("horizontal");
    expect(a.moveToTab).toHaveBeenCalled();
    expect(a.close).toHaveBeenCalled();
  });

  it("hints_show_the_platform_shortcuts", () => {
    const items = layoutMenu(ctx, act(), "pane");
    expect(entry(items, "Maximize pane").hint).toBe("⇧⌘↩");
    expect(entry(items, "Broadcast input").hint).toBe("⌥⌘B");
    expect(entry(items, "Split right").hint).toBe("⌘D");
    expect(entry(items, "Split right…").hint).toBeUndefined();
  });
});
