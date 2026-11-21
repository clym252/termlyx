import { useCallback, useEffect, useMemo, useState } from "react";
import { api } from "../api";
import type { KnownHost } from "../api/types";
import { ConfirmDialog } from "../ui/Dialog";
import { useVaultChanged } from "../sync/vaultChanged";
import { describeError } from "../ui/errors";
import { IS_MOBILE } from "../platform";
import { APP_NAME } from "../app/brand";

const DEFAULT_KNOWN_HOSTS = "~/.ssh/known_hosts";

const hostName = (k: KnownHost) => (k.port === 22 ? k.hostPattern : `${k.hostPattern}:${k.port}`);

function addedOn(k: KnownHost): string {
  return k.addedAt > 0 ? new Date(k.addedAt * 1000).toLocaleDateString() : "";
}

/** The host keys Tether trusts: where they came from, and a way to import or forget them. */
export function KnownHostsPage() {
  const [entries, setEntries] = useState<KnownHost[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [result, setResult] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [removing, setRemoving] = useState<KnownHost | null>(null);
  const [removeError, setRemoveError] = useState<string | null>(null);

  const reload = useCallback(async () => {
    try {
      setEntries(await api.listEntities("known_host"));
    } catch (e) {
      setError(describeError(e));
    }
  }, []);

  useEffect(() => {
    void reload();
  }, [reload]);
  useVaultChanged(reload);

  const sorted = useMemo(
    () =>
      [...(entries ?? [])].sort(
        (a, b) =>
          a.hostPattern.localeCompare(b.hostPattern, undefined, { sensitivity: "base" }) ||
          a.port - b.port ||
          a.algorithm.localeCompare(b.algorithm),
      ),
    [entries],
  );

  async function importFrom(path: string | null) {
    if (path === null) return;
    setBusy(true);
    setError(null);
    setResult(null);
    try {
      const report = await api.knownHostsImport(path);
      setResult(`${report.added} added, ${report.alreadyKnown} already known`);
      await reload();
    } catch (e) {
      setError(describeError(e));
    } finally {
      setBusy(false);
    }
  }

  async function pickAndImport() {
    try {
      await importFrom(await api.pickFile({ title: "Import known_hosts" }));
    } catch (e) {
      setError(describeError(e));
    }
  }

  async function remove(entry: KnownHost) {
    setBusy(true);
    setRemoveError(null);
    try {
      await api.deleteEntity("known_host", entry.id);
      setRemoving(null);
      setResult(null);
      await reload();
    } catch (e) {
      setRemoveError(describeError(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <section className="page" aria-labelledby="known-hosts-title">
      <header className="page-head">
        <h1 id="known-hosts-title">Known Hosts</h1>
        {/* iOS has no ~/.ssh to import from, so the import is desktop-only. */}
        {!IS_MOBILE && (
          <div className="row">
            <button type="button" onClick={() => void importFrom(DEFAULT_KNOWN_HOSTS)} disabled={busy}>
              Import ~/.ssh/known_hosts
            </button>
            <button type="button" className="primary" onClick={() => void pickAndImport()} disabled={busy}>
              Import known_hosts…
            </button>
          </div>
        )}
      </header>

      {error && (
        <p role="alert" className="form-error">
          {error}
        </p>
      )}
      {result && <p role="status">{result}</p>}

      {!entries ? (
        !error && (
          <p role="status" className="muted">
            Loading…
          </p>
        )
      ) : sorted.length === 0 ? (
        <p className="muted empty">
          {IS_MOBILE
            ? "No known hosts yet. Host keys you accept when connecting are listed here."
            : "No known hosts yet. Host keys you accept when connecting, and keys you import, are listed here."}
        </p>
      ) : (
        <div className="table-wrap">
          <table className="data-table" aria-label="Known hosts">
            <thead>
              <tr>
                <th scope="col">Host</th>
                <th scope="col">Port</th>
                <th scope="col">Algorithm</th>
                <th scope="col">Fingerprint</th>
                <th scope="col">Source</th>
                <th scope="col">Added</th>
                <th scope="col">
                  <span className="visually-hidden">Actions</span>
                </th>
              </tr>
            </thead>
            <tbody>
              {sorted.map((k) => (
                <tr key={k.id}>
                  <td className="mono">{k.hostPattern}</td>
                  <td>{k.port}</td>
                  <td className="mono">{k.algorithm}</td>
                  <td className="mono fingerprint">{k.fingerprint}</td>
                  <td>{k.source}</td>
                  <td>{addedOn(k)}</td>
                  <td>
                    <button
                      type="button"
                      className="danger-text"
                      aria-label={`Delete known host ${hostName(k)}`}
                      onClick={() => {
                        setRemoveError(null);
                        setRemoving(k);
                      }}
                    >
                      Delete
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}

      {removing && (
        <ConfirmDialog
          title={`Remove known host ${hostName(removing)}`}
          confirmLabel="Remove"
          danger
          busy={busy}
          error={removeError}
          onConfirm={() => void remove(removing)}
          onCancel={() => setRemoving(null)}
        >
          <p>
            Forget the {removing.algorithm} key of <strong>{hostName(removing)}</strong>? {APP_NAME} will ask
            you to verify the host again the next time you connect.
          </p>
        </ConfirmDialog>
      )}
    </section>
  );
}
