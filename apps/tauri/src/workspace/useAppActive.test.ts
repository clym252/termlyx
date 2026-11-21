import { act, cleanup, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { SETTLE_MS, setFrontCheck, useAppActive } from "./useAppActive";

let visibility: DocumentVisibilityState = "visible";
function setVisibility(next: DocumentVisibilityState) {
  visibility = next;
  document.dispatchEvent(new Event("visibilitychange"));
}

beforeEach(() => {
  visibility = "visible";
  vi.useFakeTimers();
  vi.setSystemTime(10_000);
  vi.spyOn(document, "visibilityState", "get").mockImplementation(() => visibility);
});
afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe("useAppActive", () => {
  it("starts_active_when_the_page_is_visible", () => {
    const { result } = renderHook(() => useAppActive());
    expect(result.current).toEqual({ active: true, inactiveSince: null });
  });

  it("starts_inactive_when_the_page_is_hidden", () => {
    visibility = "hidden";
    const { result } = renderHook(() => useAppActive());
    expect(result.current.active).toBe(false);
    expect(result.current.inactiveSince).toBe(10_000);
  });

  it("hiding_records_when_and_showing_again_keeps_it_for_a_moment", () => {
    const { result } = renderHook(() => useAppActive());
    act(() => setVisibility("hidden"));
    expect(result.current).toEqual({ active: false, inactiveSince: 10_000 });

    vi.advanceTimersByTime(60_000);
    act(() => setVisibility("visible"));
    // A drop that is only noticed now still counts as one from the background.
    expect(result.current).toEqual({ active: true, inactiveSince: 10_000 });

    act(() => {
      vi.advanceTimersByTime(SETTLE_MS);
    });
    expect(result.current).toEqual({ active: true, inactiveSince: null });
  });

  it("hiding_again_before_it_settles_restarts_the_record", () => {
    const { result } = renderHook(() => useAppActive());
    act(() => setVisibility("hidden"));
    act(() => setVisibility("visible"));
    vi.advanceTimersByTime(SETTLE_MS - 1);
    act(() => setVisibility("hidden"));
    act(() => {
      vi.advanceTimersByTime(SETTLE_MS * 2);
    });
    expect(result.current.active).toBe(false);
    expect(result.current.inactiveSince).not.toBeNull();
  });
});

describe("useAppActive with a front check (auto-lock)", () => {
  let removeCheck: (() => void) | null = null;
  afterEach(() => {
    removeCheck?.();
    removeCheck = null;
  });

  /** A check whose answers the test gives by hand. */
  function manualCheck() {
    const calls: boolean[] = [];
    const answers: { resolve: () => void; reject: (e: unknown) => void }[] = [];
    removeCheck = setFrontCheck(
      (front) =>
        new Promise<void>((resolve, reject) => {
          calls.push(front);
          answers.push({ resolve, reject });
        }),
    );
    return { calls, answers };
  }

  it("a_return_stays_inactive_until_the_check_answers", async () => {
    const check = manualCheck();
    const { result } = renderHook(() => useAppActive());
    act(() => setVisibility("hidden"));
    act(() => setVisibility("visible"));
    expect(check.calls).toEqual([false, true]);
    // However long the answer takes, nothing resumes before it.
    await act(async () => {
      vi.advanceTimersByTime(5000);
    });
    expect(result.current).toEqual({ active: false, inactiveSince: 10_000 });

    await act(async () => check.answers[1].resolve());
    expect(result.current).toEqual({ active: true, inactiveSince: 10_000 });
    act(() => {
      vi.advanceTimersByTime(SETTLE_MS);
    });
    expect(result.current).toEqual({ active: true, inactiveSince: null });
  });

  it("asks_once_for_all_panes", async () => {
    const check = manualCheck();
    const a = renderHook(() => useAppActive());
    const b = renderHook(() => useAppActive());
    act(() => setVisibility("hidden"));
    act(() => setVisibility("visible"));
    expect(check.calls).toEqual([false, true]);
    await act(async () => check.answers[1].resolve());
    expect(a.result.current.active).toBe(true);
    expect(b.result.current.active).toBe(true);
  });

  it("a_failed_check_still_resumes", async () => {
    const check = manualCheck();
    const { result } = renderHook(() => useAppActive());
    act(() => setVisibility("hidden"));
    act(() => setVisibility("visible"));
    await act(async () => check.answers[1].reject(new Error("ipc")));
    expect(result.current.active).toBe(true);
  });

  it("hidden_again_before_the_answer_stays_inactive", async () => {
    const check = manualCheck();
    const { result } = renderHook(() => useAppActive());
    act(() => setVisibility("hidden"));
    act(() => setVisibility("visible"));
    act(() => setVisibility("hidden"));
    await act(async () => check.answers[1].resolve());
    expect(result.current.active).toBe(false);
  });

  it("a_pane_that_mounts_during_the_check_waits_for_it", async () => {
    const check = manualCheck();
    renderHook(() => useAppActive());
    act(() => setVisibility("hidden"));
    act(() => setVisibility("visible"));
    const late = renderHook(() => useAppActive());
    expect(late.result.current.active).toBe(false);
    await act(async () => check.answers[1].resolve());
    expect(late.result.current.active).toBe(true);
  });

  it("the_check_runs_without_any_pane", () => {
    const check = manualCheck();
    setVisibility("hidden");
    setVisibility("visible");
    expect(check.calls).toEqual([false, true]);
  });
});
