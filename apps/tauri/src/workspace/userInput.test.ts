import { Terminal } from "@xterm/xterm";
import { afterEach, describe, expect, it, vi } from "vitest";
import { userInputGate } from "./userInput";

/** What would be broadcast: the data events the gate lets through. */
function watch(term: Terminal) {
  const gate = userInputGate(term)!;
  const sent: string[] = [];
  const sub = term.onData((data) => {
    if (gate.take(data)) sent.push(data);
  });
  return { sent, stop: () => (sub.dispose(), gate.dispose()) };
}

const written = (term: Terminal, data: string) => new Promise<void>((resolve) => term.write(data, resolve));

describe("user input gate", () => {
  afterEach(() => vi.restoreAllMocks());

  it("broadcast_carries_typing", () => {
    const term = new Terminal();
    const { sent, stop } = watch(term);
    term.input("a", true);
    term.input("ls\r", true);
    expect(sent).toEqual(["a", "ls\r"]);
    stop();
  });

  it("broadcast_never_carries_terminal_replies", async () => {
    const term = new Terminal();
    const { sent, stop } = watch(term);
    const replies: string[] = [];
    term.onData((d) => replies.push(d));
    // Device attributes, cursor position, device status.
    await written(term, "\x1b[c\x1b[6n\x1b[5n");
    expect(replies.length).toBeGreaterThanOrEqual(3);
    expect(sent).toEqual([]);
    // A reply does not leave the flag set for the next one.
    term.input("b", true);
    await written(term, "\x1b[c");
    expect(sent).toEqual(["b"]);
    stop();
  });

  it("broadcast_skips_mouse_reports", () => {
    const term = new Terminal();
    const { sent, stop } = watch(term);
    term.input("\x1b[<0;10;5M", true);
    term.input("\x1b[M !!", true);
    term.input("x", true);
    expect(sent).toEqual(["x"]);
    stop();
  });

  it("broadcast_flag_never_survives_to_a_later_reply", async () => {
    const term = new Terminal();
    const gate = userInputGate(term)!;
    // Typing while nothing reads `onData` (iOS: the session is halted), so `take` never runs for it.
    term.input("a", true);
    await Promise.resolve();
    const sent: string[] = [];
    const sub = term.onData((data) => {
      if (gate.take(data)) sent.push(data);
    });
    await written(term, "\x1b[c");
    expect(sent).toEqual([]);
    sub.dispose();
    gate.dispose();
  });

  it("broadcast_fails_safe_without_the_internal_event", () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const term = { _core: {} } as unknown as Terminal;
    expect(userInputGate(term)).toBeNull();
    expect(userInputGate(term)).toBeNull();
    // Once, and without any data in it.
    expect(warn).toHaveBeenCalledTimes(1);
  });
});
