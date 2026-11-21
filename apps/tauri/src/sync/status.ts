import { isApiError, type SyncState } from "../api/types";
import { describeError } from "../ui/errors";
import { APP_NAME } from "../app/brand";

/** What the status dot looks like: one tone per state; `off` is not drawn. */
export type SyncTone = "off" | "idle" | "syncing" | "offline" | "error";

export interface SyncView {
  tone: SyncTone;
  /** A few words: "Synced", "Offline". */
  label: string;
  /** The specifics: when it last synced, how far along, why it failed. */
  detail: string | null;
  /** Tooltip text: label and detail in one line. */
  title: string;
}

/** A time for the user's eyes. */
export function formatTime(ms: number): string {
  return new Date(ms).toLocaleString();
}

/** The words for an engine state (pure: the clock is the caller's). */
export function describeSync(state: SyncState, format: (ms: number) => string = formatTime): SyncView {
  const view = (tone: SyncTone, label: string, detail: string | null, joiner = " · "): SyncView => ({
    tone,
    label,
    detail,
    title: detail ? `${label}${joiner}${detail}` : label,
  });
  switch (state.state) {
    case "off":
      return view("off", "Sync is off", null);
    case "idle":
      return view("idle", "Synced", state.lastSync > 0 ? `Last sync ${format(state.lastSync)}` : "Not synced yet");
    case "syncing":
      return view("syncing", "Syncing…", state.total === null ? null : `${state.done} of ${state.total}`);
    case "offline":
      return view("offline", "Offline", state.reason);
    case "error":
      if (state.kind === "partial") return view("error", "Some items didn't sync", state.message, ": ");
      return view(
        "error",
        state.kind === "revoked" ? "Device revoked" : state.kind === "signInAgain" ? "Sign in again" : "Sync error",
        state.message,
      );
  }
}

/** An error to show: its plain message, advice where the user can act, and whether to warn loudly. */
export interface Problem {
  message: string;
  hint: string | null;
  /** A possible attack or a dead end: styled as a warning, not a line of red text. */
  severe: boolean;
}

/**
 * The advice for a sync failure. `code` is an `ErrorCode` or a `SyncErrorKind`; the two overlap
 * on `certificateChanged` and `revoked` and differ on "update Termlyx" (`updateRequired` /
 * `updateClient`).
 */
export function problemFor(code: string, message: string): Problem {
  switch (code) {
    case "certificateChanged":
      return {
        message,
        hint: "Do not continue unless you rebuilt the server. If you did not, someone may be intercepting the connection.",
        severe: true,
      };
    case "weakKdf":
      return {
        message,
        hint: "Your password was not sent. The server may be compromised or misconfigured: do not sign in to it until its owner has checked it.",
        severe: true,
      };
    case "updateRequired":
    case "updateClient":
      return { message, hint: `Please update ${APP_NAME} on this device, then try again.`, severe: false };
    case "updateServer":
      return { message, hint: `Update the ${APP_NAME} server, then try again.`, severe: false };
    case "revoked":
      return {
        message,
        hint: "This device was removed from the account. Turn off sync here, then join again with a new add-device code. Your records on this device are kept.",
        severe: false,
      };
    case "signInAgain":
      return { message, hint: "Sign in again with your master password to carry on syncing.", severe: false };
    case "needsDeviceCode":
      return {
        message,
        hint: "Turn off sync here, then join again with an add-device code from another device (or from `termlyx-server link USERNAME --url ADDRESS` on the server). Your records on this device are kept.",
        severe: false,
      };
    default:
      return { message, hint: null, severe: false };
  }
}

/** A typed address as `host:port` (8443 unless typed; IPv6 keeps its brackets). */
export function hostPort(typed: string): string {
  const t = typed.trim().replace(/^https:\/\//i, "").replace(/\/$/, "");
  const v6 = /^\[([^\]]+)\](?::(\d+))?$/.exec(t);
  if (v6) return `[${v6[1]}]:${v6[2] ?? "8443"}`;
  if ((t.match(/:/g) ?? []).length > 1) return `[${t}]:8443`;
  return t.includes(":") ? t : `${t}:8443`;
}

/**
 * The problem a failed server check stands for: the plain sentences of the self-host design, with
 * the address as typed. Codes with no sentence of their own keep the backend's message.
 */
export function problemFromCheckError(e: unknown, address: string): Problem {
  const plain = (message: string, severe = false): Problem => ({ message, hint: null, severe });
  switch (isApiError(e) ? e.code : "") {
    case "invalidAddress":
      return plain("That server address isn't valid. Use an address like 192.168.1.20, nas.local or nas.local:8443.");
    case "unreachable":
      return plain(`Can't reach ${hostPort(address)}. Is the server running, and is this device on the same network?`);
    case "protocol":
      return plain("That address answers, but it is not a Termlyx server. Check the address and port.");
    case "invalidCode":
      return plain("That code is not valid on this server. Get the setup code from the server's log, or a new invite.");
    case "serverUnverified":
      return plain("Couldn't verify this server. Make sure the address is your Termlyx server, not another device.", true);
    case "certificateChanged":
      return plain(
        "This server's certificate changed since you connected. If you reinstalled the server, connect again with a new code.",
        true,
      );
    default:
      return problemFromError(e);
  }
}

/** Whether sync is waiting for the master password to sign this device in again. */
export function needsSignIn(state: SyncState): boolean {
  return state.state === "error" && state.kind === "signInAgain";
}

/** The problem a rejected command stands for. */
export function problemFromError(e: unknown): Problem {
  return problemFor(isApiError(e) ? e.code : "", describeError(e));
}

/** The problem an `error` state stands for (null for the other states). */
export function problemFromState(state: SyncState): Problem | null {
  return state.state === "error" ? problemFor(state.kind, state.message) : null;
}
