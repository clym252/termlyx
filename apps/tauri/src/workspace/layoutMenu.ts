import type { MenuEntry } from "../ui/RowMenu";
import { PRESETS, type Orientation, type Preset } from "./model";
import type { ShortcutLabels } from "./shortcuts";

export interface LayoutMenuContext {
  /** The tab has panes (a launcher tab has none, so only presets apply). */
  hasRoot: boolean;
  paneCount: number;
  zoomed: boolean;
  broadcast: boolean;
  labels: ShortcutLabels;
}

export interface LayoutMenuActions {
  /** Splits the active pane, the new one on the same target. */
  split(o: Orientation): void;
  /** Splits the active pane and asks which host the new one opens. */
  splitPick(o: Orientation): void;
  preset(p: Preset): void;
  zoom(): void;
  broadcast(): void;
  moveToTab(): void;
  close(): void;
}

/**
 * The layout menu of the tab bar ("tab": adds the presets) and of a pane
 * ("pane": adds Move to new tab). Both act on the active pane; a pane's menu
 * activates it first.
 */
export function layoutMenu(ctx: LayoutMenuContext, act: LayoutMenuActions, scope: "tab" | "pane"): MenuEntry[] {
  const { hasRoot, paneCount, labels } = ctx;
  const off = !hasRoot;
  const items: MenuEntry[] = [
    { label: "Split right", run: () => act.split("horizontal"), disabled: off, hint: labels.splitRight },
    { label: "Split down", run: () => act.split("vertical"), disabled: off, hint: labels.splitDown },
    { label: "Split right…", run: () => act.splitPick("horizontal"), disabled: off },
    { label: "Split down…", run: () => act.splitPick("vertical"), disabled: off },
    "separator",
  ];
  if (scope === "tab") {
    for (const p of PRESETS) items.push({ label: p.label, run: () => act.preset(p.id) });
    items.push("separator");
  }
  items.push(
    {
      label: ctx.zoomed ? "Restore layout" : "Maximize pane",
      run: act.zoom,
      disabled: paneCount < 2,
      hint: labels.zoom,
    },
    { label: "Broadcast input", run: act.broadcast, disabled: off, checked: ctx.broadcast, hint: labels.broadcast },
  );
  if (scope === "pane") items.push({ label: "Move to new tab", run: act.moveToTab, disabled: paneCount < 2 });
  items.push("separator", { label: "Close pane", run: act.close, disabled: off, hint: labels.close });
  return items;
}
