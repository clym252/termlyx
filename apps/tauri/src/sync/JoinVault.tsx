import { SyncFlow } from "./SyncFlow";

/**
 * First run, "Join a synced vault": paste an add-device code, confirm the
 * server, sign in and download the vault. Also the way back in on a device
 * without a vault after a lost master password (recovery key, then an
 * add-device code). `onJoined` runs when the user leaves the last step; the
 * vault is unlocked by then.
 */
export function JoinVault({ onJoined, onBack }: { onJoined: () => void; onBack: () => void }) {
  return (
    <main className="center-screen">
      <div className="card form">
        <h1>Join a synced vault</h1>
        <p className="muted">Download the vault of a device that already syncs, and keep it in step from here.</p>
        <SyncFlow context="firstRun" onClose={onBack} onDone={onJoined} />
      </div>
    </main>
  );
}
