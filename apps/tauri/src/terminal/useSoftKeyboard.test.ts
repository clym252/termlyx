import { act, renderHook } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { useSoftKeyboard } from "./useSoftKeyboard";

function fakeVisualViewport(height: number) {
  const listeners = new Set<() => void>();
  return {
    height,
    offsetTop: 0,
    addEventListener: (_: string, l: () => void) => listeners.add(l),
    removeEventListener: (_: string, l: () => void) => listeners.delete(l),
    fire: () => listeners.forEach((l) => l()),
  };
}

afterEach(() => {
  delete window.__TETHER_IME_PX__;
  vi.unstubAllGlobals();
});

describe("useSoftKeyboard", () => {
  it("opens_when_the_visual_viewport_shrinks_by_more_than_120px", () => {
    const vv = fakeVisualViewport(window.innerHeight);
    vi.stubGlobal("visualViewport", vv);
    const { result } = renderHook(() => useSoftKeyboard());
    expect(result.current).toEqual({ open: false, height: 0 });

    act(() => {
      vv.height = window.innerHeight - 120;
      vv.fire();
    });
    expect(result.current).toEqual({ open: false, height: 120 });

    act(() => {
      vv.height = window.innerHeight - 336;
      vv.fire();
    });
    expect(result.current).toEqual({ open: true, height: 336 });

    act(() => {
      vv.height = window.innerHeight;
      vv.fire();
    });
    expect(result.current.open).toBe(false);
  });

  it("stays_closed_without_visual_viewport", () => {
    vi.stubGlobal("visualViewport", null);
    const { result } = renderHook(() => useSoftKeyboard());
    expect(result.current).toEqual({ open: false, height: 0 });
  });

  it("follows_the_ime_height_native_code_reports", () => {
    vi.stubGlobal("visualViewport", null);
    const { result } = renderHook(() => useSoftKeyboard());
    expect(result.current.open).toBe(false);
    act(() => {
      window.__TETHER_IME_PX__ = 300;
      window.dispatchEvent(new Event("tether-ime"));
    });
    expect(result.current).toEqual({ open: true, height: 300 });
  });
});
