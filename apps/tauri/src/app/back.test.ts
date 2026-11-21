import { describe, expect, it } from "vitest";
import { backAction, closeMenu, closeTopDialog, pressBack, setBackHandler, type BackState } from "./back";

const idle: BackState = { menuOpen: false, dialogOpen: false, panelOpen: false, drawerOpen: false, terminalShown: false, route: "hosts" };
const at = (s: Partial<BackState>) => backAction({ ...idle, ...s });

describe("backAction", () => {
  it("a_popup_menu_closes_before_everything_else", () => {
    expect(at({ menuOpen: true, dialogOpen: true, panelOpen: true, drawerOpen: true, terminalShown: true })).toBe("closeMenu");
    expect(at({ menuOpen: true })).toBe("closeMenu");
  });

  it("a_dialog_beats_everything", () => {
    expect(at({ dialogOpen: true, panelOpen: true, drawerOpen: true, terminalShown: true, route: "settings" })).toBe("closeDialog");
  });

  it("a_dialog_over_a_connecting_terminal_still_closes_first", () => {
    expect(at({ dialogOpen: true, terminalShown: true })).toBe("closeDialog");
  });

  it("the_docked_panel_beats_the_drawer_and_the_terminal", () => {
    expect(at({ panelOpen: true, drawerOpen: true, terminalShown: true })).toBe("closePanel");
  });

  it("the_drawer_beats_the_terminal", () => {
    expect(at({ drawerOpen: true, terminalShown: true })).toBe("closeDrawer");
  });

  it("a_terminal_goes_to_hosts", () => {
    expect(at({ terminalShown: true })).toBe("toHosts");
  });

  it("another_page_goes_to_hosts", () => {
    expect(at({ route: "settings" })).toBe("toHosts");
    expect(at({ route: "/settings" })).toBe("toHosts");
  });

  it("hosts_with_nothing_open_goes_to_the_background", () => {
    expect(at({})).toBe("background");
  });
});

describe("closeTopDialog", () => {
  it("sends_cancel_to_the_last_open_dialog_only", () => {
    document.body.innerHTML = '<dialog open id="a"></dialog><dialog id="shut"></dialog><dialog open id="b"></dialog>';
    const got: string[] = [];
    for (const el of document.querySelectorAll("dialog")) el.addEventListener("cancel", () => got.push(el.id));
    closeTopDialog();
    expect(got).toEqual(["b"]);
  });
});

describe("pressBack", () => {
  it("runs_the_apps_step_else_goes_to_the_background", () => {
    const calls: string[] = [];
    pressBack(() => calls.push("background"));
    setBackHandler(() => calls.push("step"));
    pressBack(() => calls.push("background"));
    setBackHandler(null);
    pressBack(() => calls.push("background"));
    expect(calls).toEqual(["background", "step", "background"]);
  });
});

describe("closeMenu", () => {
  it("sends_escape_to_window", () => {
    const keys: string[] = [];
    const on = (e: KeyboardEvent) => keys.push(e.key);
    window.addEventListener("keydown", on);
    closeMenu();
    window.removeEventListener("keydown", on);
    expect(keys).toEqual(["Escape"]);
  });
});
