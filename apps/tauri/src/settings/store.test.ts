import { beforeEach, describe, expect, it, vi } from "vitest";

const KEY = "tether.terminal-settings";

/** A fresh module (the store caches what it loaded) over the current localStorage. */
async function freshStore() {
  vi.resetModules();
  return import("./store");
}

beforeEach(() => localStorage.clear());

describe("terminal settings: theme", () => {
  it("falls back to Tether Dark for a removed theme id", async () => {
    localStorage.setItem(KEY, JSON.stringify({ themeId: "solarized-dark" }));
    const { getTerminalSettings } = await freshStore();
    expect(getTerminalSettings().themeId).toBe("tether-dark");
  });
});

describe("terminal settings: IP highlighting", () => {
  it("is on by default and round-trips through storage", async () => {
    const first = await freshStore();
    expect(first.getTerminalSettings().highlightIps).toBe(true);
    first.updateTerminalSettings({ highlightIps: false });
    const second = await freshStore();
    expect(second.getTerminalSettings().highlightIps).toBe(false);
  });

  it("ignores a non-boolean value on load", async () => {
    localStorage.setItem(KEY, JSON.stringify({ highlightIps: "no" }));
    const { getTerminalSettings } = await freshStore();
    expect(getTerminalSettings().highlightIps).toBe(true);
  });
});

describe("terminal settings: local shell", () => {
  it("defaults to no chosen shell", async () => {
    const { getTerminalSettings } = await freshStore();
    expect(getTerminalSettings().localShellId).toBeNull();
  });

  it("round-trips the chosen shell through storage", async () => {
    const first = await freshStore();
    first.updateTerminalSettings({ localShellId: "wsl:Ubuntu" });
    expect(first.getTerminalSettings().localShellId).toBe("wsl:Ubuntu");
    const second = await freshStore();
    expect(second.getTerminalSettings().localShellId).toBe("wsl:Ubuntu");
    second.updateTerminalSettings({ localShellId: null });
    const third = await freshStore();
    expect(third.getTerminalSettings().localShellId).toBeNull();
  });

  it("ignores a non-string shell id, on load and on update", async () => {
    localStorage.setItem(KEY, JSON.stringify({ fontSize: 14, localShellId: 7 }));
    const store = await freshStore();
    expect(store.getTerminalSettings().localShellId).toBeNull();
    expect(store.getTerminalSettings().fontSize).toBe(14);
    store.updateTerminalSettings({ localShellId: "cmd" });
    store.updateTerminalSettings({ localShellId: 7 as unknown as string });
    expect(store.getTerminalSettings().localShellId).toBe("cmd");
  });

  it("keeps settings saved before the shell picker existed", async () => {
    localStorage.setItem(KEY, JSON.stringify({ themeId: "tether-dark", fontFamily: "Fira Code", fontSize: 15 }));
    const store = await freshStore();
    expect(store.getTerminalSettings()).toMatchObject({ fontFamily: "Fira Code", fontSize: 15, localShellId: null });
  });

  it("moves the old default font stack to the new default", async () => {
    const old = 'Menlo, Monaco, "Cascadia Mono", Consolas, "Courier New", monospace';
    localStorage.setItem(KEY, JSON.stringify({ fontFamily: old }));
    const store = await freshStore();
    expect(store.getTerminalSettings().fontFamily).toBe('"JetBrains Mono", monospace');
    localStorage.setItem(KEY, JSON.stringify({ fontFamily: "Menlo, monospace" }));
    expect((await freshStore()).getTerminalSettings().fontFamily).toBe("Menlo, monospace");
  });
});
