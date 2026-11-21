import { act, cleanup, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ShellState } from "../terminal/useShell";
import { STABLE_OPEN_MS } from "./reconnect";
import { useReconnect } from "./useReconnect";

beforeEach(() => {
  vi.useFakeTimers();
  vi.setSystemTime(0);
});
afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

interface Props {
  state: ShellState;
  active: boolean;
  inactiveSince: number | null;
  locked: boolean;
}
const FRONT: Props = {
  state: { status: "open", step: "shell" },
  active: true,
  inactiveSince: null,
  locked: false,
};
const LOST: ShellState = { status: "lost", step: "shell", error: "connection reset" };

// `useReconnect` takes its inputs per render; the tests rerender with the whole set.
function harness(initial: Partial<Props> = {}) {
  let props: Props = { ...FRONT, ...initial };
  const onAttempt = vi.fn();
  const onStop = vi.fn();
  const hook = renderHook(() => useReconnect({ ...props, onAttempt, onStop }));
  const set = (next: Partial<Props>) => {
    props = { ...props, ...next };
    hook.rerender();
  };
  return { hook, onAttempt, onStop, set };
}

describe("useReconnect", () => {
  it("a_drop_while_in_front_waits_1s_then_starts_an_attempt", () => {
    const { hook, onAttempt, set } = harness();
    expect(hook.result.current.reconnect).toEqual({ kind: "idle" });
    set({ state: LOST });
    expect(hook.result.current.reconnect).toMatchObject({ kind: "waiting", attempt: 1 });
    expect(onAttempt).not.toHaveBeenCalled();
    act(() => {
      vi.advanceTimersByTime(1000);
    });
    expect(hook.result.current.reconnect).toEqual({ kind: "trying", attempt: 1 });
    expect(onAttempt).toHaveBeenCalledTimes(1);
  });

  it("a_failed_attempt_waits_for_the_next_delay", () => {
    const { hook, set } = harness();
    set({ state: LOST });
    act(() => {
      vi.advanceTimersByTime(1000);
    });
    set({ state: { status: "connecting", step: null } });
    set({ state: { status: "error", step: "tcp", error: "timed out", errorCode: "connect" } });
    expect(hook.result.current.reconnect).toMatchObject({ kind: "waiting", attempt: 2 });
    act(() => {
      vi.advanceTimersByTime(1999);
    });
    expect(hook.result.current.reconnect.kind).toBe("waiting");
    act(() => {
      vi.advanceTimersByTime(1);
    });
    expect(hook.result.current.reconnect).toEqual({ kind: "trying", attempt: 2 });
  });

  it("ended_session_never_reconnects", () => {
    const { hook, onAttempt, set } = harness();
    set({ state: { status: "ended", step: "shell", exitStatus: 0 } });
    act(() => {
      vi.advanceTimersByTime(60_000);
    });
    expect(hook.result.current.reconnect).toEqual({ kind: "idle" });
    expect(onAttempt).not.toHaveBeenCalled();
  });

  it("first_connect_failure_never_reconnects", () => {
    const { hook, onAttempt, set } = harness({ state: { status: "connecting", step: null } });
    set({ state: { status: "error", step: "tcp", error: "refused", errorCode: "connect" } });
    act(() => {
      vi.advanceTimersByTime(60_000);
    });
    expect(hook.result.current.reconnect).toEqual({ kind: "idle" });
    expect(onAttempt).not.toHaveBeenCalled();
  });

  it("a_drop_in_the_background_waits_for_the_user", () => {
    const { hook, onAttempt, set } = harness({ active: false, inactiveSince: 0 });
    set({ state: LOST });
    expect(hook.result.current.reconnect).toEqual({ kind: "manual" });
    set({ active: true });
    act(() => {
      vi.advanceTimersByTime(60_000);
    });
    expect(onAttempt).not.toHaveBeenCalled();
    act(() => hook.result.current.reconnectNow());
    expect(hook.result.current.reconnect).toEqual({ kind: "trying", attempt: 1 });
    expect(onAttempt).toHaveBeenCalledTimes(1);
  });

  it("a_drop_found_just_after_the_app_returns_counts_as_a_background_drop", () => {
    const { hook, set } = harness({ active: true, inactiveSince: 500 });
    set({ state: LOST });
    expect(hook.result.current.reconnect).toEqual({ kind: "manual" });
  });

  it("no_countdown_runs_while_the_app_is_in_the_background", () => {
    const { hook, onAttempt, set } = harness();
    set({ state: LOST });
    set({ active: false, inactiveSince: 100 });
    act(() => {
      vi.advanceTimersByTime(30_000);
    });
    expect(onAttempt).not.toHaveBeenCalled();
    expect(hook.result.current.reconnect.kind).toBe("waiting");
  });

  it("locking_during_the_wait_stops_the_countdown_and_waits_for_the_user", () => {
    const { hook, onAttempt, set } = harness();
    set({ state: LOST });
    set({ locked: true });
    expect(hook.result.current.reconnect).toEqual({ kind: "manual" });
    act(() => {
      vi.advanceTimersByTime(60_000);
    });
    expect(onAttempt).not.toHaveBeenCalled();
  });

  it("a_drop_while_locked_waits_for_the_user", () => {
    const { hook, set } = harness({ locked: true });
    set({ state: LOST });
    expect(hook.result.current.reconnect).toEqual({ kind: "manual" });
  });

  it("a_host_key_failure_during_an_attempt_gives_up_at_once", () => {
    const { hook, onAttempt, set } = harness();
    set({ state: LOST });
    act(() => hook.result.current.reconnectNow());
    set({ state: { status: "error", step: "hostKey", error: "host key rejected", errorCode: "hostKeyRejected" } });
    expect(hook.result.current.reconnect).toEqual({ kind: "gaveUp" });
    act(() => {
      vi.advanceTimersByTime(60_000);
    });
    expect(onAttempt).toHaveBeenCalledTimes(1);
  });

  it("cancel_stops_the_countdown", () => {
    const { hook, onAttempt, set } = harness();
    set({ state: LOST });
    act(() => hook.result.current.cancel());
    expect(hook.result.current.reconnect).toEqual({ kind: "gaveUp" });
    act(() => {
      vi.advanceTimersByTime(60_000);
    });
    expect(onAttempt).not.toHaveBeenCalled();
  });

  it("opening_again_clears_the_state_once_it_has_stayed_open", () => {
    const { hook, set } = harness();
    set({ state: LOST });
    act(() => hook.result.current.reconnectNow());
    set({ state: { status: "open", step: "shell" } });
    act(() => {
      vi.advanceTimersByTime(STABLE_OPEN_MS - 1);
    });
    expect(hook.result.current.reconnect).toEqual({ kind: "trying", attempt: 1 });
    act(() => {
      vi.advanceTimersByTime(1);
    });
    expect(hook.result.current.reconnect).toEqual({ kind: "idle" });
    // A later drop starts the schedule over.
    set({ state: { ...LOST, error: "again" } });
    expect(hook.result.current.reconnect).toMatchObject({ kind: "waiting", attempt: 1 });
  });

  it("a_drop_soon_after_reopening_continues_the_schedule", () => {
    const { hook, set } = harness();
    set({ state: LOST });
    act(() => hook.result.current.reconnectNow()); // attempt 1
    set({ state: { status: "open", step: "shell" } });
    act(() => {
      vi.advanceTimersByTime(STABLE_OPEN_MS - 1);
    });
    set({ state: { ...LOST, error: "dropped again" } });
    expect(hook.result.current.reconnect).toMatchObject({ kind: "waiting", attempt: 2 });
    // The stable timer of the dead session must not wake it.
    act(() => {
      vi.advanceTimersByTime(STABLE_OPEN_MS * 2);
    });
    expect(hook.result.current.reconnect.kind).not.toBe("idle");
  });

  it("flapping_connections_give_up_after_five_attempts", () => {
    const { hook, onAttempt, set } = harness();
    set({ state: LOST });
    for (let n = 1; n <= 5; n++) {
      act(() => hook.result.current.reconnectNow());
      expect(hook.result.current.reconnect).toEqual({ kind: "trying", attempt: n });
      set({ state: { status: "open", step: "shell" } });
      act(() => {
        vi.advanceTimersByTime(2000);
      });
      set({ state: { ...LOST, error: `drop ${n}` } });
    }
    expect(hook.result.current.reconnect).toEqual({ kind: "gaveUp" });
    expect(onAttempt).toHaveBeenCalledTimes(5);
  });

  it("a_drop_before_the_open_answered_is_a_failed_attempt", () => {
    const { hook, set } = harness();
    set({ state: LOST });
    act(() => hook.result.current.reconnectNow());
    set({ state: { status: "connecting", step: "shell" } });
    set({ state: { status: "lost", step: "shell", error: "reset" } });
    expect(hook.result.current.reconnect).toMatchObject({ kind: "waiting", attempt: 2 });
  });

  it("locking_while_a_fresh_session_is_open_does_not_close_it", () => {
    const { hook, onStop, set } = harness();
    set({ state: LOST });
    act(() => hook.result.current.reconnectNow());
    set({ state: { status: "open", step: "shell" } });
    set({ locked: true });
    expect(onStop).not.toHaveBeenCalled();
    act(() => {
      vi.advanceTimersByTime(STABLE_OPEN_MS);
    });
    expect(hook.result.current.reconnect).toEqual({ kind: "idle" });
  });
});
