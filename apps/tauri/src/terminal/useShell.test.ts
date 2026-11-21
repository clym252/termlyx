import { act, render, renderHook, screen, waitFor } from "@testing-library/react";
import { createElement } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { SessionHandlers, TetherApi } from "../api/types";
import { SessionStatus } from "../workspace/SessionStatus";
import { sessionsReady } from "../workspace/startup";
import { MAX_QUEUED_WRITE_BYTES, useShell, type ShellTarget, type ShellTerm } from "./useShell";

const LOCAL: ShellTarget = { kind: "local" };

function fakeTerm() {
  const term = {
    cols: 80,
    rows: 24,
    // Like xterm: the callback runs once the data has been parsed.
    write: vi.fn((_d: Uint8Array | string, cb?: () => void) => cb?.()),
    onData: vi.fn((_l: (d: string) => void) => ({ dispose: vi.fn() })),
    onResize: vi.fn((_l: (s: { cols: number; rows: number }) => void) => ({
      dispose: vi.fn(),
    })),
  };
  return term as typeof term & ShellTerm;
}

/** The session slice of TetherApi, recording the order of calls. */
function fakeApi() {
  const calls: string[] = [];
  const handlers: SessionHandlers[] = [];
  const api = {
    closeAllSessions: vi.fn(async () => {
      calls.push("closeAllSessions");
      return 0;
    }),
    openSession: vi.fn(async (_t: unknown, _s: unknown, h: SessionHandlers) => {
      calls.push("openSession");
      handlers.push(h);
      return { sessionId: "sess-1" };
    }),
    openShell: vi.fn(async (_p: unknown, h: SessionHandlers) => {
      calls.push("openShell");
      handlers.push(h);
      return { sessionId: "dev-1" };
    }),
    write: vi.fn(async (_id: string, _data: Uint8Array) => {}),
    resize: vi.fn(async (_id: string, _c: number, _r: number) => {}),
    ack: vi.fn(async (_id: string, _n: number) => {}),
    close: vi.fn(async (_id: string) => {}),
  };
  return { api, typed: api as unknown as TetherApi, calls, handlers };
}

type Fake = ReturnType<typeof fakeApi>;

async function openShell(fake: Fake, term = fakeTerm(), target: ShellTarget = LOCAL) {
  const hook = renderHook(() => useShell(term, target, { api: fake.typed }));
  await waitFor(() => expect(hook.result.current.status).toBe("open"));
  return { ...hook, term };
}

const bytesOf = (s: string) => Array.from(new TextEncoder().encode(s));

let fake: Fake;
beforeEach(() => {
  fake = fakeApi();
});

describe("useShell", () => {
  it("reload_closes_orphaned_sessions", async () => {
    // A reload leaves the previous page's sessions running in Rust: the
    // app's bootstrap closes them before any session is opened.
    let release!: (n: number) => void;
    fake.api.closeAllSessions.mockImplementation(() => {
      fake.calls.push("closeAllSessions");
      return new Promise<number>((r) => (release = r));
    });
    void sessionsReady(fake.typed); // what main.tsx does on load
    const term = fakeTerm();
    const { result } = renderHook(() => useShell(term, LOCAL, { api: fake.typed }));
    await new Promise((r) => setTimeout(r, 20));
    expect(fake.api.openSession).not.toHaveBeenCalled();
    expect(result.current.status).toBe("connecting");

    release(2);
    await waitFor(() => expect(result.current.status).toBe("open"));
    expect(fake.calls).toEqual(["closeAllSessions", "openSession"]);
    // Once per load, not once per session.
    const term2 = fakeTerm();
    renderHook(() => useShell(term2, LOCAL, { api: fake.typed }));
    await waitFor(() => expect(fake.api.openSession).toHaveBeenCalledTimes(2));
    expect(fake.api.closeAllSessions).toHaveBeenCalledTimes(1);
  });

  it("orphan_cleanup_failure_does_not_block_sessions", async () => {
    fake.api.closeAllSessions.mockRejectedValue(new Error("boom"));
    await openShell(fake);
    expect(fake.api.openSession).toHaveBeenCalledTimes(1);
  });

  it("status_distinguishes_remote_exit_from_connection_lost", async () => {
    const exited = await openShell(fake);
    act(() => fake.handlers[0].onClosed(null, 3));
    expect(exited.result.current).toEqual({ status: "ended", step: null, exitStatus: 3 });

    const lost = await openShell(fake);
    act(() => fake.handlers[1].onClosed("connection reset by peer", null));
    expect(lost.result.current).toEqual({ status: "lost", step: null, error: "connection reset by peer" });

    // The pane says which, and only a lost connection offers Reconnect.
    const onReconnect = vi.fn();
    const ended = render(
      createElement(SessionStatus, { state: exited.result.current, header: null, onReconnect, onCancel: vi.fn(), onClose: vi.fn() }),
    );
    expect(screen.getByRole("status").textContent).toContain("Session ended (exit status 3)");
    expect(screen.queryByRole("button", { name: "Reconnect" })).toBeNull();
    ended.unmount();

    render(createElement(SessionStatus, { state: lost.result.current, header: null, onReconnect, onCancel: vi.fn(), onClose: vi.fn() }));
    expect(screen.getByRole("alert").textContent).toContain("Connection lost");
    expect(screen.getByRole("alert").textContent).toContain("connection reset by peer");
    act(() => screen.getByRole("button", { name: "Reconnect" }).click());
    expect(onReconnect).toHaveBeenCalledTimes(1);
  });

  it("ended_without_status_says_session_ended", async () => {
    render(
      createElement(SessionStatus, {
        state: { status: "ended", step: null, exitStatus: null },
        header: null,
        onReconnect: vi.fn(),
        onCancel: vi.fn(),
        onClose: vi.fn(),
      }),
    );
    expect(screen.getByRole("status").textContent).toBe("Session ended");
  });

  it("writes_frames_to_terminal_and_coalesces_acks", async () => {
    const { term } = await openShell(fake);
    const output = fake.handlers[0];
    act(() => {
      output.onOutput(new Uint8Array([104, 105]));
      output.onOutput(new Uint8Array([1, 2, 3]));
    });
    expect(term.write).toHaveBeenCalledTimes(2);
    expect(term.write.mock.calls[0][0]).toEqual(new Uint8Array([104, 105]));
    // One ack per frame for everything written meanwhile.
    await waitFor(() => expect(fake.api.ack).toHaveBeenCalled());
    expect(fake.api.ack.mock.calls).toEqual([["sess-1", 5]]);

    // The ack comes from the write callback, not before the write completes.
    term.write.mockImplementationOnce(() => {});
    fake.api.ack.mockClear();
    act(() => output.onOutput(new Uint8Array([9])));
    await new Promise((r) => setTimeout(r, 50));
    expect(fake.api.ack).not.toHaveBeenCalled();
  });

  it("writes_send_raw_bytes", async () => {
    const { term } = await openShell(fake);
    term.onData.mock.calls[0][0]("é\x03");
    await waitFor(() => expect(fake.api.write).toHaveBeenCalled());
    const [id, body] = fake.api.write.mock.calls[0];
    expect(id).toBe("sess-1");
    // Binary (a Uint8Array, possibly from another realm in jsdom), never a JSON number array.
    expect(Object.prototype.toString.call(body)).toBe("[object Uint8Array]");
    expect(Array.from(body)).toEqual([0xc3, 0xa9, 0x03]);
  });

  it("writes_are_serialized_in_order", async () => {
    const { term } = await openShell(fake);
    let releaseFirst!: () => void;
    const firstDone = new Promise<void>((r) => (releaseFirst = r));
    fake.api.write.mockImplementation(async (_id, data) => {
      if (data[0] === 97) await firstDone;
    });

    const onData = term.onData.mock.calls[0][0];
    onData("a");
    onData("b");
    await new Promise((r) => setTimeout(r, 20));
    const writes = () => fake.api.write.mock.calls.map(([, d]) => d[0]);
    // #2 must not be sent while #1 is unsettled.
    expect(writes()).toEqual([97]);

    releaseFirst();
    await waitFor(() => expect(writes()).toEqual([97, 98]));
  });

  it("write_chain_is_bounded", async () => {
    const { term } = await openShell(fake);
    let release!: () => void;
    const stuck = new Promise<void>((r) => (release = r));
    fake.api.write.mockImplementation(() => stuck);
    const onData = term.onData.mock.calls[0][0];

    onData("a".repeat(MAX_QUEUED_WRITE_BYTES - 1));
    onData("b"); // fills the cap exactly
    onData("c"); // over the cap: dropped
    onData("d"); // still over: dropped, without a second notice
    const notices = () =>
      term.write.mock.calls.filter(([d]) => typeof d === "string" && d.includes("input dropped"));
    expect(notices()).toHaveLength(1);

    release();
    await waitFor(() => expect(fake.api.write).toHaveBeenCalledTimes(2));
    // Room again once the queue drains: the next keystroke goes through.
    onData("e");
    await waitFor(() => expect(fake.api.write).toHaveBeenCalledTimes(3));
    const sent = fake.api.write.mock.calls.map(([, d]) => new TextDecoder().decode(d).slice(-1));
    expect(sent).toEqual(["a", "b", "e"]);
  });

  it("a_throwing_write_does_not_kill_later_input", async () => {
    const { term } = await openShell(fake);
    fake.api.write.mockImplementationOnce(() => {
      throw new Error("invoke threw synchronously");
    });
    fake.api.write.mockImplementationOnce(() => Promise.reject(new Error("rejected")));
    const onData = term.onData.mock.calls[0][0];
    onData("a"); // throws
    onData("b"); // rejects
    await waitFor(() => expect(fake.api.write).toHaveBeenCalledTimes(2));
    await new Promise((r) => setTimeout(r, 10));
    onData("c".repeat(MAX_QUEUED_WRITE_BYTES - 1)); // fits only if a and b were released
    onData("d");
    await waitFor(() => expect(fake.api.write).toHaveBeenCalledTimes(4));
    const sent = fake.api.write.mock.calls.map(([, d]) => new TextDecoder().decode(d).slice(-1));
    expect(sent).toEqual(["a", "b", "c", "d"]);
    expect(term.write.mock.calls.some(([d]) => typeof d === "string")).toBe(false); // nothing dropped
  });

  it("resizes_coalesce_to_latest_size_after_pending_write", async () => {
    const { term } = await openShell(fake);
    let releaseWrite!: () => void;
    const writeDone = new Promise<void>((r) => (releaseWrite = r));
    fake.api.write.mockImplementation(() => writeDone);

    term.onData.mock.calls[0][0]("x");
    const onResize = term.onResize.mock.calls[0][0];
    onResize({ cols: 100, rows: 30 });
    onResize({ cols: 120, rows: 40 });
    releaseWrite();

    await waitFor(() => expect(fake.api.resize).toHaveBeenCalledWith("sess-1", 120, 40));
    expect(fake.api.resize).toHaveBeenCalledTimes(1);
  });

  it("resize_before_open_is_sent_once_open", async () => {
    let resolveOpen!: (o: { sessionId: string }) => void;
    fake.api.openSession.mockImplementation(async (_t, _s, h) => {
      fake.handlers.push(h);
      return new Promise((r) => (resolveOpen = r));
    });
    const term = fakeTerm();
    const { result } = renderHook(() => useShell(term, LOCAL, { api: fake.typed }));
    await waitFor(() => expect(fake.api.openSession).toHaveBeenCalled());
    // Opened at 80x24; the terminal is fitted to 132x43 before open resolves.
    expect(fake.api.openSession.mock.calls[0][1]).toEqual({ cols: 80, rows: 24 });
    term.onResize.mock.calls[0][0]({ cols: 132, rows: 43 });
    await new Promise((r) => setTimeout(r, 20));
    expect(fake.api.resize).not.toHaveBeenCalled();

    resolveOpen({ sessionId: "sess-1" });
    await waitFor(() => expect(result.current.status).toBe("open"));
    await waitFor(() => expect(fake.api.resize).toHaveBeenCalledWith("sess-1", 132, 43));
    expect(fake.api.resize).toHaveBeenCalledTimes(1);
  });

  it("throw_in_open_callback_closes_session", async () => {
    const onOpen = vi.fn(() => {
      throw new Error("pane went away");
    });
    const term = fakeTerm();
    const { result } = renderHook(() => useShell(term, LOCAL, { api: fake.typed, onOpen }));
    await waitFor(() => expect(result.current.status).toBe("error"));
    expect(result.current.error).toContain("pane went away");
    expect(onOpen).toHaveBeenCalledWith("sess-1");
    // Not orphaned: the session that opened is closed.
    await waitFor(() => expect(fake.api.close).toHaveBeenCalledWith("sess-1"));
  });

  it("open_failure_reports_error", async () => {
    fake.api.openSession.mockRejectedValue({ code: "hostKeyRejected", message: "host key rejected", refs: [] });
    const term = fakeTerm();
    const { result } = renderHook(() => useShell(term, LOCAL, { api: fake.typed }));
    await waitFor(() => expect(result.current.status).toBe("error"));
    expect(result.current.error).toBe("host key rejected");
    expect(result.current.errorCode).toBe("hostKeyRejected");
  });

  describe("connect steps", () => {
    /** An open that waits for the test, so progress can be reported while it is pending. */
    function heldOpen() {
      let finish!: (v: { sessionId: string }) => void;
      let fail!: (e: unknown) => void;
      fake.api.openSession.mockImplementation(async (_t: unknown, _s: unknown, h: SessionHandlers) => {
        fake.handlers.push(h);
        return new Promise<{ sessionId: string }>((res, rej) => {
          finish = res;
          fail = rej;
        });
      });
      return { finish: (v = { sessionId: "sess-1" }) => finish(v), fail: (e: unknown) => fail(e) };
    }

    it("tracks_the_last_reported_step_even_when_it_goes_back", async () => {
      const held = heldOpen();
      const term = fakeTerm();
      const { result } = renderHook(() => useShell(term, LOCAL, { api: fake.typed }));
      await waitFor(() => expect(fake.handlers).toHaveLength(1));
      expect(result.current).toEqual({ status: "connecting", step: null });
      act(() => fake.handlers[0].onProgress?.("auth"));
      expect(result.current.step).toBe("auth");
      act(() => fake.handlers[0].onProgress?.("tcp"));
      expect(result.current.step).toBe("tcp");
      held.finish();
      await waitFor(() => expect(result.current.status).toBe("open"));
    });

    it("keeps_the_step_that_was_current_when_the_open_fails", async () => {
      const held = heldOpen();
      const term = fakeTerm();
      const { result } = renderHook(() => useShell(term, LOCAL, { api: fake.typed }));
      await waitFor(() => expect(fake.handlers).toHaveLength(1));
      act(() => fake.handlers[0].onProgress?.("tcp"));
      act(() => held.fail({ code: "connect", message: "timed out", refs: [] }));
      await waitFor(() => expect(result.current.status).toBe("error"));
      expect(result.current).toEqual({ status: "error", step: "tcp", error: "timed out", errorCode: "connect" });
    });

    it("ignores_progress_that_trails_the_answer", async () => {
      const held = heldOpen();
      const term = fakeTerm();
      const { result } = renderHook(() => useShell(term, LOCAL, { api: fake.typed }));
      await waitFor(() => expect(fake.handlers).toHaveLength(1));
      act(() => fake.handlers[0].onProgress?.("auth"));
      held.finish();
      await waitFor(() => expect(result.current.status).toBe("open"));
      act(() => fake.handlers[0].onProgress?.("shell"));
      expect(result.current).toEqual({ status: "open", step: "auth" });
    });

    it("starts_over_on_a_new_attempt", async () => {
      const term = fakeTerm();
      heldOpen();
      const { result, rerender } = renderHook(
        ({ attempt }) => useShell(term, LOCAL, { api: fake.typed, attempt }),
        { initialProps: { attempt: 0 } },
      );
      await waitFor(() => expect(fake.handlers).toHaveLength(1));
      act(() => fake.handlers[0].onProgress?.("hostKey"));
      expect(result.current.step).toBe("hostKey");
      rerender({ attempt: 1 });
      expect(result.current).toEqual({ status: "connecting", step: null });
      await waitFor(() => expect(fake.handlers).toHaveLength(2));
      // The first attempt's late progress does not reach the new one.
      act(() => fake.handlers[0].onProgress?.("shell"));
      expect(result.current.step).toBeNull();
    });
  });

  it("direct_target_uses_open_shell_with_terminal_size", async () => {
    const params = { host: "127.0.0.1", port: 2222, username: "tether", password: "x" };
    await openShell(fake, fakeTerm(), { kind: "direct", params });
    expect(fake.api.openShell.mock.calls[0][0]).toEqual({ ...params, cols: 80, rows: 24 });
  });

  it("unmount_closes_session_and_late_open_is_closed", async () => {
    // Case 1: unmount after open -> close with the id.
    const first = await openShell(fake);
    first.unmount();
    await waitFor(() => expect(fake.api.close).toHaveBeenCalledWith("sess-1"));

    // Case 2: unmount while open is still pending -> closed once it resolves.
    fake.api.close.mockClear();
    let resolveOpen!: (o: { sessionId: string }) => void;
    fake.api.openSession.mockImplementation(() => new Promise((r) => (resolveOpen = r)));
    const term2 = fakeTerm();
    const second = renderHook(() => useShell(term2, LOCAL, { api: fake.typed }));
    await waitFor(() => expect(fake.api.openSession).toHaveBeenCalledTimes(2));
    second.unmount();
    expect(fake.api.close).not.toHaveBeenCalled();
    resolveOpen({ sessionId: "late-1" });
    await waitFor(() => expect(fake.api.close).toHaveBeenCalledWith("late-1"));
    expect(fake.api.write).not.toHaveBeenCalled();
  });

  it("keystrokes_before_open_are_sent_once_open", async () => {
    let resolveOpen!: (o: { sessionId: string }) => void;
    fake.api.openSession.mockImplementation(() => new Promise((r) => (resolveOpen = r)));
    const term = fakeTerm();
    renderHook(() => useShell(term, LOCAL, { api: fake.typed }));
    await waitFor(() => expect(fake.api.openSession).toHaveBeenCalled());
    term.onData.mock.calls[0][0]("ls\r");
    expect(fake.api.write).not.toHaveBeenCalled();
    resolveOpen({ sessionId: "sess-1" });
    await waitFor(() => expect(fake.api.write).toHaveBeenCalled());
    expect(Array.from(fake.api.write.mock.calls[0][1])).toEqual(bytesOf("ls\r"));
  });
});
