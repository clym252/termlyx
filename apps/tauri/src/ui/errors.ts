import { isApiError } from "../api/types";

/** A message for the user from whatever a command rejected with. */
export function describeError(e: unknown): string {
  if (isApiError(e)) {
    switch (e.code) {
      case "wrongPassword":
        return "Wrong master password.";
      case "wrongRecoveryKey":
        return "That recovery key is wrong.";
      case "locked":
        return "The vault is locked.";
      default:
        return e.message;
    }
  }
  return e instanceof Error ? e.message : String(e);
}
