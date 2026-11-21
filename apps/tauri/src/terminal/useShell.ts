import { useEffect, useRef, useState } from "react";
import { api as appApi } from "../api";
import { isApiError, type ConnectStep, type SessionHandlers, type SessionTarget, type TetherApi } from "../api/types";
import { describeError } from "../ui/errors";
import { sessionsReady } from "../workspace/startup";

/** The slice of xterm's `Terminal` that `useShell` needs (kept narrow so tests can fake it). */
export interface ShellTerm {
  readonly cols: number;
  readonly rows: number;
  write(data: Uint8Array | string, callback?: () => void): void;
  onData(listener: (data: string) => void): { dispose(): void };
  onResize(listener: (size: { cols: number; rows: number }) => void): {
    dispose(): void;
  };
}

/** DEV connect form input (direct parameters). Terminal size is taken from the bound terminal. */
export interface ConnectInput {
  host: string;
  port: number;
  username: string;
  password?: string;
  privateKeyPem?: string;
  passphrase?: string;
}

/** What a shell runs on: a vault host, a local pty, or (DEV only) direct connect parameters. */
export type ShellTarget = SessionTarget | { kind: "direct"; params: ConnectInput };

/**
 * `ended`: the remote shell exited (`exitStatus` when known) or the session
 * was closed. `lost`: the connection failed (`error` says why). `error`:
 * the session could not be opened.
 */
export type ShellStatus = "connecting" | "open" | "ended" | "lost" | "error";

export interface ShellState {
  status: ShellStatus;
  /**
   * The connect step last reported for this attempt (`null`: none yet, or
   * none ever: a tab waiting on another tab's connection to the same host
   * hears nothing until `shell`). Kept once the attempt fails, where it is
   * the step that failed.
   */
  step: ConnectStep | null;
  error?: string;
  /** With `error` on `status: "error"`: the API error's code (`connect`, `authFailed`, `hostKeyRejected`, ...), when it had one. */
  errorCode?: string;
  exitStatus?: number | null;
}

export interface UseShellOptions {
  /** Defaults to the app's API. */
  api?: TetherApi;
  /** Called with the session id once it is open. A throw closes the session and reports an error. */
  onOpen?: (sessionId: string) => void;
  /** Bump to open a new session on the same target (reconnect). */
  attempt?: number;
}

/**
 * Keystroke bytes allowed in flight or queued for one session. Past it,
 * input is dropped (and the terminal says so) rather than buffered without
 * bound: the backend applies writes in order, so a stalled connection
 * would otherwise grow the queue forever (a huge paste, a stuck network).
 */
export const MAX_QUEUED_WRITE_BYTES = 1 << 20;

/** Shown in the terminal (not sent) when input is dropped; once per overflow. */
const DROPPED_NOTICE = "\r\n\x1b[33m[tether: input dropped: too much input is waiting to be sent]\x1b[0m\r\n";

/**
 * Acks are flushed once per animation frame; this timer is the fallback
 * when frames do not run (a hidden or minimized window), so backpressure
 * never stalls a session the user is not looking at.
 */
const ACK_FALLBACK_MS = 100;

const encoder = new TextEncoder();

/** Calls `fn` on the next animation frame, or after the fallback delay, whichever is first. */
function nextFrame(fn: () => void): () => void {
  let done = false;
  const hasFrames = typeof requestAnimationFrame === "function";
  const cancel = () => {
    done = true;
    if (hasFrames) cancelAnimationFrame(raf);
    clearTimeout(timer);
  };
  const run = () => {
    if (done) return;
    cancel();
    fn();
  };
  const raf = hasFrames ? requestAnimationFrame(run) : 0;
  const timer = setTimeout(run, ACK_FALLBACK_MS);
  return cancel;
}

/**
 * Runs a fire-and-forget command, logging (never propagating) its failure,
 * whether it rejects or throws synchronously.
 */
function quietly(what: string, run: () => Promise<unknown>): Promise<void> {
  return Promise.resolve()
    .then(run)
    .then(
      () => {},
      (err: unknown) => console.debug(`${what} failed`, err),
    );
}

/**
 * Opens a shell session on `target` and binds it to `term` until `target`,
 * `term` or `options.attempt` changes or the component unmounts. Pass
 * `null` for either to stay idle. `target` must be stable (memoize it).
 *
 * - Sessions open only after the load-time orphan cleanup (`sessionsReady`).
 * - Keystrokes go to the backend as raw bytes, in order, through one chain
 *   capped at `MAX_QUEUED_WRITE_BYTES`; keystrokes and resizes made before
 *   the session is open are sent once it is.
 * - Backpressure: output is acked only from xterm's write callback (once
 *   parsed), coalesced to one ack per animation frame.
 */
export function useShell(
  term: ShellTerm | null,
  target: ShellTarget | null,
  options: UseShellOptions = {},
): ShellState {
  const [state, setState] = useState<ShellState>({ status: "connecting", step: null });
  const api = options.api ?? appApi;
  const onOpen = useRef(options.onOpen);
  onOpen.current = options.onOpen;
  const attempt = options.attempt ?? 0;

  useEffect(() => {
    if (!term || !target) return;

    let disposed = false;
    let ended = false;
    let id: string | null = null;
    // Set once the open has resolved or failed: progress that trails the
    // answer (Channel messages can arrive after the invoke response) is stale.
    let settled = false;
    let step: ConnectStep | null = null;
    setState({ status: "connecting", step });

    // -- output and acks --------------------------------------------------
    let unacked = 0;
    let cancelAckFlush: (() => void) | null = null;
    const flushAcks = () => {
      cancelAckFlush = null;
      if (id === null || unacked === 0) return;
      const bytes = unacked;
      unacked = 0;
      {
        const sessionId = id;
        void quietly("ack", () => api.ack(sessionId, bytes));
      }
    };
    const acked = (n: number) => {
      unacked += n;
      // Before the id is known the bytes wait; the open handler flushes them.
      if (id !== null && !cancelAckFlush) cancelAckFlush = nextFrame(flushAcks);
    };

    const handlers: SessionHandlers = {
      onOutput: (bytes) => {
        if (disposed) return;
        term.write(bytes, () => acked(bytes.length));
      },
      onClosed: (reason, exitStatus) => {
        if (disposed || ended) return;
        ended = true;
        setState(
          reason !== null
            ? { status: "lost", step, error: reason }
            : { status: "ended", step, exitStatus: exitStatus ?? null },
        );
      },
      onProgress: (next) => {
        if (disposed || settled || ended) return;
        step = next;
        setState({ status: "connecting", step });
      },
    };

    // -- input: one ordered, bounded chain --------------------------------
    let opened!: (sessionId: string | null) => void;
    const openedId = new Promise<string | null>((r) => (opened = r));
    let chain: Promise<unknown> = openedId;
    let queuedBytes = 0;
    let overflowNotified = false;
    // Resizes coalesce: only the latest size is sent when its turn comes.
    let pendingSize: { cols: number; rows: number } | null = null;

    // Each link catches its own failure, so one failed task can never
    // leave the chain rejected (and all later input dead).
    const enqueue = (task: (sessionId: string) => Promise<void>, always?: () => void) => {
      chain = chain
        .then(async () => {
          const sessionId = await openedId;
          if (sessionId === null || disposed || ended) return;
          await task(sessionId);
        })
        .catch((err: unknown) => console.debug("session input failed", err))
        .finally(always);
    };

    const subscriptions = [
      term.onData((data) => {
        const bytes = encoder.encode(data);
        if (queuedBytes + bytes.length > MAX_QUEUED_WRITE_BYTES) {
          if (!overflowNotified) {
            overflowNotified = true;
            term.write(DROPPED_NOTICE);
          }
          return;
        }
        queuedBytes += bytes.length;
        enqueue(
          (sessionId) => quietly("write", () => api.write(sessionId, bytes)),
          // Released however the write ends (sent, failed, skipped).
          () => {
            queuedBytes -= bytes.length;
            if (queuedBytes === 0) overflowNotified = false;
          },
        );
      }),
      term.onResize((size) => {
        pendingSize = size;
        enqueue(async (sessionId) => {
          const next = pendingSize;
          if (!next) return; // already sent by an earlier queued task
          pendingSize = null;
          await quietly("resize", () => api.resize(sessionId, next.cols, next.rows));
        });
      }),
    ];

    // -- open ---------------------------------------------------------------
    const size = { cols: term.cols, rows: term.rows };
    const open = () =>
      target.kind === "direct"
        ? api.openShell({ ...target.params, ...size }, handlers)
        : api.openSession(target, size, handlers);

    sessionsReady(api)
      .then(() => {
        if (disposed) return null;
        return open();
      })
      .then(
        (session) => {
          if (!session) return;
          settled = true;
          const sessionId = session.sessionId;
          if (disposed) {
            void quietly("close", () => api.close(sessionId));
            return;
          }
          // Nothing here may orphan the session: on any throw it is closed.
          try {
            id = sessionId;
            onOpen.current?.(sessionId);
            if (unacked > 0) flushAcks();
            opened(sessionId);
            if (!ended) setState({ status: "open", step });
          } catch (err) {
            ended = true;
            id = null;
            opened(null);
            void quietly("close", () => api.close(sessionId));
            setState({ status: "error", step, error: describeError(err) });
          }
        },
        (err: unknown) => {
          settled = true;
          opened(null);
          if (!disposed) {
            setState({
              status: "error",
              step,
              error: describeError(err),
              ...(isApiError(err) && { errorCode: err.code }),
            });
          }
        },
      );

    return () => {
      disposed = true;
      opened(null);
      cancelAckFlush?.();
      subscriptions.forEach((s) => s.dispose());
      const sessionId = id;
      if (sessionId !== null && !ended) void quietly("close", () => api.close(sessionId));
    };
  }, [term, target, api, attempt]);

  return state;
}
