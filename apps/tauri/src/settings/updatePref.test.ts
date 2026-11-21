import { beforeEach, describe, expect, it, vi } from "vitest";

const KEY = "tether.update-check";

/** A fresh module (the pref caches what it loaded) over the current localStorage. */
async function freshPref() {
  vi.resetModules();
  return import("./updatePref");
}

beforeEach(() => localStorage.clear());

describe("automatic update check preference", () => {
  it("is on by default", async () => {
    expect((await freshPref()).getAutoCheck()).toBe(true);
  });

  it("persists off across loads", async () => {
    const first = await freshPref();
    first.setAutoCheck(false);
    expect(first.getAutoCheck()).toBe(false);
    expect((await freshPref()).getAutoCheck()).toBe(false);
    (await freshPref()).setAutoCheck(true);
    expect((await freshPref()).getAutoCheck()).toBe(true);
  });

  it("reads anything unknown as on", async () => {
    for (const junk of ["", "no", "0", "False", "{}"]) {
      localStorage.setItem(KEY, junk);
      expect((await freshPref()).getAutoCheck(), junk).toBe(true);
    }
  });
});
