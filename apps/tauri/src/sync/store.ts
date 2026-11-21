/**
 * The sync status for the UI: one subscription to the backend's `sync-status`
 * event shared by the sidebar dot and the Settings section. It starts with
 * the first component that reads it and stops with the last.
 */

import { useSyncExternalStore } from "react";
import { api } from "../api";
import type { SyncStatus, Unlisten } from "../api/types";
import { describeError } from "../ui/errors";

export interface SyncSnapshot {
  /** Null until the first answer. */
  sync: SyncStatus | null;
  /** The status could not be read. */
  error: string | null;
}

let snapshot: SyncSnapshot = { sync: null, error: null };
const subscribers = new Set<() => void>();
let stop: (() => void) | null = null;
let epoch = 0;

function publish(next: SyncSnapshot) {
  snapshot = next;
  for (const cb of [...subscribers]) cb();
}

function start() {
  const mine = ++epoch;
  let off: Unlisten | undefined;
  let heard = false;
  void api.sync
    .onStatus((s) => {
      if (epoch !== mine) return;
      heard = true;
      publish({ sync: s, error: null });
    })
    .then((u) => {
      // Stopped before the subscription was ready: drop it again.
      if (epoch === mine) off = u;
      else u();
    });
  api.sync.status().then(
    (s) => {
      // An event that came first is newer than this answer.
      if (epoch === mine && !heard) publish({ sync: s, error: null });
    },
    (e) => {
      if (epoch === mine && !heard) publish({ sync: null, error: describeError(e) });
    },
  );
  stop = () => {
    epoch += 1;
    off?.();
    stop = null;
    snapshot = { sync: null, error: null };
  };
}

function subscribe(cb: () => void): () => void {
  subscribers.add(cb);
  if (!stop) start();
  return () => {
    subscribers.delete(cb);
    if (subscribers.size === 0) stop?.();
  };
}

/** The current sync status; re-renders on every change. */
export function useSyncStatus(): SyncSnapshot {
  return useSyncExternalStore(subscribe, () => snapshot);
}
