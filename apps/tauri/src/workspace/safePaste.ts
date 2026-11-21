/**
 * Pasted or snippet text with ESC and the other C0/C1 control characters
 * removed (tab, newline and carriage return stay). xterm only wraps a paste in
 * `ESC[200~ … ESC[201~`; a payload holding its own `ESC[201~` would end it
 * early and the rest would run as typed input.
 */
// eslint-disable-next-line no-control-regex
const CONTROLS = /[\x00-\x08\x0b\x0c\x0e-\x1f\x7f-\x9f]/g;

export function safePasteText(text: string): string {
  return text.replace(CONTROLS, "");
}

/**
 * Pastes the clipboard's plain text sanitized, whatever way the paste starts
 * (Cmd/Ctrl+V, menu): xterm's own paste-event handler would otherwise wrap the
 * raw text. Capture phase on `container`, so xterm's listener never runs.
 * Returns the detach function.
 */
export function attachSafePaste(container: HTMLElement, term: { paste(text: string): void }): () => void {
  const onPaste = (e: Event) => {
    e.preventDefault();
    e.stopPropagation();
    const text = (e as ClipboardEvent).clipboardData?.getData("text/plain") ?? "";
    if (text) term.paste(safePasteText(text));
  };
  container.addEventListener("paste", onPaste, true);
  return () => container.removeEventListener("paste", onPaste, true);
}
