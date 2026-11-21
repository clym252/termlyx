import { type ReactNode, useCallback, useEffect, useRef, useState } from "react";
import { flushSync } from "react-dom";
import { api } from "../api";
import type { LockReason, VaultStatus } from "../api/types";
import { FORM_FACTOR, IS_IOS, IS_MOBILE } from "../platform";
import { applyAutoLock } from "../settings/autoLockPref";
import { applyKeepAlive, followKeepAlive } from "../settings/KeepAliveSetting";
import { describeError } from "../ui/errors";
import { setFrontCheck } from "../workspace/useAppActive";
import { CreateVault } from "../vault/CreateVault";
import { UnlockScreen } from "../vault/UnlockScreen";
import { VaultUnavailable } from "../vault/VaultUnavailable";
import { Layout } from "./Layout";

type Phase = { kind: "loading" } | { kind: "ready"; status: VaultStatus } | { kind: "failed"; message: string };

/**
 * Decides what the window shows from the vault's state: create, unlock, an
 * error, or the app. `unlockedView` replaces the app once unlocked (DEV: the
 * throughput bench, whose connection checks host keys against the vault and
 * so needs it unlocked).
 */
export function Gate({ unlockedView }: { unlockedView?: ReactNode } = {}) {
  const [phase, setPhase] = useState<Phase>({ kind: "loading" });
  // Why the vault last locked: another device changing the password gets its own message.
  const [lockReason, setLockReason] = useState<LockReason>("manual");

  // The backend said the vault is locked and its status is being read: the app counts as
  // locked meanwhile (hidden, panes told), behind a loading screen.
  const [locking, setLocking] = useState(false);
  // Only the latest status read applies: one started before a lock must not undo it.
  const reads = useRef(0);

  const refresh = useCallback(async () => {
    const read = ++reads.current;
    try {
      const status = await api.vaultStatus();
      if (read !== reads.current) return;
      // Unlocked: whatever locked it last is history.
      if (status.state === "unlocked") setLockReason("manual");
      setPhase({ kind: "ready", status });
    } catch (e) {
      if (read !== reads.current) return;
      setPhase({ kind: "failed", message: describeError(e) });
    }
    setLocking(false);
  }, []);

  /** The backend says the vault is locked: locked from this render on, then the status is read. */
  const lockedNow = useCallback(() => {
    setLocking(true);
    void refresh();
  }, [refresh]);

  useEffect(() => {
    // Auto-lock: the backend keeps the clock; it gets this device's limit, and every move
    // to and from the front. On a return the panes stay inactive until the answer is in,
    // and a "locked" answer is rendered before they resume, so no reconnect attempt can
    // start ahead of the lock (the `vault-locked` event may arrive later).
    // A limit the backend refuses twice is shown in Settings → Security (it runs with Never).
    void applyAutoLock((m) => api.setAutoLock(m));
    // Every mount says where the page is: a reload while in front fires no visibility
    // change, and the backend would otherwise read the app as away and lock while in use.
    const shown = document.visibilityState === "visible";
    api
      .appFront(shown)
      .then((locked) => {
        if (shown && locked) lockedNow();
      })
      .catch(() => {});
    return setFrontCheck(async (front) => {
      let locked: boolean;
      try {
        locked = await api.appFront(front);
      } catch {
        // Unknown on a return: locked until the vault's status answers, so no pane
        // reconnects ahead of a lock the check could not report.
        locked = true;
      }
      if (front && locked) flushSync(lockedNow);
    });
  }, [lockedNow]);

  useEffect(() => {
    // The sync device list names this device by its host name, which iOS keeps generic.
    if (IS_IOS) void api.setDeviceFallback(FORM_FACTOR === "tablet" ? "iPad" : "iPhone").catch(() => {});
  }, []);

  useEffect(() => {
    // Background keep-alive (iOS, Android): back on if this device chose it and permission is still
    // given (never asks at start-up); the stored choice then follows the backend.
    if (!IS_MOBILE) return;
    void applyKeepAlive(api);
    const unlisten = followKeepAlive(api);
    return () => void unlisten.then((u) => u());
  }, []);

  useEffect(() => {
    void refresh();
    // The vault can lock under us (auto-lock, another window): show the unlock screen.
    let unlisten: (() => void) | undefined;
    let live = true;
    void api
      .onVaultLocked((e) => {
        setLockReason(e?.reason ?? "manual");
        lockedNow();
      })
      .then((off) => {
        if (live) unlisten = off;
        else off();
      });
    return () => {
      live = false;
      unlisten?.();
    };
  }, [refresh, lockedNow]);

  const unlocked = phase.kind === "ready" && phase.status.state === "unlocked" && !locking;
  // Locked, or its state could not be read: keep what is open.
  const locked = (phase.kind === "ready" && phase.status.state === "locked") || phase.kind === "failed" || locking;
  // Once the app has been unlocked, locking keeps it mounted (hidden) under
  // the unlock screen, so open terminals are not silently ended.
  const [kept, setKept] = useState(false);
  if (unlocked && !kept) setKept(true);
  if (kept && !unlocked && !locked && phase.kind === "ready") setKept(false); // vault gone
  const layout =
    (unlocked || (kept && locked)) &&
    (unlockedView ?? <Layout locked={!unlocked} onLocked={() => void refresh()} />);

  return (
    <>
      {layout}
      {!unlocked && (
        <GateScreen
          // Covered while locking, so the unlock screen starts from the fresh status (quick unlock on or off).
          phase={locking && phase.kind === "ready" && phase.status.state === "unlocked" ? { kind: "loading" } : phase}
          refresh={refresh}
          lockReason={lockReason}
        />
      )}
    </>
  );
}

function GateScreen({
  phase,
  refresh,
  lockReason,
}: {
  phase: Phase;
  refresh: () => Promise<void>;
  lockReason: LockReason;
}) {
  switch (phase.kind) {
    case "loading":
      return (
        <main className="center-screen">
          <p role="status" className="muted">
            Loading…
          </p>
        </main>
      );
    case "failed":
      return <VaultUnavailable error={phase.message} onRetry={() => void refresh()} />;
    case "ready":
      switch (phase.status.state) {
        case "uninitialized":
          return <CreateVault onCreated={() => void refresh()} />;
        case "locked":
          return (
            <UnlockScreen
              onUnlocked={() => void refresh()}
              quickUnlock={phase.status.quickUnlockEnabled}
              quickUnlockKind={phase.status.quickUnlockKind}
              passwordChanged={lockReason === "passwordChanged"}
            />
          );
        case "unavailable":
          return <VaultUnavailable error={phase.status.error} onRetry={() => void refresh()} />;
        case "unlocked":
          return null;
      }
  }
}
