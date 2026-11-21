import { hasTarget, panesOf, type Tab } from "./model";
import type { PaneStatus } from "./PhoneTerminals";

/**
 * The panes broadcast typing from `fromPaneId` also goes to: the tab's other
 * panes whose session is open, in layout order. A launcher pane has no
 * session, and a pane still connecting (or dropped) would swallow or replay
 * the input later, so neither is ever a target.
 */
export function broadcastTargets(tab: Tab, fromPaneId: string, statuses: ReadonlyMap<string, PaneStatus>): string[] {
  return panesOf(tab.root)
    .filter((p) => p.id !== fromPaneId && hasTarget(p) && statuses.get(p.id) === "open")
    .map((p) => p.id);
}

const PASTE_START = "\x1b[200~";
const PASTE_END = "\x1b[201~";

/**
 * The text of `data` when it is one whole bracketed paste (as the source pane's xterm wrapped it for its own
 * application), else `null`. Each target pastes the text itself, wrapping it by its own mode: the markers
 * would reach a shell that never asked for bracketed paste as literal input.
 */
export function bracketedPasteText(data: string): string | null {
  if (!data.startsWith(PASTE_START) || !data.endsWith(PASTE_END)) return null;
  const inner = data.slice(PASTE_START.length, data.length - PASTE_END.length);
  // Markers inside are a hostile payload, not a second paste: the caller sanitizes the text (safePasteText).
  return inner;
}
