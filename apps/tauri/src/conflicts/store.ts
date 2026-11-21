/**
 * The conflicts for the UI: one shared list behind the sidebar badge and the
 * Conflicts page. It is read again whenever the engine reports (`sync-status`:
 * conflicts arrive with a sync), when the vault unlocks, and after any
 * resolve (`refreshConflicts`). The list holds no secrets: those are masked
 * by the backend and fetched one at a time by the page.
 */

import { useSyncExternalStore } from "react";
import { api } from "../api";
import type { Conflict, Unlisten } from "../api/types";
import { describeError } from "../ui/errors";

export interface ConflictsSnapshot {
  /** Null until read, or while the vault is locked. */
  conflicts: Conflict[] | null;
  /** The last read failed (the list, if any, is from before). */
  error: string | null;
}

let snapshot: ConflictsSnapshot = { conflicts: null, error: null };
const subscribers = new Set<() => void>();
let off: Unlisten | null = null;
let listening = 0;
/** Identifies the current event subscription. */
let epoch = 0;
/** Bumped by `clearConflicts`, so a read that started before it is dropped. */
let generation = 0;
let reading = false;
let again = false;

function publish(next: ConflictsSnapshot) {
  snapshot = next;
  for (const cb of [...subscribers]) cb();
}

/**
 * Reads the list. Overlapping calls coalesce into one more read after the
 * running one, so a burst of status events costs two reads at most.
 */
export async function refreshConflicts(): Promise<void> {
  if (reading) {
    again = true;
    return;
  }
  reading = true;
  try {
    do {
      again = false;
      const mine = generation;
      try {
        const conflicts = await api.conflicts.list();
        if (generation === mine) publish({ conflicts, error: null });
      } catch (e) {
        if (generation === mine) publish({ conflicts: snapshot.conflicts, error: describeError(e) });
      }
    } while (again);
  } finally {
    reading = false;
  }
}

/** Forgets the list (the vault locked); a read in flight is dropped. */
export function clearConflicts() {
  generation += 1;
  publish({ conflicts: null, error: null });
}

function subscribe(cb: () => void): () => void {
  subscribers.add(cb);
  if (listening++ === 0) {
    const mine = ++epoch;
    void api.sync.onStatus(() => void refreshConflicts()).then((u) => {
      // Everyone left before the subscription was ready: drop it again.
      if (epoch === mine) off = u;
      else u();
    });
  }
  return () => {
    subscribers.delete(cb);
    if (--listening === 0) {
      epoch += 1;
      off?.();
      off = null;
    }
  };
}

/** The conflicts; re-renders on every change. Reads nothing by itself: `refreshConflicts` does. */
export function useConflicts(): ConflictsSnapshot {
  return useSyncExternalStore(subscribe, () => snapshot);
}
