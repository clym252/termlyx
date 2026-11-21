import { useState } from "react";
import { api } from "../api";
import type { ImportReport } from "../api/types";
import { Dialog } from "../ui/Dialog";
import { describeError } from "../ui/errors";

/** The default OpenSSH config; the backend expands `~`. */
const DEFAULT_CONFIG = "~/.ssh/config";

interface ImportSshConfigDialogProps {
  onClose: () => void;
  /** An import finished (its report is shown); reload what it may have added. */
  onImported: () => void;
}

/**
 * Imports hosts from an OpenSSH config file: the default `~/.ssh/config` or
 * a file chosen with the native picker. Shows what was added, what was
 * skipped and why, and key files that could not be imported.
 */
export function ImportSshConfigDialog({ onClose, onImported }: ImportSshConfigDialogProps) {
  const [report, setReport] = useState<ImportReport | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  async function run(path: string | null) {
    if (path === null) return;
    setBusy(true);
    setError(null);
    try {
      setReport(await api.importSshConfig(path));
      onImported();
    } catch (e) {
      setError(describeError(e));
    } finally {
      setBusy(false);
    }
  }

  async function choose() {
    try {
      await run(await api.pickFile({ title: "Import SSH config" }));
    } catch (e) {
      setError(describeError(e));
    }
  }

  return (
    <Dialog title="Import SSH config" onClose={busy ? undefined : onClose} wide>
      {report ? (
        <>
          <section className="dialog-content" aria-label="Import result">
            <ul className="plain-list">
              <li>Hosts added: {report.hostsAdded}</li>
              <li>Identities added: {report.identitiesAdded}</li>
              <li>Keys added: {report.keysAdded}</li>
            </ul>
            {report.skipped.length > 0 && (
              <>
                <h3>Skipped</h3>
                <ul aria-label="Skipped" className="plain-list">
                  {report.skipped.map((s, i) => (
                    <li key={`${i}-${s.name}`}>
                      <strong>{s.name}</strong>: {s.reason}
                    </li>
                  ))}
                </ul>
              </>
            )}
            {report.keyFailures.length > 0 && (
              <>
                <h3>Keys that could not be imported</h3>
                <ul aria-label="Keys that could not be imported" className="plain-list">
                  {report.keyFailures.map((f, i) => (
                    <li key={`${i}-${f.path}`}>
                      <span className="mono">{f.path}</span>:{" "}
                      {f.needsPassphrase
                        ? "this key needs a passphrase. Import it from Keychain with Import key and enter the passphrase there."
                        : f.message}
                    </li>
                  ))}
                </ul>
              </>
            )}
          </section>
          <div className="dialog-actions">
            <button type="button" className="primary" onClick={onClose}>
              Done
            </button>
          </div>
        </>
      ) : (
        <>
          <div className="dialog-content">
            <p>
              Adds the hosts, users and key files named in an OpenSSH config. The file itself is never
              changed, and hosts that already exist are skipped.
            </p>
          </div>
          {error && (
            <p role="alert" className="form-error">
              {error}
            </p>
          )}
          <div className="dialog-actions">
            <button type="button" onClick={onClose} disabled={busy}>
              Cancel
            </button>
            <button type="button" onClick={() => void choose()} disabled={busy}>
              Choose file…
            </button>
            <button type="button" className="primary" onClick={() => void run(DEFAULT_CONFIG)} disabled={busy}>
              Use ~/.ssh/config
            </button>
          </div>
        </>
      )}
    </Dialog>
  );
}
