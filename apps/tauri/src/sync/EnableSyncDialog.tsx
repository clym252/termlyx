import { useState } from "react";
import { Dialog } from "../ui/Dialog";
import { SyncFlow } from "./SyncFlow";

/**
 * Settings → Sync → "Connect to server". Takes an invite code (creates the
 * account and uploads this vault) or an add-device code (replaces this
 * device's vault with the synced one, after a warning). Escape and the
 * backdrop close it, except while a step is running.
 */
export function EnableSyncDialog({ onClose }: { onClose: () => void }) {
  const [busy, setBusy] = useState(false);
  return (
    <Dialog title="Connect to server" onClose={busy ? undefined : onClose}>
      <SyncFlow context="settings" onClose={onClose} onDone={onClose} onBusy={setBusy} />
    </Dialog>
  );
}
