import type { SessionTarget } from "../api/types";
import { getTerminalSettings } from "../settings/store";

/** A new local terminal, on the shell chosen in Settings → Terminal (null: the default). */
export function localTarget(): SessionTarget {
  return { kind: "local", shellId: getTerminalSettings().localShellId };
}
export const LOCAL_TITLE = "Local shell";
