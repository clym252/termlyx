import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const KEY = "tether.auto-lock";

/** A fresh module (the pref caches what it loaded) over the current localStorage. */
async function freshPref() {
  vi.resetModules();
  return import("./autoLockPref");
}

beforeEach(() => localStorage.clear());
afterEach(() => {
  delete window.__TETHER_OS__;
});

describe("auto-lock preference", () => {
  it("offers never, 5, 15, 30 and 60 minutes, with their labels", async () => {
    const { AUTO_LOCK_CHOICES, autoLockLabel } = await freshPref();
    expect(AUTO_LOCK_CHOICES).toEqual([null, 5, 15, 30, 60]);
    expect(AUTO_LOCK_CHOICES.map(autoLockLabel)).toEqual([
      "Never",
      "After 5 minutes",
      "After 15 minutes",
      "After 30 minutes",
      "After 1 hour",
    ]);
  });

  it("defaults to never on the desktop", async () => {
    const { getAutoLock } = await freshPref();
    expect(getAutoLock()).toBeNull();
  });

  it("defaults to 30 minutes on iOS, keeping a stored choice", async () => {
    window.__TETHER_OS__ = "ios";
    expect((await freshPref()).getAutoLock()).toBe(30);
    localStorage.setItem(KEY, "never");
    expect((await freshPref()).getAutoLock()).toBeNull();
  });

  it("defaults to 30 minutes on Android", async () => {
    window.__TETHER_OS__ = "android";
    expect((await freshPref()).getAutoLock()).toBe(30);
  });

  it("persists the choice across loads", async () => {
    const first = await freshPref();
    first.setAutoLockPref(30);
    expect(first.getAutoLock()).toBe(30);
    expect(localStorage.getItem(KEY)).toBe("30");
    const second = await freshPref();
    expect(second.getAutoLock()).toBe(30);
    second.setAutoLockPref(null);
    expect(localStorage.getItem(KEY)).toBe("never");
    const third = await freshPref();
    expect(third.getAutoLock()).toBeNull();
  });

  it("falls back to the default for a value it does not know", async () => {
    for (const bad of ["7", "0", "-5", "soon", "", "30.5"]) {
      localStorage.setItem(KEY, bad);
      const { getAutoLock } = await freshPref();
      expect(getAutoLock(), bad).toBeNull();
    }
  });

  it("refuses a value that is not a choice", async () => {
    const { getAutoLock, setAutoLockPref } = await freshPref();
    setAutoLockPref(7 as never);
    expect(getAutoLock()).toBeNull();
    expect(localStorage.getItem(KEY)).toBeNull();
  });

  it("notifies subscribers once per change", async () => {
    const { setAutoLockPref, subscribeAutoLock } = await freshPref();
    const seen = vi.fn();
    const off = subscribeAutoLock(seen);
    setAutoLockPref(5);
    setAutoLockPref(5);
    expect(seen).toHaveBeenCalledTimes(1);
    off();
    setAutoLockPref(15);
    expect(seen).toHaveBeenCalledTimes(1);
  });

  it("keeps working when storage is unavailable", async () => {
    const { getAutoLock, setAutoLockPref } = await freshPref();
    const spy = vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => {
      throw new Error("denied");
    });
    setAutoLockPref(60);
    expect(getAutoLock()).toBe(60);
    spy.mockRestore();
  });

  it("applies the choice, trying once more before it gives up", async () => {
    const { applyAutoLock, getAutoLockError, setAutoLockPref, subscribeAutoLock } = await freshPref();
    setAutoLockPref(30);
    const seen = vi.fn();
    subscribeAutoLock(seen);
    const flaky = vi.fn().mockRejectedValueOnce(new Error("busy")).mockResolvedValue(undefined);
    expect(await applyAutoLock(flaky)).toBe(true);
    expect(flaky.mock.calls).toEqual([[30], [30]]);
    expect(getAutoLockError()).toBeNull();

    const down = vi.fn().mockRejectedValue({ code: "internal", message: "no backend", refs: [] });
    expect(await applyAutoLock(down)).toBe(false);
    expect(down).toHaveBeenCalledTimes(2);
    expect(getAutoLockError()).toBe("no backend");
    expect(seen).toHaveBeenCalled();

    // A later success clears it.
    expect(await applyAutoLock(vi.fn().mockResolvedValue(undefined))).toBe(true);
    expect(getAutoLockError()).toBeNull();
  });
});
