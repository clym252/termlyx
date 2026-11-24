/**
 * Auto-lock: how long Tether may stay away from the front (window unfocused
 * or hidden; on iOS, the app in the background) before the vault locks.
 * `null` is Never. The backend keeps the clock (`autolock.rs`); this is the
 * device's choice, sent to it with `api.setAutoLock` at start-up and on
 * change.
 *
 * Per device, like the terminal settings: it lives in this webview's
 * localStorage (not a secret, not synced). An unreadable or unknown stored
 * value falls back to the default.
 */

import { useSyncExternalStore } from "react";
import { describeError } from "../ui/errors";

export const AUTO_LOCK_CHOICES = [null, 5, 15, 30, 60] as const;

/** Minutes away before the vault locks; `null`: never. */
export type AutoLockMinutes = (typeof AUTO_LOCK_CHOICES)[number];

const STORAGE_KEY = "tether.auto-lock";
/** How Never is stored. */
const NEVER = "never";

/** What a device that has never chosen gets: 30 minutes on every platform. */
function defaultAutoLock(): AutoLockMinutes {
  return 30;
}

export function autoLockLabel(minutes: AutoLockMinutes): string {
  if (minutes === null) return "Never";
  if (minutes === 60) return "After 1 hour";
  return `After ${minutes} minutes`;
}

function isChoice(v: unknown): v is AutoLockMinutes {
  return (AUTO_LOCK_CHOICES as readonly unknown[]).includes(v);
}

/** The stored form of a choice: `never` or its minutes. */
export function autoLockValue(minutes: AutoLockMinutes): string {
  return minutes === null ? NEVER : String(minutes);
}

/** The choice a stored form names, or `undefined` for anything else. */
export function parseAutoLock(raw: string | null): AutoLockMinutes | undefined {
  if (raw === NEVER) return null;
  if (raw === null || !/^\d+$/.test(raw)) return undefined;
  const n = Number(raw);
  return isChoice(n) ? n : undefined;
}

function load(): AutoLockMinutes {
  try {
    // Not `??`: a stored Never (`null`) must not fall back to the default.
    const stored = parseAutoLock(localStorage.getItem(STORAGE_KEY));
    return stored === undefined ? defaultAutoLock() : stored;
  } catch {
    return defaultAutoLock();
  }
}

let current: AutoLockMinutes | undefined;
/** Why the backend did not take the choice (it then runs with Never); `null` once it did. */
let applyError: string | null = null;
const listeners = new Set<() => void>();

export function getAutoLock(): AutoLockMinutes {
  if (current === undefined) current = load();
  return current;
}

/** Saves the choice and notifies subscribers; a value that is not a choice, or the current one, does nothing. */
export function setAutoLockPref(minutes: AutoLockMinutes) {
  if (!isChoice(minutes) || minutes === getAutoLock()) return;
  current = minutes;
  try {
    localStorage.setItem(STORAGE_KEY, autoLockValue(minutes));
  } catch {
    // Storage unavailable: the change still applies for this session.
  }
  listeners.forEach((l) => l());
}

export function subscribeAutoLock(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

export function useAutoLockPref(): AutoLockMinutes {
  return useSyncExternalStore(subscribeAutoLock, getAutoLock);
}

/**
 * Sends the device's choice to the backend (Gate, at start-up; Settings,
 * "Try again"), trying once more on failure. A second failure is kept for
 * Settings → Security to show: the backend would otherwise run with Never
 * while the select shows the choice. True when the backend took it.
 */
export async function applyAutoLock(send: (minutes: AutoLockMinutes) => Promise<void>): Promise<boolean> {
  const minutes = getAutoLock();
  let error: unknown = null;
  for (let attempt = 0; attempt < 2; attempt++) {
    try {
      await send(minutes);
      noteAutoLockApplied(null);
      return true;
    } catch (e) {
      error = e;
    }
  }
  noteAutoLockApplied(describeError(error));
  return false;
}

/** Records whether the backend took the choice: `null` it did, else why not. */
export function noteAutoLockApplied(error: string | null) {
  if (error === applyError) return;
  applyError = error;
  listeners.forEach((l) => l());
}

export function getAutoLockError(): string | null {
  return applyError;
}

export function useAutoLockError(): string | null {
  return useSyncExternalStore(subscribeAutoLock, getAutoLockError);
}
