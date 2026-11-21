import type { PaneControls } from "./Workspace";

const WAIT_MS = 50;
const MAX_TRIES = 20;

/**
 * Keyboard focus into a pane: the terminal of a session, else the first host of a launcher pane (its first
 * button when there are none). A launcher lists its hosts a moment after it appears, so this waits for that.
 */
export function focusPane(paneId: string, controls: (paneId: string) => PaneControls | undefined, tries = 0) {
  const c = controls(paneId);
  if (c) return c.focus();
  const pane = [...document.querySelectorAll<HTMLElement>("[data-pane-id][data-launcher]")].find((el) => el.dataset.paneId === paneId);
  if (!pane) return;
  const loaded = pane.querySelector(".launcher[data-loaded]");
  if (!loaded && tries < MAX_TRIES) {
    setTimeout(() => focusPane(paneId, controls, tries + 1), WAIT_MS);
    return;
  }
  (pane.querySelector<HTMLElement>(".launcher-hosts button") ?? pane.querySelector<HTMLElement>("button"))?.focus();
}
