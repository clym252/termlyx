import { beforeEach, describe, expect, it, vi } from "vitest";

const KEY = "tether.side-panel";

async function fresh() {
  vi.resetModules();
  return import("./sidePanelPref");
}

beforeEach(() => localStorage.clear());

describe("side panel preference", () => {
  it("starts closed on Snippets", async () => {
    const { getSidePanel } = await fresh();
    expect(getSidePanel()).toEqual({ open: false, tab: "snippets" });
  });

  it("persists what is set", async () => {
    const first = await fresh();
    first.setSidePanel({ open: true, tab: "themes" });
    const second = await fresh();
    expect(second.getSidePanel()).toEqual({ open: true, tab: "themes" });
  });

  it("ignores bad stored values", async () => {
    localStorage.setItem(KEY, "{nope");
    expect((await fresh()).getSidePanel()).toEqual({ open: false, tab: "snippets" });
    localStorage.setItem(KEY, JSON.stringify({ open: "yes", tab: "colours" }));
    expect((await fresh()).getSidePanel()).toEqual({ open: false, tab: "snippets" });
  });
});
