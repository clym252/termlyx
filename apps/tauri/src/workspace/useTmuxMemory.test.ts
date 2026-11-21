import { act, cleanup, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { TMUX_POLL_MS, useTmuxMemory } from "./useTmuxMemory";

beforeEach(() => {
  vi.useFakeTimers();
});
afterEach(() => {
  cleanup();
  vi.useRealTimers();
  Reflect.deleteProperty(document, "visibilityState");
});

interface Props {
  sessionId: string | null;
  active: boolean;
  locked: boolean;
}

function harness(answer: (id: string) => Promise<string | null>, initial: Partial<Props> = {}) {
  let props: Props = { sessionId: "s1", active: true, locked: false, ...initial };
  const sessionTmux = vi.fn(answer);
  const hook = renderHook(() => useTmuxMemory(props.sessionId, props.active, props.locked, { sessionTmux }));
  const set = (next: Partial<Props>) => {
    props = { ...props, ...next };
    hook.rerender();
  };
  return { hook, sessionTmux, set };
}

const advance = (ms: number) =>
  act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });

function hide() {
  Object.defineProperty(document, "visibilityState", { get: () => "hidden", configurable: true });
  document.dispatchEvent(new Event("visibilitychange"));
}

describe("useTmuxMemory", () => {
  it("polls_every_15_s_while_the_session_is_open_and_the_app_active", async () => {
    const { hook, sessionTmux } = harness(async () => "work");
    expect(TMUX_POLL_MS).toBe(15_000);
    expect(hook.result.current).toBeNull();
    await advance(14_999);
    expect(sessionTmux).not.toHaveBeenCalled();
    await advance(1);
    expect(sessionTmux).toHaveBeenCalledTimes(1);
    expect(sessionTmux).toHaveBeenCalledWith("s1");
    expect(hook.result.current).toBe("work");
    await advance(15_000);
    expect(sessionTmux).toHaveBeenCalledTimes(2);
  });

  it("stops_polling_while_inactive_or_without_a_session", async () => {
    const { sessionTmux, set } = harness(async () => "work");
    set({ active: false });
    await advance(60_000);
    expect(sessionTmux).not.toHaveBeenCalled();
    set({ active: true, sessionId: null });
    await advance(60_000);
    expect(sessionTmux).not.toHaveBeenCalled();
  });

  it("keeps_the_last_answer_when_a_poll_fails", async () => {
    const answers: (() => Promise<string | null>)[] = [
      async () => "work",
      async () => Promise.reject(new Error("closed")),
      async () => null,
    ];
    const { hook } = harness(() => answers.shift()!());
    await advance(15_000);
    expect(hook.result.current).toBe("work");
    await advance(15_000);
    expect(hook.result.current).toBe("work");
    // "No tmux" is an answer: the shell left tmux.
    await advance(15_000);
    expect(hook.result.current).toBeNull();
  });

  it("keeps_the_answer_across_a_drop_and_polls_the_new_session", async () => {
    const { hook, sessionTmux, set } = harness(async (id) => (id === "s1" ? "work" : "other"));
    await advance(15_000);
    expect(hook.result.current).toBe("work");
    set({ sessionId: null });
    await advance(30_000);
    expect(hook.result.current).toBe("work");
    set({ sessionId: "s2" });
    expect(hook.result.current).toBe("work");
    await advance(15_000);
    expect(sessionTmux).toHaveBeenLastCalledWith("s2");
    expect(hook.result.current).toBe("other");
  });

  it("polls_once_as_the_app_goes_to_the_background", async () => {
    const { hook, sessionTmux, set } = harness(async () => "work");
    hide();
    set({ active: false });
    await advance(0);
    expect(sessionTmux).toHaveBeenCalledTimes(1);
    expect(hook.result.current).toBe("work");
    await advance(60_000);
    expect(sessionTmux).toHaveBeenCalledTimes(1);
  });

  it("never_asks_while_the_vault_is_locked_and_keeps_the_last_answer", async () => {
    const { hook, sessionTmux, set } = harness(async () => "work");
    await advance(15_000);
    expect(sessionTmux).toHaveBeenCalledTimes(1);
    // Locked in front (the pane under the unlock screen): no polling...
    set({ locked: true });
    await advance(60_000);
    // ...and none as the page goes to the background either.
    hide();
    set({ active: false });
    await advance(60_000);
    expect(sessionTmux).toHaveBeenCalledTimes(1);
    expect(hook.result.current).toBe("work");
    // Unlocked and in front again: polling resumes.
    set({ locked: false, active: true });
    await advance(15_000);
    expect(sessionTmux).toHaveBeenCalledTimes(2);
  });

  it("ignores_an_answer_for_a_session_it_no_longer_tracks", async () => {
    let release: (v: string | null) => void = () => {};
    const { hook, set } = harness(
      (id) => (id === "s1" ? new Promise((resolve) => (release = resolve)) : Promise.resolve(null)),
    );
    await advance(15_000);
    set({ sessionId: null });
    await act(async () => release("late"));
    expect(hook.result.current).toBeNull();
  });
});
