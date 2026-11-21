/**
 * Android's system Back: one step back, never closing the app or a session.
 * Order: popup menu, dialog or sheet, docked panel, drawer, terminal, Hosts, a page,
 * Hosts, then the background (as Home does).
 */

export interface BackState {
  /** A popup menu (`role=menu`: a row's "⋯", the session menu, the terminal's selection menu) is open. */
  menuOpen: boolean;
  /** A modal `<dialog>` is open (the phone drawer is not one: it is `drawerOpen`). */
  dialogOpen: boolean;
  /** The docked Snippets/Themes panel is open (phone). */
  panelOpen: boolean;
  drawerOpen: boolean;
  /** The full-screen terminal is showing. */
  terminalShown: boolean;
  /** The page behind it ("hosts" is the start page). */
  route: string;
}

export type BackAction = "closeMenu" | "closeDialog" | "closePanel" | "closeDrawer" | "toHosts" | "background";

export function backAction(s: BackState): BackAction {
  if (s.menuOpen) return "closeMenu";
  if (s.dialogOpen) return "closeDialog";
  if (s.panelOpen) return "closePanel";
  if (s.drawerOpen) return "closeDrawer";
  if (s.terminalShown || s.route !== "hosts") return "toHosts";
  return "background";
}

/** Closes the open popup menu the way every one closes itself: they all listen for Escape on `window`. */
export function closeMenu() {
  window.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
}

/**
 * Closes the topmost modal `<dialog>` the way Escape does. A script cannot press Escape for the browser
 * (a synthetic key does not close a dialog), so it sends the `cancel` event the browser would: `Dialog`'s
 * `onCancel` runs its `onClose`, and a dialog without one (only its own buttons may close it) stays.
 */
export function closeTopDialog() {
  const open = document.querySelectorAll("dialog[open]");
  open[open.length - 1]?.dispatchEvent(new Event("cancel", { cancelable: true }));
}

let handler: (() => void) | null = null;

/** The app's step back while it is mounted (`Layout`); `null` when no page is up (setup, unlock screens). */
export function setBackHandler(h: (() => void) | null) {
  handler = h;
}

/** A Back press: the app's step, or the background when no page is up. */
export function pressBack(toBackground: () => void) {
  if (handler) handler();
  else toBackground();
}
