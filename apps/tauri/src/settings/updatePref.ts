/**
 * Whether Termlyx looks for an update once after unlock (desktop only). Per
 * device, in this webview's localStorage like the auto-lock choice; anything
 * but a stored "false" reads as on.
 */

const STORAGE_KEY = "tether.update-check";

export function getAutoCheck(): boolean {
  try {
    return localStorage.getItem(STORAGE_KEY) !== "false";
  } catch {
    return true;
  }
}

export function setAutoCheck(on: boolean) {
  try {
    localStorage.setItem(STORAGE_KEY, String(on));
  } catch {
    // Storage unavailable: the default (on) stays.
  }
}
