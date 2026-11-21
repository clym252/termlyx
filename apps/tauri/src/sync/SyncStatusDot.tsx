import { useState } from "react";
import { SignInDialog } from "./SignInDialog";
import { describeSync, needsSignIn, type SyncTone } from "./status";
import { useSyncStatus } from "./store";

/** What the sidebar says next to the dot (the full text is the tooltip). */
const SHORT: Record<SyncTone, string> = {
  off: "",
  idle: "Synced",
  syncing: "Syncing…",
  offline: "Offline",
  error: "Sync problem",
};

/**
 * The sync status in the sidebar footer: a dot (green synced, blue syncing,
 * amber offline, red problem) and a few words; the tooltip has the last sync
 * time or the reason. Hidden while sync is off. A click opens the Sync
 * settings, or the password prompt when sync waits to sign in again.
 * `compact` (the sidebar's icon rail) leaves the dot alone; the tooltip and
 * accessible name still say it all.
 */
export function SyncStatusDot({ onOpen, compact = false }: { onOpen: () => void; compact?: boolean }) {
  const { sync } = useSyncStatus();
  const [signIn, setSignIn] = useState(false);
  if (!sync || sync.status.state === "off") return null;
  const view = describeSync(sync.status);
  const waiting = needsSignIn(sync.status);
  return (
    <>
      <button
        type="button"
        className={compact ? "nav-item sync-status sync-status-compact" : "nav-item sync-status"}
        data-testid="sync-status"
        data-state={view.tone}
        title={view.title}
        aria-label={`Sync status: ${view.title}`}
        onClick={waiting ? () => setSignIn(true) : onOpen}
      >
        <span className={`sync-dot sync-dot-${view.tone}`} aria-hidden="true" />
        {!compact && <span>{waiting ? "Sign in again" : SHORT[view.tone]}</span>}
      </button>
      {signIn && <SignInDialog onClose={() => setSignIn(false)} />}
    </>
  );
}
