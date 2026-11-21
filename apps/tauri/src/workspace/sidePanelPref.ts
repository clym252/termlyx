/**
 * The terminal side panel (Snippets, Themes): open or closed, and which tab.
 *
 * Per device, in this webview's localStorage like the sidebar mode (not a
 * secret, not synced). On the phone layout the panel is a sheet that starts
 * closed whatever was stored.
 */

import { useSyncExternalStore } from "react";

export type PanelTab = "snippets" | "themes";

export interface SidePanelState {
  open: boolean;
  tab: PanelTab;
}

const STORAGE_KEY = "tether.side-panel";
const DEFAULT: SidePanelState = { open: false, tab: "snippets" };

function load(): SidePanelState {
  try {
    const raw = JSON.parse(localStorage.getItem(STORAGE_KEY) ?? "null") as Partial<SidePanelState> | null;
    if (!raw || typeof raw.open !== "boolean" || (raw.tab !== "snippets" && raw.tab !== "themes")) return DEFAULT;
    return { open: raw.open, tab: raw.tab };
  } catch {
    return DEFAULT;
  }
}

let current: SidePanelState | null = null;
const listeners = new Set<() => void>();

export function getSidePanel(): SidePanelState {
  current ??= load();
  return current;
}

export function setSidePanel(next: SidePanelState) {
  const prev = getSidePanel();
  if (prev.open === next.open && prev.tab === next.tab) return;
  current = { open: next.open, tab: next.tab };
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(current));
  } catch {
    // Storage unavailable: the change still applies for this session.
  }
  listeners.forEach((l) => l());
}

/** Opens the panel on `tab`. */
export const openSidePanel = (tab: PanelTab) => setSidePanel({ open: true, tab });

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

export function useSidePanel(): SidePanelState {
  return useSyncExternalStore(subscribe, getSidePanel);
}
