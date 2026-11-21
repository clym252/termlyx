import type { ShellTerm } from "../terminal/useShell";

/** Called once xterm has parsed a chunk (i.e. from its write callback). */
/** `submittedAt` is when the chunk arrived from IPC; `parsedAt - submittedAt` is xterm's write latency (queue wait plus parse). */
export type WriteListener = (chunk: Uint8Array, parsedAt: number, submittedAt: number) => void;

const listeners = new Set<WriteListener>();

/** Subscribe to terminal output as written to xterm. Returns the unsubscribe. */
export function onTerminalWrite(listener: WriteListener): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/**
 * Wraps a terminal so dev tooling (throughput overlay, bench) can observe
 * output. With no listeners the wrapper is a plain pass-through.
 */
export function tapShellTerm(term: ShellTerm): ShellTerm {
  return {
    get cols() {
      return term.cols;
    },
    get rows() {
      return term.rows;
    },
    write(data, callback) {
      // Local notices (strings) are not session output.
      if (listeners.size === 0 || typeof data === "string") return term.write(data, callback);
      const submittedAt = performance.now();
      term.write(data, () => {
        const parsedAt = performance.now();
        // The callback is what acks the bytes to the backend; a throwing dev
        // listener must never be able to swallow it.
        try {
          for (const l of listeners) l(data, parsedAt, submittedAt);
        } finally {
          callback?.();
        }
      });
    },
    onData: (l) => term.onData(l),
    onResize: (l) => term.onResize(l),
  };
}
