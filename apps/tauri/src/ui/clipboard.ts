import { api } from "../api";

/** Copies text to the clipboard; false when the webview refuses. */
export async function copyText(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    return false;
  }
}

/** How long a copied secret stays on the clipboard. */
export const SECRET_CLIPBOARD_MS = 60_000;

/**
 * Clears the clipboard after `ms` if it still holds `text` (a copied
 * secret); anything copied since is left alone. The text is kept only in
 * this closure, never in React state.
 */
export function clearClipboardLater(text: string, ms = SECRET_CLIPBOARD_MS): void {
  setTimeout(() => {
    void api.readClipboard().then((now) => {
      if (now === text) void api.writeClipboard("");
    });
  }, ms);
}
