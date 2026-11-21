import { useState } from "react";
import type { Id } from "../api/types";
import { useRemembered } from "../ui/remembered";
import { DeleteEntityDialog } from "./DeleteEntityDialog";
import { GenerateKeyDialog } from "./GenerateKeyDialog";
import { IdentityEditor } from "./IdentityEditor";
import { ImportKeyDialog } from "./ImportKeyDialog";
import { KeyEditor } from "./KeyEditor";
import { useKeychainData } from "./useKeychainData";

type Dialog =
  | { kind: "generate" }
  | { kind: "import" }
  | { kind: "editKey"; id: Id }
  | { kind: "deleteKey"; id: Id }
  | { kind: "identity"; id: Id | null }
  | { kind: "deleteIdentity"; id: Id }
  | null;

const byLabel = (a: { label: string }, b: { label: string }) =>
  a.label.localeCompare(b.label, undefined, { sensitivity: "base" });

/** Keys (generate, import, rename, export, delete) and the identities that use them. */
export function KeychainPage() {
  const { data, error, reload } = useKeychainData();
  const [dialog, setDialog] = useState<Dialog>(null);

  const keys = [...(data?.keys ?? [])].sort(byLabel);
  const identities = [...(data?.identities ?? [])].sort(byLabel);
  const keyLabel = (id: Id | null) => (id === null ? null : (keys.find((k) => k.id === id)?.label ?? null));

  // An editor survives its record being deleted on another device (it keeps what was typed).
  const editKey = useRemembered(
    dialog?.kind === "editKey" ? dialog.id : null,
    dialog?.kind === "editKey" ? keys.find((k) => k.id === dialog.id) : undefined,
  );
  const deleteKey = dialog?.kind === "deleteKey" ? keys.find((k) => k.id === dialog.id) : undefined;
  const identity = useRemembered(
    dialog?.kind === "identity" ? dialog.id : null,
    dialog?.kind === "identity" && dialog.id !== null ? identities.find((i) => i.id === dialog.id) : undefined,
  );
  const deleteIdentity =
    dialog?.kind === "deleteIdentity" ? identities.find((i) => i.id === dialog.id) : undefined;

  const done = () => {
    setDialog(null);
    void reload();
  };

  return (
    <section className="page keychain-page" aria-labelledby="keychain-title">
      <header className="page-head">
        <h1 id="keychain-title">Keychain</h1>
        <div className="row">
          <button type="button" onClick={() => setDialog({ kind: "import" })}>
            Import key
          </button>
          <button type="button" className="primary" onClick={() => setDialog({ kind: "generate" })}>
            Generate key
          </button>
        </div>
      </header>

      {error && (
        <p role="alert" className="form-error">
          {error}
        </p>
      )}

      {!data ? (
        !error && (
          <p role="status" className="muted">
            Loading…
          </p>
        )
      ) : (
        <>
          <section className="list-section" aria-labelledby="keys-title">
            <h2 id="keys-title">Keys</h2>
            {keys.length === 0 ? (
              <p className="muted empty">No keys yet. Generate one or import an existing private key.</p>
            ) : (
              <ul className="item-list" aria-label="Keys">
                {keys.map((k) => (
                  <li key={k.id} className="item-row">
                    <div className="item-main">
                      <span className="host-label">{k.label}</span>
                      <span className="item-meta mono">
                        <span>{k.algorithm}</span>
                        <span className="fingerprint">{k.fingerprint}</span>
                        {k.hasPassphrase && <span className="tag-pill">Passphrase</span>}
                      </span>
                    </div>
                    <div className="item-actions">
                      <button
                        type="button"
                        aria-label={`Edit key ${k.label}`}
                        onClick={() => setDialog({ kind: "editKey", id: k.id })}
                      >
                        Edit
                      </button>
                      <button
                        type="button"
                        className="danger-text"
                        aria-label={`Delete key ${k.label}`}
                        onClick={() => setDialog({ kind: "deleteKey", id: k.id })}
                      >
                        Delete
                      </button>
                    </div>
                  </li>
                ))}
              </ul>
            )}
          </section>

          <section className="list-section" aria-labelledby="identities-title">
            <div className="page-head">
              <h2 id="identities-title">Identities</h2>
              <button type="button" onClick={() => setDialog({ kind: "identity", id: null })}>
                New identity
              </button>
            </div>
            {identities.length === 0 ? (
              <p className="muted empty">No identities yet. An identity pairs a username with a password or key.</p>
            ) : (
              <ul className="item-list" aria-label="Identities">
                {identities.map((i) => {
                  const key = keyLabel(i.keyId);
                  return (
                    <li key={i.id} className="item-row">
                      <div className="item-main">
                        <span className="host-label">{i.label}</span>
                        <span className="item-meta">
                          <span className="mono">{i.username}</span>
                          {key && <span>{key}</span>}
                          {i.hasPassword && <span className="tag-pill">Password</span>}
                        </span>
                      </div>
                      <div className="item-actions">
                        <button
                          type="button"
                          aria-label={`Edit identity ${i.label}`}
                          onClick={() => setDialog({ kind: "identity", id: i.id })}
                        >
                          Edit
                        </button>
                        <button
                          type="button"
                          className="danger-text"
                          aria-label={`Delete identity ${i.label}`}
                          onClick={() => setDialog({ kind: "deleteIdentity", id: i.id })}
                        >
                          Delete
                        </button>
                      </div>
                    </li>
                  );
                })}
              </ul>
            )}
          </section>
        </>
      )}

      {dialog?.kind === "generate" && (
        <GenerateKeyDialog onClose={() => setDialog(null)} onGenerated={() => void reload()} />
      )}
      {dialog?.kind === "import" && <ImportKeyDialog onClose={() => setDialog(null)} onImported={done} />}
      {editKey && <KeyEditor sshKey={editKey} onClose={() => setDialog(null)} onSaved={done} />}
      {data && deleteKey && (
        <DeleteEntityDialog
          kind="ssh_key"
          id={deleteKey.id}
          label={deleteKey.label}
          fallback={data}
          onClose={() => setDialog(null)}
          onDeleted={done}
        />
      )}
      {data && dialog?.kind === "identity" && (dialog.id === null || identity) && (
        <IdentityEditor
          identity={identity ?? null}
          keys={keys}
          onClose={() => setDialog(null)}
          onSaved={done}
        />
      )}
      {data && deleteIdentity && (
        <DeleteEntityDialog
          kind="identity"
          id={deleteIdentity.id}
          label={deleteIdentity.label}
          fallback={data}
          onClose={() => setDialog(null)}
          onDeleted={done}
        />
      )}
    </section>
  );
}
