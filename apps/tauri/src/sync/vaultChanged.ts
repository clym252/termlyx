/**
 * Reading again when sync changes the vault. The backend sends
 * `vault-changed` (no payload) after it applied rows from another device;
 * every list that shows vault data reloads on it, so an open page does not
 * stay stale until the user leaves and comes back.
 *
 * One subscription is shared by all the lists: it starts with the first
 * `useVaultChanged` and stops with the last. A burst of events costs the
 * running reload plus at most one more, per list. The hook carries no data:
 * the callback re-reads through the usual commands, and editors keep their
 * own drafts (they take their initial values when they open).
 */

import { useEffect, useRef } from "react";
import { api } from "../api";
import type { Unlisten } from "../api/types";

const listeners = new Set<() => void>();
let off: Unlisten | null = null;
/** Identifies the current event subscription. */
let epoch = 0;

function notify() {
  for (const cb of [...listeners]) cb();
}

function subscribe(cb: () => void): () => void {
  listeners.add(cb);
  if (listeners.size === 1) {
    const mine = ++epoch;
    api.onVaultChanged(notify).then(
      (u) => {
        // Everyone left before the subscription was ready: drop it again.
        if (epoch !== mine) {
          u();
          return;
        }
        off = u;
        // A change that landed while the subscription was starting was
        // missed: read once more now that none can be.
        notify();
      },
      () => {}, // no events: lists keep what they have
    );
  }
  return () => {
    listeners.delete(cb);
    if (listeners.size === 0) {
      epoch += 1;
      off?.();
      off = null;
    }
  };
}

/**
 * Calls `reload` whenever sync changed the vault, while the component is
 * mounted. Overlapping events coalesce: while a reload runs, further events
 * cost one more run after it. `reload` should handle its own errors; one
 * that throws or rejects is ignored.
 */
export function useVaultChanged(reload: () => unknown): void {
  const latest = useRef(reload);
  latest.current = reload;

  useEffect(() => {
    let live = true;
    let running = false;
    let again = false;
    async function run() {
      if (running) {
        again = true;
        return;
      }
      running = true;
      try {
        do {
          again = false;
          try {
            await latest.current();
          } catch {
            // The caller shows its own errors.
          }
        } while (again && live);
      } finally {
        running = false;
      }
    }
    const unsubscribe = subscribe(() => void run());
    return () => {
      live = false;
      unsubscribe();
    };
  }, []);
}
