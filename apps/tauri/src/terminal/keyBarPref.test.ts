import { beforeEach, describe, expect, it, vi } from "vitest";

const KEY = "tether.keyBar";

/** A fresh module (the pref caches what it loaded) over the current localStorage. */
async function freshPref() {
  vi.resetModules();
  return import("./keyBarPref");
}

beforeEach(() => localStorage.clear());

describe("key bar mode preference", () => {
  it("defaults to two rows", async () => {
    const { getKeyBarMode } = await freshPref();
    expect(getKeyBarMode()).toBe("two");
  });

  it("cycles two rows, one row, hidden, two rows", async () => {
    const { nextKeyBarMode } = await freshPref();
    expect(nextKeyBarMode("two")).toBe("one");
    expect(nextKeyBarMode("one")).toBe("hidden");
    expect(nextKeyBarMode("hidden")).toBe("two");
  });

  it("persists the choice across loads", async () => {
    const first = await freshPref();
    first.setKeyBarMode("one");
    expect(localStorage.getItem(KEY)).toBe("one");
    const second = await freshPref();
    expect(second.getKeyBarMode()).toBe("one");
    second.setKeyBarMode("hidden");
    expect((await freshPref()).getKeyBarMode()).toBe("hidden");
  });

  it("falls back to two rows on an unknown stored value", async () => {
    localStorage.setItem(KEY, "three");
    expect((await freshPref()).getKeyBarMode()).toBe("two");
  });

  it("ignores a value that is not a mode", async () => {
    const pref = await freshPref();
    pref.setKeyBarMode("big" as never);
    expect(pref.getKeyBarMode()).toBe("two");
    expect(localStorage.getItem(KEY)).toBeNull();
  });

  it("notifies subscribers on a change only", async () => {
    const pref = await freshPref();
    const listener = vi.fn();
    const off = pref.subscribeKeyBarMode(listener);
    pref.setKeyBarMode("two");
    expect(listener).not.toHaveBeenCalled();
    pref.setKeyBarMode("hidden");
    expect(listener).toHaveBeenCalledTimes(1);
    off();
    pref.setKeyBarMode("one");
    expect(listener).toHaveBeenCalledTimes(1);
  });
});
