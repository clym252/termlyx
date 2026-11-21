/**
 * Terminal preferences: theme, font family, font size, local shell, IP highlighting.
 *
 * The backend has no preference store yet (the vault holds records, not
 * app preferences), so they live in this webview's localStorage. They are
 * not secrets. Unreadable or invalid values fall back to the defaults.
 */

import { useSyncExternalStore } from "react";
import { DEFAULT_THEME_ID, isThemeId, type ThemeId } from "../themes";

export interface TerminalSettings {
  themeId: ThemeId;
  fontFamily: string;
  fontSize: number;
  /** The local shell chosen in Settings (an id from `api.localShells()`); null is the default shell. */
  localShellId: string | null;
  /** Draw IP addresses in output in the theme's highlight colour (display only). */
  highlightIps: boolean;
}

export const FONT_SIZE_MIN = 8;
export const FONT_SIZE_MAX = 32;

export const DEFAULT_TERMINAL_SETTINGS: TerminalSettings = {
  themeId: DEFAULT_THEME_ID,
  fontFamily: '"JetBrains Mono", monospace',
  fontSize: 13,
  localShellId: null,
  highlightIps: true,
};

const STORAGE_KEY = "tether.terminal-settings";

function validFontSize(n: unknown): n is number {
  return typeof n === "number" && Number.isInteger(n) && n >= FONT_SIZE_MIN && n <= FONT_SIZE_MAX;
}

function validShellId(v: unknown): v is string {
  return typeof v === "string" && v !== "";
}

// The default before the font picker: nobody chose it, so it moves to the new default.
const OLD_DEFAULT_FONT = 'Menlo, Monaco, "Cascadia Mono", Consolas, "Courier New", monospace';

function load(): TerminalSettings {
  try {
    const raw = JSON.parse(localStorage.getItem(STORAGE_KEY) ?? "null") as Partial<TerminalSettings> | null;
    if (!raw || typeof raw !== "object") return DEFAULT_TERMINAL_SETTINGS;
    return {
      themeId: isThemeId(raw.themeId) ? raw.themeId : DEFAULT_TERMINAL_SETTINGS.themeId,
      fontFamily:
        typeof raw.fontFamily === "string" && raw.fontFamily.trim() && raw.fontFamily !== OLD_DEFAULT_FONT
          ? raw.fontFamily
          : DEFAULT_TERMINAL_SETTINGS.fontFamily,
      fontSize: validFontSize(raw.fontSize) ? raw.fontSize : DEFAULT_TERMINAL_SETTINGS.fontSize,
      localShellId: validShellId(raw.localShellId) ? raw.localShellId : null,
      highlightIps: typeof raw.highlightIps === "boolean" ? raw.highlightIps : DEFAULT_TERMINAL_SETTINGS.highlightIps,
    };
  } catch {
    return DEFAULT_TERMINAL_SETTINGS;
  }
}

let current: TerminalSettings | null = null;
const listeners = new Set<() => void>();

export function getTerminalSettings(): TerminalSettings {
  current ??= load();
  return current;
}

/** Merges `patch` (invalid values are ignored), saves, and notifies subscribers. */
export function updateTerminalSettings(patch: Partial<TerminalSettings>) {
  const prev = getTerminalSettings();
  const next: TerminalSettings = {
    themeId: patch.themeId !== undefined && isThemeId(patch.themeId) ? patch.themeId : prev.themeId,
    fontFamily: patch.fontFamily !== undefined && patch.fontFamily.trim() ? patch.fontFamily : prev.fontFamily,
    fontSize: patch.fontSize !== undefined && validFontSize(patch.fontSize) ? patch.fontSize : prev.fontSize,
    localShellId:
      patch.localShellId === null || validShellId(patch.localShellId) ? patch.localShellId : prev.localShellId,
    highlightIps: typeof patch.highlightIps === "boolean" ? patch.highlightIps : prev.highlightIps,
  };
  if (
    next.themeId === prev.themeId &&
    next.fontFamily === prev.fontFamily &&
    next.fontSize === prev.fontSize &&
    next.localShellId === prev.localShellId &&
    next.highlightIps === prev.highlightIps
  ) {
    return;
  }
  current = next;
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(next));
  } catch {
    // Storage unavailable: the change still applies for this session.
  }
  listeners.forEach((l) => l());
}

export function subscribeTerminalSettings(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

export function useTerminalSettings(): TerminalSettings {
  return useSyncExternalStore(subscribeTerminalSettings, getTerminalSettings);
}
