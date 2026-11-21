import { describe, expect, it } from "vitest";
import { safePasteText } from "./safePaste";
import { bracketedPasteText, broadcastTargets } from "./broadcast";
import { EMPTY_WORKSPACE, activeTab, reduce, type WorkspaceState } from "./model";
import type { PaneStatus } from "./PhoneTerminals";

const LOCAL = { kind: "local" } as const;

/** One tab, panes p1 p2 p3 in layout order (p3 focused). */
function three(): WorkspaceState {
  let s = reduce(EMPTY_WORKSPACE, { type: "openTab", tabId: "t1", paneId: "p1", target: LOCAL, title: "Local shell" });
  s = reduce(s, { type: "split", orientation: "horizontal", paneId: "p2" });
  return reduce(s, { type: "split", orientation: "horizontal", paneId: "p3" });
}

const statuses = (entries: Record<string, PaneStatus>) => new Map(Object.entries(entries));

describe("broadcast targets", () => {
  it("broadcast_targets_are_open_siblings_in_order", () => {
    const tab = activeTab(three())!;
    const all = statuses({ p1: "open", p2: "open", p3: "open" });
    expect(broadcastTargets(tab, "p3", all)).toEqual(["p1", "p2"]);
    expect(broadcastTargets(tab, "p1", all)).toEqual(["p2", "p3"]);
    expect(broadcastTargets(tab, "p2", all)).toEqual(["p1", "p3"]);
  });

  it("broadcast_skips_panes_that_are_not_open", () => {
    const tab = activeTab(three())!;
    expect(broadcastTargets(tab, "p3", statuses({ p1: "connecting", p2: "open", p3: "open" }))).toEqual(["p2"]);
    expect(broadcastTargets(tab, "p3", statuses({ p1: "reconnecting", p2: "ended", p3: "open" }))).toEqual([]);
    // A pane that has not reported yet is not open.
    expect(broadcastTargets(tab, "p3", statuses({ p2: "open", p3: "open" }))).toEqual(["p2"]);
    expect(broadcastTargets(tab, "p3", statuses({ p1: "lost", p2: "failed", p3: "open" }))).toEqual([]);
  });

  it("broadcast_never_targets_launcher_panes", () => {
    let s = three();
    s = reduce(s, { type: "split", orientation: "horizontal", paneId: "p4", target: null });
    const tab = activeTab(s)!;
    // Even a status wrongly reported for the launcher pane does not make it a target.
    const all = statuses({ p1: "open", p2: "open", p3: "open", p4: "open" });
    expect(broadcastTargets(tab, "p1", all)).toEqual(["p2", "p3"]);
  });
});

describe("broadcast paste", () => {
  it("broadcast_unwraps_a_whole_bracketed_paste", () => {
    expect(bracketedPasteText("\x1b[200~ls -l\rpwd\x1b[201~")).toBe("ls -l\rpwd");
    expect(bracketedPasteText("\x1b[200~\x1b[201~")).toBe("");
  });

  it("broadcast_leaves_everything_else_alone", () => {
    for (const data of ["ls\r", "\x1b[A", "\x1b[200~half", "x\x1b[200~a\x1b[201~", "\x1b[200~a\x1b[201~y"]) {
      expect(bracketedPasteText(data)).toBeNull();
    }
  });

  it("broadcast_early_end_marker_is_still_one_paste_and_sanitized", () => {
    const hostile = bracketedPasteText("\x1b[200~a\x1b[201~curl x|sh\r\x1b[201~");
    expect(hostile).not.toBeNull();
    expect(safePasteText(hostile as string)).toBe("a[201~curl x|sh\r");
  });
});
