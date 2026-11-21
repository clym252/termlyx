import { afterEach, describe, expect, it, vi } from "vitest";
import { detectFormFactor, detectPlatform, layoutFor, usesCmdKey } from "./platform";

describe("detectPlatform", () => {
  it("takes the backend's OS first", () => {
    expect(detectPlatform("ios", "MacIntel")).toBe("ios");
    expect(detectPlatform("macos", "MacIntel")).toBe("mac");
    expect(detectPlatform("windows", "Win32")).toBe("windows");
    expect(detectPlatform("linux", "Linux x86_64")).toBe("other");
    expect(detectPlatform("macos", "Win32")).toBe("mac");
    expect(detectPlatform("android", "Linux armv81")).toBe("android");
  });

  it("falls back to navigator.platform without one (browser mock)", () => {
    expect(detectPlatform(undefined, "MacIntel")).toBe("mac");
    expect(detectPlatform(undefined, "iPhone")).toBe("ios");
    expect(detectPlatform(undefined, "iPad")).toBe("ios");
    expect(detectPlatform(undefined, "Win32")).toBe("windows");
    expect(detectPlatform(undefined, "Linux x86_64")).toBe("other");
    expect(detectPlatform(undefined, "")).toBe("other");
  });

  it("matches Windows only at the start", () => {
    expect(detectPlatform(undefined, "Darwin")).toBe("other");
    expect(detectPlatform(undefined, "xWin32")).toBe("other");
  });
});

describe("detectFormFactor", () => {
  it("is phone or tablet on iOS by the screen's short side", () => {
    expect(detectFormFactor("ios", 393)).toBe("phone");
    expect(detectFormFactor("ios", 599)).toBe("phone");
    expect(detectFormFactor("ios", 600)).toBe("tablet");
    expect(detectFormFactor("ios", 744)).toBe("tablet");
    expect(detectFormFactor("ios", 820)).toBe("tablet");
  });

  it("is phone or tablet on Android the same way", () => {
    expect(detectFormFactor("android", 412)).toBe("phone");
    expect(detectFormFactor("android", 800)).toBe("tablet");
    expect(layoutFor(detectFormFactor("android", 800), 500)).toBe("phone");
  });

  it("is a desktop everywhere else, however small", () => {
    for (const p of ["mac", "windows", "other"] as const) {
      expect(detectFormFactor(p, 393)).toBe("desktop");
    }
  });
});

describe("layoutFor", () => {
  it("gives a narrow iPad (Slide Over, narrow Split View) the phone layout", () => {
    expect(layoutFor("tablet", 507)).toBe("phone");
    expect(layoutFor("tablet", 600)).toBe("tablet");
    expect(layoutFor("tablet", 1180)).toBe("tablet");
  });

  it("never gives a desktop the phone layout", () => {
    expect(layoutFor("desktop", 400)).toBe("desktop");
    expect(layoutFor("phone", 932)).toBe("phone");
  });
});

describe("usesCmdKey", () => {
  it("uses ⌘ on macOS and iOS", () => {
    expect(usesCmdKey("mac")).toBe(true);
    expect(usesCmdKey("ios")).toBe(true);
    expect(usesCmdKey("android")).toBe(false);
    expect(usesCmdKey("windows")).toBe(false);
    expect(usesCmdKey("other")).toBe(false);
  });
});

describe("platform constants", () => {
  afterEach(() => {
    delete window.__TETHER_OS__;
    vi.unstubAllGlobals();
    vi.resetModules();
  });

  it("follow the OS the backend set before load and the screen size", async () => {
    window.__TETHER_OS__ = "ios";
    vi.stubGlobal("screen", { width: 820, height: 1180 });
    vi.resetModules();
    const p = await import("./platform");
    expect(p.PLATFORM).toBe("ios");
    expect(p.IS_IOS).toBe(true);
    expect(p.IS_WINDOWS).toBe(false);
    expect(p.CMD_KEYS).toBe(true);
    expect(p.FORM_FACTOR).toBe("tablet");
  });

  it("name Android a mobile phone or tablet, with Ctrl", async () => {
    window.__TETHER_OS__ = "android";
    vi.stubGlobal("screen", { width: 412, height: 915 });
    vi.resetModules();
    const p = await import("./platform");
    expect(p.PLATFORM).toBe("android");
    expect([p.IS_ANDROID, p.IS_IOS, p.IS_MOBILE, p.CMD_KEYS]).toEqual([true, false, true, false]);
    expect([p.FORM_FACTOR, p.DEVICE_NOUN]).toEqual(["phone", "phone"]);
    vi.stubGlobal("screen", { width: 1280, height: 800 });
    vi.resetModules();
    const t = await import("./platform");
    expect([t.FORM_FACTOR, t.DEVICE_NOUN]).toEqual(["tablet", "tablet"]);
  });

  it("name the device iPhone, iPad or computer elsewhere", async () => {
    window.__TETHER_OS__ = "ios";
    vi.stubGlobal("screen", { width: 393, height: 852 });
    vi.resetModules();
    expect((await import("./platform")).DEVICE_NOUN).toBe("iPhone");
    vi.stubGlobal("screen", { width: 820, height: 1180 });
    vi.resetModules();
    expect((await import("./platform")).DEVICE_NOUN).toBe("iPad");
    window.__TETHER_OS__ = "macos";
    vi.resetModules();
    const m = await import("./platform");
    expect([m.DEVICE_NOUN, m.IS_MOBILE]).toEqual(["computer", false]);
  });

  it("an iPhone in landscape is still a phone", async () => {
    window.__TETHER_OS__ = "ios";
    vi.stubGlobal("screen", { width: 874, height: 402 });
    vi.resetModules();
    const p = await import("./platform");
    expect(p.FORM_FACTOR).toBe("phone");
  });
});
