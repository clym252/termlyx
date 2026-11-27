import { describe, expect, it } from "vitest";
import { isApiError } from "../api/types";
import {
  describeSync,
  hostPort,
  needsSignIn,
  problemFor,
  problemFromCheckError,
  problemFromError,
  problemFromState,
  unreachableReason,
} from "./status";

const at = (ms: number) => `T${ms}`;

describe("describeSync", () => {
  it("is quiet when sync is off", () => {
    expect(describeSync({ state: "off" }, at)).toMatchObject({ tone: "off", label: "Sync is off", detail: null });
  });

  it("says when it last synced", () => {
    const v = describeSync({ state: "idle", lastSync: 1234 }, at);
    expect(v).toEqual({ tone: "idle", label: "Synced", detail: "Last sync T1234", title: "Synced · Last sync T1234" });
  });

  it("does not claim a time before the first sync", () => {
    expect(describeSync({ state: "idle", lastSync: 0 }, at).detail).toBe("Not synced yet");
  });

  it("shows progress only when the total is known", () => {
    expect(describeSync({ state: "syncing", done: 3, total: 10 }, at)).toMatchObject({
      tone: "syncing",
      label: "Syncing…",
      detail: "3 of 10",
    });
    expect(describeSync({ state: "syncing", done: 0, total: null }, at).detail).toBeNull();
  });

  it("gives the reason when offline", () => {
    expect(describeSync({ state: "offline", reason: "server unreachable" }, at)).toMatchObject({
      tone: "offline",
      label: "Offline",
      title: "Offline · server unreachable",
    });
  });

  it("names the partial case and carries its reason", () => {
    const v = describeSync({ state: "error", kind: "partial", message: "2 items could not be decrypted" }, at);
    expect(v.tone).toBe("error");
    expect(v.label).toBe("Some items didn't sync");
    expect(v.title).toBe("Some items didn't sync: 2 items could not be decrypted");
  });

  it("is an error for every other failure, revoked named as such", () => {
    expect(describeSync({ state: "error", kind: "server", message: "server error" }, at)).toMatchObject({
      tone: "error",
      label: "Sync error",
      detail: "server error",
    });
    expect(describeSync({ state: "error", kind: "revoked", message: "device revoked" }, at).label).toBe("Device revoked");
  });
});

describe("problems", () => {
  it("warns loudly about a changed certificate and says when to continue", () => {
    const p = problemFor("certificateChanged", "server certificate changed");
    expect(p.severe).toBe(true);
    expect(p.hint).toContain("Do not continue unless you rebuilt the server");
  });

  it("warns loudly when the server asks for weaker password protection", () => {
    const e = { code: "weakKdf", message: "the server asked for weaker password protection", refs: [] };
    const p = problemFromError(e);
    expect(p.severe).toBe(true);
    expect(p.hint).toContain("Your password was not sent");
  });

  it("tells the user to update Termlyx for both spellings of the error", () => {
    for (const code of ["updateRequired", "updateClient"]) {
      expect(problemFor(code, "m").hint).toContain("update Termlyx");
    }
    expect(problemFor("updateServer", "m").hint).toContain("server");
  });

  it("adds nothing to plain failures", () => {
    expect(problemFor("unreachable", "server unreachable (timed out)")).toEqual({
      message: "server unreachable (timed out)",
      hint: null,
      severe: false,
    });
  });

  it("names the code's server and the reason when it cannot be reached", () => {
    const e = { code: "unreachable", message: "server unreachable (DNS lookup failed)", refs: [] };
    expect(problemFromError(e, "https://nas.local:8443").message).toBe(
      "Can't reach nas.local:8443 (DNS lookup failed). Is the server running, and is this device on the same network?",
    );
    // The scheme's own port is left out; a reason-less message adds none.
    expect(problemFor("unreachable", "server unreachable", "https://sync.example.com").message).toMatch(
      /^Can't reach sync\.example\.com\. /,
    );
    expect(problemFromCheckError({ ...e, message: "server unreachable (TLS: bad cert)" }, "[fd00::5]").message).toMatch(
      /^Can't reach \[fd00::5\]:8443 \(TLS: bad cert\)\. /,
    );
    expect(unreachableReason("server unreachable (timed out)")).toBe("timed out");
    expect(unreachableReason("raw backend text")).toBeNull();
  });

  it("reads a rejected command through its code", () => {
    const e = { code: "wrongPassword", message: "wrong password", refs: [] };
    expect(isApiError(e)).toBe(true);
    expect(problemFromError(e).message).toBe("Wrong master password.");
    expect(problemFromError(new Error("boom")).message).toBe("boom");
    expect(problemFromError({ code: "certificateChanged", message: "server certificate changed", refs: [] }).severe).toBe(
      true,
    );
  });

  it("has a problem only for the error state", () => {
    expect(problemFromState({ state: "idle", lastSync: 1 })).toBeNull();
    expect(problemFromState({ state: "error", kind: "certificateChanged", message: "m" })?.severe).toBe(true);
  });

  it("asks for the password when the session is gone", () => {
    const state = { state: "error", kind: "signInAgain", message: "not authorized; sign in again" } as const;
    expect(needsSignIn(state)).toBe(true);
    expect(needsSignIn({ state: "error", kind: "revoked", message: "device revoked" })).toBe(false);
    expect(describeSync(state, at).label).toBe("Sign in again");
    expect(problemFromState(state)?.hint).toContain("master password");
    expect(problemFor("needsDeviceCode", "m").hint).toContain("add-device code");
  });
});

describe("hostPort", () => {
  it("shows the normalized host:port", () => {
    expect(hostPort("192.168.1.20")).toBe("192.168.1.20:8443");
    expect(hostPort("https://nas.local/")).toBe("nas.local:8443");
    expect(hostPort("nas.local:9000")).toBe("nas.local:9000");
    expect(hostPort("fd00::5")).toBe("[fd00::5]:8443");
    expect(hostPort("[fd00::5]:9000")).toBe("[fd00::5]:9000");
  });
});
