import { act, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { CLICK_GRACE_MS, LONG_PRESS_MS, useLongPressLabel } from "./useLongPressLabel";

/** The bits of a React pointer/click event the handlers read. */
function pointer(pointerType: string) {
  return {
    pointerType,
    currentTarget: { getBoundingClientRect: () => ({ right: 48, top: 100, height: 44 }) },
  } as never;
}
const click = () => ({ preventDefault: vi.fn(), stopPropagation: vi.fn() });

beforeEach(() => vi.useFakeTimers());
afterEach(() => vi.useRealTimers());

describe("useLongPressLabel", () => {
  it("shows the name beside the icon after a half-second touch", () => {
    const { result } = renderHook(() => useLongPressLabel());
    const h = result.current.bind("Keychain");
    act(() => h.onPointerDown(pointer("touch")));
    act(() => void vi.advanceTimersByTime(LONG_PRESS_MS - 1));
    expect(result.current.label).toBeNull();
    act(() => void vi.advanceTimersByTime(1));
    expect(result.current.label).toEqual({ name: "Keychain", x: 56, y: 122 });
    act(() => h.onPointerUp());
    expect(result.current.label).toBeNull();
  });

  it("does nothing for a tap, a mouse or a lifted finger", () => {
    const { result } = renderHook(() => useLongPressLabel());
    const h = result.current.bind("Keychain");
    act(() => h.onPointerDown(pointer("touch")));
    act(() => h.onPointerUp());
    act(() => void vi.advanceTimersByTime(LONG_PRESS_MS * 2));
    expect(result.current.label).toBeNull();
    act(() => h.onPointerDown(pointer("mouse")));
    act(() => void vi.advanceTimersByTime(LONG_PRESS_MS * 2));
    expect(result.current.label).toBeNull();
    const e = click();
    h.onClickCapture(e as never);
    expect(e.preventDefault).not.toHaveBeenCalled();
  });

  it("swallows the click that ends a long press, once", () => {
    const { result } = renderHook(() => useLongPressLabel());
    const h = result.current.bind("Keychain");
    act(() => h.onPointerDown(pointer("touch")));
    act(() => void vi.advanceTimersByTime(LONG_PRESS_MS));
    act(() => h.onPointerUp());
    const first = click();
    h.onClickCapture(first as never);
    expect(first.preventDefault).toHaveBeenCalled();
    expect(first.stopPropagation).toHaveBeenCalled();
    const second = click();
    h.onClickCapture(second as never);
    expect(second.preventDefault).not.toHaveBeenCalled();
  });

  it("does not swallow a later click when the press ended without one", () => {
    const { result } = renderHook(() => useLongPressLabel());
    const h = result.current.bind("Keychain");
    act(() => h.onPointerDown(pointer("touch")));
    act(() => void vi.advanceTimersByTime(LONG_PRESS_MS));
    // The finger dragged off: pointercancel, no click.
    act(() => h.onPointerCancel());
    act(() => void vi.advanceTimersByTime(CLICK_GRACE_MS));
    const later = click();
    h.onClickCapture(later as never);
    expect(later.preventDefault).not.toHaveBeenCalled();
  });

  it("a new press of any kind starts clean", () => {
    const { result } = renderHook(() => useLongPressLabel());
    const h = result.current.bind("Keychain");
    act(() => h.onPointerDown(pointer("touch")));
    act(() => void vi.advanceTimersByTime(LONG_PRESS_MS));
    act(() => h.onPointerUp());
    // A mouse press (before the grace has passed) clears the leftover.
    act(() => h.onPointerDown(pointer("mouse")));
    const e = click();
    h.onClickCapture(e as never);
    expect(e.preventDefault).not.toHaveBeenCalled();
  });

  it("a second touch press replaces the first one's timer", () => {
    const { result } = renderHook(() => useLongPressLabel());
    const h = result.current.bind("Keychain");
    act(() => h.onPointerDown(pointer("touch")));
    act(() => void vi.advanceTimersByTime(LONG_PRESS_MS - 100));
    act(() => h.onPointerDown(pointer("touch")));
    // The first timer would have fired here.
    act(() => void vi.advanceTimersByTime(100));
    expect(result.current.label).toBeNull();
    act(() => void vi.advanceTimersByTime(LONG_PRESS_MS - 100));
    expect(result.current.label).not.toBeNull();
  });
});
