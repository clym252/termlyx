/**
 * Whether the sidebar shows in full, as an icon rail, or not at all (hidden).
 *
 * Per device, like the terminal settings: it lives in this webview's
 * localStorage (not a secret, not synced). An unreadable or unknown stored
 * value falls back to the default.
 */

import { useSyncExternalStore } from "react";
import { FORM_FACTOR } from "../platform";

export type SidebarMode = "full" | "rail" | "hidden";

const STORAGE_KEY = "tether.sidebar";
/** The most recent visible mode, so showing a hidden sidebar returns to it. */
const VISIBLE_KEY = "tether.sidebar.visible";

const ORDER: SidebarMode[] = ["full", "rail", "hidden"];

/** full -> rail -> hidden -> full. */
export function nextSidebarMode(m: SidebarMode): SidebarMode {
  return ORDER[(ORDER.indexOf(m) + 1) % ORDER.length];
}

/** What a device that has never chosen gets. The one place a platform default (the iPad rail) goes. */
function defaultMode(): SidebarMode {
  return FORM_FACTOR === "tablet" ? "rail" : "full";
}

function isMode(v: unknown): v is SidebarMode {
  return v === "full" || v === "rail" || v === "hidden";
}

function visibleDefault(): "full" | "rail" {
  const d = defaultMode();
  return d === "hidden" ? "full" : d;
}

let lastVisible: "full" | "rail" | null = null;

/** The most recent non-hidden mode set (the default when none was). */
export function lastVisibleSidebarMode(): "full" | "rail" {
  if (lastVisible) return lastVisible;
  try {
    const raw = localStorage.getItem(VISIBLE_KEY);
    if (raw === "full" || raw === "rail") return (lastVisible = raw);
  } catch {
    // Unreadable: fall through to what the mode itself says.
  }
  const m = getSidebarMode();
  return (lastVisible = m === "hidden" ? visibleDefault() : m);
}

function load(): SidebarMode {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    return isMode(raw) ? raw : defaultMode();
  } catch {
    return defaultMode();
  }
}

let current: SidebarMode | null = null;
const listeners = new Set<() => void>();

export function getSidebarMode(): SidebarMode {
  current ??= load();
  return current;
}

/** Saves the mode and notifies subscribers; a value that is not a mode, or the current one, does nothing. */
export function setSidebarMode(mode: SidebarMode) {
  if (!isMode(mode) || mode === getSidebarMode()) return;
  current = mode;
  if (mode !== "hidden") lastVisible = mode;
  try {
    localStorage.setItem(STORAGE_KEY, mode);
    if (mode !== "hidden") localStorage.setItem(VISIBLE_KEY, mode);
  } catch {
    // Storage unavailable: the change still applies for this session.
  }
  listeners.forEach((l) => l());
}

export function subscribeSidebarMode(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

export function useSidebarMode(): [SidebarMode, (mode: SidebarMode) => void] {
  return [useSyncExternalStore(subscribeSidebarMode, getSidebarMode), setSidebarMode];
}
