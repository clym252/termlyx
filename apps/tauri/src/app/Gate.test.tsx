import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { VaultLocked, VaultStatus } from "../api/types";

const status = vi.hoisted(() => ({ current: null as VaultStatus | null }));
const backend = vi.hoisted(() => ({
  /** `vaultStatus` waits (from the next call) until `release` while set. */
  hang: false,
  release: [] as (() => void)[],
  front: [] as boolean[],
  /** What `appFront` answers. */
  frontAnswer: false,
  /** `appFront` fails instead. */
  frontFails: false,
  limits: [] as (number | null)[],
  /** `setAutoLock` fails this many more times. */
  limitFailures: 0,
  onLocked: null as ((e: VaultLocked) => void) | null,
}));

vi.mock("../api", () => ({
  api: {
    vaultStatus: () =>
      backend.hang
        ? new Promise((resolve) => backend.release.push(() => resolve(status.current)))
        : Promise.resolve(status.current),
    onVaultLocked: async (cb: (e: VaultLocked) => void) => {
      backend.onLocked = cb;
      return () => {};
    },
    setAutoLock: async (m: number | null) => {
      backend.limits.push(m);
      if (backend.limitFailures > 0) {
        backend.limitFailures -= 1;
        throw { code: "internal", message: "backend busy", refs: [] };
      }
    },
    appFront: async (front: boolean) => {
      backend.front.push(front);
      if (backend.frontFails) throw { code: "internal", message: "backend busy", refs: [] };
      return backend.frontAnswer;
    },
  },
}));
// The real Layout pulls in the whole app; only which view shows (and whether it is locked) matters here.
vi.mock("./Layout", () => ({ Layout: ({ locked }: { locked: boolean }) => <p>{locked ? "app layout (locked)" : "app layout"}</p> }));

import { getAutoLockError, noteAutoLockApplied, setAutoLockPref } from "../settings/autoLockPref";
import { Gate } from "./Gate";

beforeEach(() => {
  Object.assign(backend, { hang: false, release: [], front: [], frontAnswer: false, frontFails: false, limits: [], limitFailures: 0, onLocked: null });
});
afterEach(() => {
  cleanup();
  setAutoLockPref(null);
  noteAutoLockApplied(null);
  vi.restoreAllMocks();
});

function setStatus(state: VaultStatus["state"]) {
  status.current = { state, quickUnlockEnabled: false, quickUnlockKind: "none", error: null };
}

// The DEV throughput bench runs as Gate's `unlockedView`: its connection
// checks host keys against the vault, which a locked vault rejects.
describe("Gate unlockedView (bench)", () => {
  it("shows the unlocked view instead of the app once unlocked", async () => {
    setStatus("unlocked");
    render(<Gate unlockedView={<p>bench terminal</p>} />);
    expect(await screen.findByText("bench terminal")).toBeTruthy();
    expect(screen.queryByText("app layout")).toBeNull();
  });

  it("keeps the unlocked view behind the unlock screen while locked", async () => {
    setStatus("locked");
    render(<Gate unlockedView={<p>bench terminal</p>} />);
    expect(await screen.findByRole("button", { name: /unlock/i })).toBeTruthy();
    expect(screen.queryByText("bench terminal")).toBeNull();
  });

  it("shows the app when no unlocked view is given", async () => {
    setStatus("unlocked");
    render(<Gate />);
    expect(await screen.findByText("app layout")).toBeTruthy();
  });
});

describe("Gate auto-lock", () => {
  let visibility: DocumentVisibilityState = "visible";
  const setVisibility = (next: DocumentVisibilityState) => {
    visibility = next;
    document.dispatchEvent(new Event("visibilitychange"));
  };
  beforeEach(() => {
    visibility = "visible";
    vi.spyOn(document, "visibilityState", "get").mockImplementation(() => visibility);
  });

  it("sends this device's limit at start-up", async () => {
    setStatus("unlocked");
    render(<Gate />);
    await screen.findByText("app layout");
    expect(backend.limits).toEqual([null]);
    cleanup();

    setAutoLockPref(15);
    render(<Gate />);
    await screen.findByText("app layout");
    expect(backend.limits).toEqual([null, 15]);
  });

  it("tries the limit once more, then keeps the failure for Settings", async () => {
    setAutoLockPref(30);
    setStatus("unlocked");
    backend.limitFailures = 1;
    render(<Gate />);
    await screen.findByText("app layout");
    await vi.waitFor(() => expect(backend.limits).toEqual([30, 30]));
    expect(getAutoLockError()).toBeNull();
    cleanup();

    backend.limits = [];
    backend.limitFailures = 2;
    render(<Gate />);
    await screen.findByText("app layout");
    await vi.waitFor(() => expect(getAutoLockError()).toBe("backend busy"));
    expect(backend.limits).toEqual([30, 30]);
  });

  it("reports moves to and from the front", async () => {
    setStatus("unlocked");
    render(<Gate />);
    await screen.findByText("app layout");
    act(() => setVisibility("hidden"));
    act(() => setVisibility("visible"));
    expect(backend.front).toEqual([true, false, true]);
  });

  // A reload while in front sends no visibility change: without this the backend would read
  // the app as away and lock every limit while it is in use.
  it("tells the backend where the page is on every mount, shown or hidden", async () => {
    setStatus("unlocked");
    render(<Gate />);
    await screen.findByText("app layout");
    expect(backend.front).toEqual([true]);
    cleanup();

    backend.front = [];
    visibility = "hidden";
    render(<Gate />);
    await screen.findByText("app layout");
    expect(backend.front).toEqual([false]);
  });

  it("a locked answer at mount shows the lock, even after an unlocked status read", async () => {
    setStatus("unlocked");
    backend.frontAnswer = true;
    render(<Gate />);
    // The mount's status read already has its (unlocked) answer; the backend locks on the front report.
    setStatus("locked");
    expect(await screen.findByRole("heading", { name: "Unlock Termlyx" })).toBeTruthy();
  });

  it("a_locked_answer_on_return_shows_the_lock_before_the_status_is_read", async () => {
    setStatus("unlocked");
    render(<Gate />);
    await screen.findByText("app layout");
    act(() => setVisibility("hidden"));
    // The backend locked on the return; reading the status again takes its time.
    backend.frontAnswer = true;
    backend.hang = true;
    await act(async () => setVisibility("visible"));
    // Locked for the panes from the answer on; covered until the status is in.
    expect(screen.getByText("app layout (locked)")).toBeTruthy();
    expect(screen.getByRole("status").textContent).toBe("Loading…");

    setStatus("locked");
    await act(async () => backend.release.forEach((r) => r()));
    expect(screen.getByRole("heading", { name: "Unlock Termlyx" })).toBeTruthy();
    expect(screen.getByText("app layout (locked)")).toBeTruthy();
  });

  it("a_failed_check_on_return_counts_as_locked_until_the_status_answers", async () => {
    setStatus("unlocked");
    render(<Gate />);
    await screen.findByText("app layout");
    act(() => setVisibility("hidden"));
    backend.frontFails = true;
    backend.hang = true;
    await act(async () => setVisibility("visible"));
    // The panes are told "locked" before anything resumes.
    expect(screen.getByText("app layout (locked)")).toBeTruthy();
    expect(backend.release).toHaveLength(1);

    // The status says unlocked after all: the app is back as it was.
    await act(async () => backend.release.forEach((r) => r()));
    expect(screen.getByText("app layout")).toBeTruthy();
    expect(screen.queryByRole("heading", { name: "Unlock Termlyx" })).toBeNull();
  });

  it("a_failed_check_on_return_shows_a_lock_the_status_reports", async () => {
    setStatus("unlocked");
    render(<Gate />);
    await screen.findByText("app layout");
    act(() => setVisibility("hidden"));
    backend.frontFails = true;
    setStatus("locked");
    await act(async () => setVisibility("visible"));
    expect(screen.getByRole("heading", { name: "Unlock Termlyx" })).toBeTruthy();
    expect(screen.getByText("app layout (locked)")).toBeTruthy();
  });

  it("an_older_status_read_answering_last_does_not_undo_the_lock", async () => {
    setStatus("unlocked");
    render(<Gate />);
    await screen.findByText("app layout");
    backend.hang = true;
    await act(async () => backend.onLocked?.({ reason: "manual" }));
    await act(async () => backend.onLocked?.({ reason: "idle" }));
    expect(backend.release).toHaveLength(2);
    // The newer read answers "locked" first; the older one answers "unlocked" after it.
    setStatus("locked");
    await act(async () => backend.release[1]());
    setStatus("unlocked");
    await act(async () => backend.release[0]());
    expect(screen.getByText("app layout (locked)")).toBeTruthy();
    expect(screen.getByRole("heading", { name: "Unlock Termlyx" })).toBeTruthy();
  });

  it("an_idle_lock_shows_the_plain_unlock_screen", async () => {
    setStatus("unlocked");
    render(<Gate />);
    await screen.findByText("app layout");
    setStatus("locked");
    await act(async () => backend.onLocked?.({ reason: "idle" }));
    expect(await screen.findByRole("heading", { name: "Unlock Termlyx" })).toBeTruthy();
    expect(screen.getByText("app layout (locked)")).toBeTruthy();
    expect(screen.queryByText(/master password was changed/i)).toBeNull();
  });
});
