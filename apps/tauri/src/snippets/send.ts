import type { Snippet } from "../api/types";
import { safePasteText } from "../workspace/safePaste";

/** What sending needs of a terminal (xterm's `Terminal`). */
interface Target {
  paste(text: string): void;
  input(data: string, wasUserInput?: boolean): void;
}

/**
 * Sends a snippet as a paste, so bracketed paste keeps a multi-line command
 * from running line by line; `run` presses Enter once after it. Both go out
 * as the user's input, so a broadcasting tab sends them to its other panes.
 */
export function sendSnippet(term: Target, snippet: Snippet) {
  term.paste(safePasteText(snippet.text));
  if (snippet.run) term.input("\r", true);
}

/** Selected terminal text as a snippet's command: without the line breaks a selection picks up at its end. */
export function snippetTextOf(selection: string): string {
  return selection.replace(/[\r\n]+$/, "");
}
