import type { IDisposable, Terminal } from "@xterm/xterm";

/** Reports the terminal makes of the mouse (SGR and X10): positional noise for any pane but its own. */
const MOUSE_REPORT = /^\x1b\[(<|M)/;

/**
 * Tells what the user did from what the terminal merely answered.
 *
 * xterm's `onData` carries both: a remote program's queries (device
 * attributes, cursor position, focus, colours...) come back through it as
 * replies, and those must never be copied to other panes. xterm has no public
 * way to tell them apart, but it fires the core service's `onUserInput` right
 * before `onData` for keyboard, IME, paste and `input(data, true)` only, so
 * that internal event marks the data as the user's (checked against xterm
 * 6.0.0, `CoreService.triggerDataEvent(data, wasUserInput)`).
 */
export interface UserInputGate {
  /** Whether the `onData` now firing is the user's input. Call it once per `onData`, which resets it. */
  take(data: string): boolean;
  dispose(): void;
}

interface CoreWithUserInput {
  coreService?: { onUserInput?: (listener: () => void) => IDisposable };
}

let warned = false;

/** A gate on `term`, or `null` when xterm no longer has the internal event (then nothing may be broadcast). */
export function userInputGate(term: Terminal): UserInputGate | null {
  const core = (term as unknown as { _core?: CoreWithUserInput })._core;
  const service = core?.coreService;
  if (typeof service?.onUserInput !== "function") {
    if (!warned) {
      warned = true;
      console.warn("xterm has no user-input event; broadcast is off");
    }
    return null;
  }
  let user = false;
  const sub = service.onUserInput(() => {
    user = true;
    // `onData` follows at once; if nothing took the flag (iOS reads it only inside the session's listener, which
    // is detached while the session is halted) it must not mark a later reply as typing.
    queueMicrotask(() => {
      user = false;
    });
  });
  return {
    take(data) {
      const was = user;
      user = false;
      return was && !MOUSE_REPORT.test(data);
    },
    dispose: () => sub.dispose(),
  };
}
