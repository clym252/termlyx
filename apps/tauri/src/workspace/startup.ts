import type { TetherApi } from "../api/types";

const ready = new WeakMap<TetherApi, Promise<void>>();

/**
 * Resolves once sessions left over from a previous load of this webview
 * (a reload keeps the Rust side running) have been closed. The first call
 * starts `closeAllSessions`, so the app calls it on load (main.tsx); every
 * session waits for it before opening (useShell). A failure is logged and
 * does not block new sessions: the backend also reaps sessions whose
 * output channel is gone.
 */
export function sessionsReady(api: TetherApi): Promise<void> {
  let p = ready.get(api);
  if (!p) {
    p = Promise.resolve()
      .then(() => api.closeAllSessions())
      .then(
        (n) => {
          if (n > 0) console.debug(`closed ${n} session(s) left from a previous load`);
        },
        (e: unknown) => console.debug("close_all_sessions failed", e),
      );
    ready.set(api, p);
  }
  return p;
}
