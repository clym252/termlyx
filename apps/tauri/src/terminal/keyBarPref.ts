/**
 * How much of the iOS key bar shows: both rows, one row, or only a small
 * "Show key bar" pill.
 *
 * Per device, like the sidebar mode: it lives in this webview's localStorage
 * (not a secret, not synced). An unreadable or unknown stored value falls
 * back to both rows.
 */

import { useSyncExternalStore } from "react";

export type KeyBarMode = "two" | "one" | "hidden";

const STORAGE_KEY = "tether.keyBar";
const DEFAULT_MODE: KeyBarMode = "two";

/** The bar's size toggle steps through these: two rows → one row → hidden → two rows. */
export function nextKeyBarMode(mode: KeyBarMode): KeyBarMode {
  return mode === "two" ? "one" : mode === "one" ? "hidden" : "two";
}

function isMode(v: unknown): v is KeyBarMode {
  return v === "two" || v === "one" || v === "hidden";
}

function load(): KeyBarMode {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    return isMode(raw) ? raw : DEFAULT_MODE;
  } catch {
    return DEFAULT_MODE;
  }
}

let current: KeyBarMode | null = null;
const listeners = new Set<() => void>();

export function getKeyBarMode(): KeyBarMode {
  current ??= load();
  return current;
}

/** Saves the mode and notifies subscribers; a value that is not a mode, or the current one, does nothing. */
export function setKeyBarMode(mode: KeyBarMode) {
  if (!isMode(mode) || mode === getKeyBarMode()) return;
  current = mode;
  try {
    localStorage.setItem(STORAGE_KEY, mode);
  } catch {
    // Storage unavailable: the change still applies for this session.
  }
  listeners.forEach((l) => l());
}

export function subscribeKeyBarMode(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

export function useKeyBarMode(): [KeyBarMode, (mode: KeyBarMode) => void] {
  return [useSyncExternalStore(subscribeKeyBarMode, getKeyBarMode), setKeyBarMode];
}

