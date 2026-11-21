import { safePasteText } from "../workspace/safePaste";

/** DEL, what Backspace sends. */
const DEL = "\x7f";

/**
 * The bytes that turn the text `before` into `after` on the remote line:
 * a DEL for each character after the common start, then the new tail.
 * A line break is typed as Enter.
 */
export function imeDelta(before: string, after: string): string {
  let same = 0;
  while (same < before.length && same < after.length && before[same] === after[same]) same++;
  // Committed text (a keyboard's clipboard chip, voice) may hold ESC or other controls: never into the session.
  return DEL.repeat(before.length - same) + safePasteText(after.slice(same)).replace(/\r?\n/g, "\r");
}

/** Keyboard events xterm must not see while this runs: it would compose, then send each word again. */
const COMPOSITION = ["compositionstart", "compositionupdate", "compositionend"] as const;

/**
 * Android: software keyboards (Samsung Keyboard, Gboard) compose each word,
 * and xterm sends a word only when its composition ends, so the cursor waits
 * at the word's start. Here every change to xterm's textarea goes out as it
 * happens instead (`imeDelta` of the value before and after), whatever made
 * it: a key, the keyboard taking a letter back, voice, a picked suggestion.
 *
 * Listens on `container` in the capture phase, so xterm's own listeners on
 * `textarea` never get the composition events, the keyboard's placeholder
 * keydowns (keyCode 229) or the input events. Real keys (Backspace, Enter,
 * arrows) still reach xterm. Returns the detach function.
 */
export function attachImeInput(
  container: HTMLElement,
  textarea: HTMLTextAreaElement,
  send: (data: string) => void,
): () => void {
  let before = textarea.value;
  const mine = (ev: Event) => ev.target === textarea;
  const swallow = (ev: Event) => {
    if (mine(ev)) ev.stopPropagation();
  };
  const onKeyDown = (ev: KeyboardEvent) => {
    if (!mine(ev)) return;
    if (ev.keyCode === 229) {
      ev.stopPropagation();
      return;
    }
    // A real key goes to xterm and changes the remote line, not the field: empty the field, or the
    // keyboard re-opens the text left in it (behind a Backspace) as the next word to compose.
    textarea.value = "";
    before = "";
  };
  const onBeforeInput = (ev: Event) => {
    if (mine(ev)) before = textarea.value;
  };
  const onInput = (ev: Event) => {
    if (!mine(ev)) return;
    ev.stopPropagation();
    const data = imeDelta(before, textarea.value);
    before = textarea.value;
    if (data) send(data);
  };

  for (const t of COMPOSITION) container.addEventListener(t, swallow, true);
  container.addEventListener("keydown", onKeyDown, true);
  container.addEventListener("beforeinput", onBeforeInput, true);
  container.addEventListener("input", onInput, true);
  return () => {
    for (const t of COMPOSITION) container.removeEventListener(t, swallow, true);
    container.removeEventListener("keydown", onKeyDown, true);
    container.removeEventListener("beforeinput", onBeforeInput, true);
    container.removeEventListener("input", onInput, true);
  };
}
