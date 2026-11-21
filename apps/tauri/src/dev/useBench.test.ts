import { renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(() => Promise.resolve()) }));

import { invoke } from "@tauri-apps/api/core";
import { createBench } from "./bench";
import type { ShellStatus } from "../terminal/useShell";
import { useBench } from "./useBench";

const invokeMock = vi.mocked(invoke);
const renderer = () => "webgl";
type Status = ShellStatus;

function reports() {
  return invokeMock.mock.calls
    .filter(([cmd]) => cmd === "bench_report")
    .map(([, args]) => JSON.parse((args as { json: string }).json));
}

beforeEach(() => {
  invokeMock.mockClear();
  vi.useFakeTimers();
});
afterEach(() => vi.useRealTimers());

describe("useBench failure handling", () => {
  it("reports a failure when the connection errors before the shell ever opens", () => {
    const { rerender } = renderHook(
      ({ status, error }: { status: Status; error?: string }) =>
        useBench(true, null, status, error, renderer),
      { initialProps: { status: "connecting" as Status, error: undefined as string | undefined } },
    );
    expect(reports()).toHaveLength(0);
    rerender({ status: "error", error: "auth failed" });
    const r = reports();
    expect(r).toHaveLength(1);
    expect(r[0]).toMatchObject({ ok: false, renderer: "webgl" });
    expect(r[0].error).toContain("auth failed");
  });

  it("reports exactly once even if status keeps changing", () => {
    const { rerender } = renderHook(
      ({ status, error }: { status: Status; error?: string }) =>
        useBench(true, null, status, error, renderer),
      { initialProps: { status: "connecting" as Status, error: undefined as string | undefined } },
    );
    rerender({ status: "error", error: "x" });
    rerender({ status: "ended", error: undefined });
    expect(reports()).toHaveLength(1);
  });

  it("does nothing when bench mode is off", () => {
    const { rerender } = renderHook(
      ({ status }: { status: Status }) => useBench(false, null, status, "boom", renderer),
      { initialProps: { status: "connecting" as Status } },
    );
    rerender({ status: "error" });
    vi.advanceTimersByTime(10 * 60_000);
    expect(invokeMock).not.toHaveBeenCalled();
  });

  it("the watchdog reports a timeout if the shell never opens or answers", () => {
    renderHook(() => useBench(true, null, "connecting", undefined, renderer));
    vi.advanceTimersByTime(179_000);
    expect(reports()).toHaveLength(0);
    vi.advanceTimersByTime(2_000);
    const r = reports();
    expect(r).toHaveLength(1);
    expect(r[0].ok).toBe(false);
    expect(r[0].error).toContain("connecting");
  });
});

describe("createBench", () => {
  it("fail() before begin() still reports, and later calls are no-ops", () => {
    const b = createBench(renderer);
    b.fail("no route to host");
    b.fail("again");
    b.dispose();
    vi.advanceTimersByTime(10 * 60_000);
    expect(reports()).toHaveLength(1);
    expect(reports()[0].error).toBe("no route to host");
  });

  it("dispose() cancels the watchdog without reporting", () => {
    createBench(renderer).dispose();
    vi.advanceTimersByTime(10 * 60_000);
    expect(invokeMock).not.toHaveBeenCalled();
  });
});
