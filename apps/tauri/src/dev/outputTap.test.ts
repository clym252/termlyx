import { describe, expect, it, vi } from "vitest";
import type { ShellTerm } from "../terminal/useShell";
import { onTerminalWrite, tapShellTerm } from "./outputTap";

function fakeTerm() {
  return {
    cols: 80,
    rows: 24,
    write: vi.fn((_d: Uint8Array, cb?: () => void) => cb?.()),
    onData: vi.fn(() => ({ dispose: vi.fn() })),
    onResize: vi.fn(() => ({ dispose: vi.fn() })),
  } as unknown as ShellTerm & { write: ReturnType<typeof vi.fn> };
}

describe("tapShellTerm", () => {
  it("passes writes straight through with no listeners", () => {
    const term = fakeTerm();
    const cb = vi.fn();
    tapShellTerm(term).write(Uint8Array.of(1), cb);
    expect(cb).toHaveBeenCalledOnce();
  });

  it("always runs the write callback (the ack) even if a listener throws", () => {
    const term = fakeTerm();
    const cb = vi.fn();
    const off = onTerminalWrite(() => {
      throw new Error("listener bug");
    });
    try {
      expect(() => tapShellTerm(term).write(Uint8Array.of(1), cb)).toThrow("listener bug");
      expect(cb).toHaveBeenCalledOnce();
    } finally {
      off();
    }
  });
});
