import { afterEach, describe, expect, it, vi } from "vitest";
import { keyboardHeight, trackVisualViewport } from "./viewport";

type Listener = () => void;

function fakeVisualViewport(height: number, offsetTop = 0) {
  const listeners = new Map<string, Set<Listener>>();
  return {
    height,
    offsetTop,
    addEventListener: (type: string, l: Listener) => {
      if (!listeners.has(type)) listeners.set(type, new Set());
      listeners.get(type)?.add(l);
    },
    removeEventListener: (type: string, l: Listener) => listeners.get(type)?.delete(l),
    fire: (type: string) => listeners.get(type)?.forEach((l) => l()),
  };
}

describe("trackVisualViewport", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
    document.documentElement.style.removeProperty("--app-height");
    document.documentElement.removeAttribute("data-soft-keyboard");
  });

  it("is a no-op without visualViewport", () => {
    vi.stubGlobal("visualViewport", null);
    expect(trackVisualViewport()()).toBeUndefined();
    expect(document.documentElement.style.getPropertyValue("--app-height")).toBe("");
  });

  it("mirrors the visual viewport height and cleans up", () => {
    const vv = fakeVisualViewport(800);
    vi.stubGlobal("visualViewport", vv);
    const stop = trackVisualViewport();
    expect(document.documentElement.style.getPropertyValue("--app-height")).toBe("800px");

    vv.height = 500; // keyboard opened
    vv.fire("resize");
    expect(document.documentElement.style.getPropertyValue("--app-height")).toBe("500px");

    stop();
    expect(document.documentElement.style.getPropertyValue("--app-height")).toBe("");
    vv.height = 300;
    vv.fire("resize");
    expect(document.documentElement.style.getPropertyValue("--app-height")).toBe("");
  });

  it("marks_the_root_while_a_software_keyboard_is_up", () => {
    const vv = fakeVisualViewport(window.innerHeight);
    vi.stubGlobal("visualViewport", vv);
    const root = document.documentElement;
    const stop = trackVisualViewport();
    expect(root.hasAttribute("data-soft-keyboard")).toBe(false);
    expect(root.style.getPropertyValue("--keyboard-height")).toBe("0px");

    vv.height = window.innerHeight - 300;
    vv.fire("resize");
    expect(root.hasAttribute("data-soft-keyboard")).toBe(true);
    expect(root.style.getPropertyValue("--keyboard-height")).toBe("300px");

    // A hardware keyboard's shortcut strip is not a software keyboard.
    vv.height = window.innerHeight - 70;
    vv.fire("resize");
    expect(root.hasAttribute("data-soft-keyboard")).toBe(false);

    stop();
    expect(root.style.getPropertyValue("--keyboard-height")).toBe("");
  });
});

describe("keyboardHeight", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("is_what_the_visual_viewport_lost_and_never_negative", () => {
    vi.stubGlobal("visualViewport", fakeVisualViewport(window.innerHeight - 250));
    expect(keyboardHeight()).toBe(250);
    vi.stubGlobal("visualViewport", fakeVisualViewport(window.innerHeight + 10));
    expect(keyboardHeight()).toBe(0);
    vi.stubGlobal("visualViewport", null);
    expect(keyboardHeight()).toBe(0);
  });
});

describe("native IME height (Android)", () => {
  afterEach(() => {
    delete window.__TETHER_IME_PX__;
    vi.unstubAllGlobals();
  });

  it("keyboard_and_app_height_come_from_native_code_and_follow_its_event", () => {
    // The WebView never resizes (adjustNothing): only native code knows the IME.
    vi.stubGlobal("visualViewport", null);
    vi.stubGlobal("innerHeight", 900);
    window.__TETHER_IME_PX__ = 300;
    expect(keyboardHeight()).toBe(300);

    const root = document.documentElement;
    const stop = trackVisualViewport();
    expect(root.style.getPropertyValue("--app-height")).toBe("600px");
    expect(root.style.getPropertyValue("--keyboard-height")).toBe("300px");
    expect(root.hasAttribute("data-soft-keyboard")).toBe(true);

    window.__TETHER_IME_PX__ = 0;
    window.dispatchEvent(new Event("tether-ime"));
    expect(root.style.getPropertyValue("--app-height")).toBe("900px");
    expect(root.style.getPropertyValue("--keyboard-height")).toBe("0px");
    expect(root.hasAttribute("data-soft-keyboard")).toBe(false);

    stop();
    expect(root.style.getPropertyValue("--app-height")).toBe("");
  });
});
