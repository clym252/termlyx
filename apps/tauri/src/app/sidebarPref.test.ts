import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const KEY = "tether.sidebar";

/** A fresh module (the pref caches what it loaded) over the current localStorage. */
async function freshPref() {
  vi.resetModules();
  return import("./sidebarPref");
}

beforeEach(() => localStorage.clear());
afterEach(() => {
  delete window.__TETHER_OS__;
  vi.unstubAllGlobals();
});

/** Loads as on iOS with this screen (points). */
async function iosPref(width: number, height: number) {
  window.__TETHER_OS__ = "ios";
  vi.stubGlobal("screen", { width, height });
  return freshPref();
}

describe("sidebar mode preference", () => {
  it("defaults to the full sidebar", async () => {
    const { getSidebarMode } = await freshPref();
    expect(getSidebarMode()).toBe("full");
  });

  it("defaults to the rail on an iPad, the full sidebar on an iPhone", async () => {
    expect((await iosPref(820, 1180)).getSidebarMode()).toBe("rail");
    expect((await iosPref(402, 874)).getSidebarMode()).toBe("full");
  });

  it("keeps an iPad's own choice over the rail default", async () => {
    localStorage.setItem(KEY, "full");
    expect((await iosPref(820, 1180)).getSidebarMode()).toBe("full");
  });

  it("persists the rail across loads", async () => {
    const first = await freshPref();
    first.setSidebarMode("rail");
    expect(first.getSidebarMode()).toBe("rail");
    expect(localStorage.getItem(KEY)).toBe("rail");
    const second = await freshPref();
    expect(second.getSidebarMode()).toBe("rail");
    second.setSidebarMode("full");
    const third = await freshPref();
    expect(third.getSidebarMode()).toBe("full");
  });

  it("falls back to the default for a value it does not know", async () => {
    localStorage.setItem(KEY, "huge");
    const { getSidebarMode } = await freshPref();
    expect(getSidebarMode()).toBe("full");
  });

  it("notifies subscribers once per change, not for a repeat", async () => {
    const { setSidebarMode, subscribeSidebarMode } = await freshPref();
    const seen = vi.fn();
    const off = subscribeSidebarMode(seen);
    setSidebarMode("rail");
    setSidebarMode("rail");
    expect(seen).toHaveBeenCalledTimes(1);
    off();
    setSidebarMode("full");
    expect(seen).toHaveBeenCalledTimes(1);
  });

  it("keeps working when storage is unavailable", async () => {
    const { getSidebarMode, setSidebarMode } = await freshPref();
    const spy = vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => {
      throw new Error("denied");
    });
    setSidebarMode("rail");
    expect(getSidebarMode()).toBe("rail");
    spy.mockRestore();
  });

  it("cycles full, rail, hidden, full", async () => {
    const { nextSidebarMode } = await freshPref();
    expect(nextSidebarMode("full")).toBe("rail");
    expect(nextSidebarMode("rail")).toBe("hidden");
    expect(nextSidebarMode("hidden")).toBe("full");
  });

  it("loads a stored hidden as hidden and remembers the visible mode before it", async () => {
    const first = await freshPref();
    first.setSidebarMode("rail");
    first.setSidebarMode("hidden");
    expect(first.getSidebarMode()).toBe("hidden");
    expect(first.lastVisibleSidebarMode()).toBe("rail");
    const second = await freshPref();
    expect(second.getSidebarMode()).toBe("hidden");
    expect(second.lastVisibleSidebarMode()).toBe("rail");
  });

  it("last visible defaults to the platform default when hidden was stored first", async () => {
    localStorage.setItem(KEY, "hidden");
    expect((await freshPref()).lastVisibleSidebarMode()).toBe("full");
    localStorage.setItem(KEY, "hidden");
    window.__TETHER_OS__ = "ios";
    vi.stubGlobal("screen", { width: 820, height: 1180 });
    expect((await freshPref()).lastVisibleSidebarMode()).toBe("rail");
  });
});
