import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api } from "../api";
import type { Conflict, EntityKind, Identity, Resolution } from "../api/types";
import { ConfirmDialog } from "../ui/Dialog";
import { useVaultChanged } from "../sync/vaultChanged";
import { describeError } from "../ui/errors";
import { ConflictDiff, foundAt } from "./ConflictDiff";
import { conflictTitle, discardedSecrets, joinLabels, replacedSecrets, type NameOf } from "./model";
import { refreshConflicts, useConflicts } from "./store";

type Names = Map<string, string>;

/** How an identity reads where a setting points at it. */
const describeIdentity = (i: Identity) => (i.owner ? `Own login (${i.username})` : i.label);

/** Every record's display name, keyed by kind and id, so references can be shown by name. */
async function loadNames(): Promise<Names> {
  const [groups, hosts, tags, identities, keys, rules] = await Promise.all([
    api.listEntities("group"),
    api.listEntities("host"),
    api.listEntities("tag"),
    api.listEntities("identity"),
    api.listEntities("ssh_key"),
    api.listEntities("forward_rule"),
  ]);
  const names: Names = new Map();
  const add = (kind: EntityKind, id: string, name: string) => names.set(`${kind}:${id}`, name);
  groups.forEach((g) => add("group", g.id, g.name));
  hosts.forEach((h) => add("host", h.id, h.label));
  tags.forEach((t) => add("tag", t.id, t.name));
  identities.forEach((i) => add("identity", i.id, describeIdentity(i)));
  keys.forEach((k) => add("ssh_key", k.id, k.label));
  rules.forEach((r) => add("forward_rule", r.id, r.label));
  return names;
}

/** What "Use other version" does, when it is more than a swap. */
function otherHint(c: Conflict): string | null {
  if (c.other === null) return "Deletes the record.";
  if (c.current === null) return "Restores the record.";
  return null;
}

interface CardProps {
  conflict: Conflict;
  name: NameOf;
  onResolved(): void;
}

/** A resolution waiting for the user to confirm that it loses secrets. */
interface Pending {
  resolution: Resolution;
  /** The version that is discarded, and its secrets ("password", "private key and passphrase"). */
  discarded: { side: "current" | "other"; secrets: string } | null;
  /** The current version's secrets a pick overwrites with the other version's. */
  replaced: string | null;
}

/** The confirm's title and button for what `p` loses. */
function confirmText(p: Pending): { title: string; confirm: string } {
  if (p.discarded && p.replaced) {
    return {
      title: `Lose the current version's ${p.replaced} and the other version's ${p.discarded.secrets}?`,
      confirm: "Discard",
    };
  }
  if (p.replaced) return { title: `Replace the current version's ${p.replaced}?`, confirm: "Replace" };
  return { title: `Discard the ${p.discarded?.side} version's ${p.discarded?.secrets}?`, confirm: "Discard" };
}

function ConflictCard({ conflict, name, onResolved }: CardProps) {
  const [picking, setPicking] = useState<Set<string> | null>(null);
  const [pending, setPending] = useState<Pending | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const title = conflictTitle(conflict, name);
  const hint = otherHint(conflict);
  // A field can only be taken from a version that exists, into one that exists.
  const canPick = conflict.current !== null && conflict.other !== null && conflict.changedFields.length > 0;
  // Either version holds a secret that differs: say so beside the buttons.
  const hasSecret =
    discardedSecrets(conflict, "current").length > 0 || discardedSecrets(conflict, "other").length > 0;

  async function resolve(resolution: Resolution) {
    setBusy(true);
    setError(null);
    try {
      await api.conflicts.resolve(conflict.id, resolution);
      setBusy(false);
      setPending(null);
      onResolved();
    } catch (e) {
      setError(describeError(e));
      setBusy(false);
    }
  }

  /**
   * Settles it, after a confirm when a secret is lost: one the discarded version holds, or (for a
   * pick) one of the current version's that a taken field overwrites.
   */
  function settle(resolution: Resolution, side: "current" | "other", taken?: ReadonlySet<string>) {
    const lost = discardedSecrets(conflict, side, taken);
    const replaced = taken ? replacedSecrets(conflict, taken) : [];
    if (lost.length === 0 && replaced.length === 0) {
      void resolve(resolution);
      return;
    }
    setError(null);
    setPending({
      resolution,
      discarded: lost.length > 0 ? { side, secrets: joinLabels(lost) } : null,
      replaced: replaced.length > 0 ? joinLabels(replaced) : null,
    });
  }

  function toggle(field: string) {
    setPicking((prev) => {
      const next = new Set(prev ?? []);
      if (!next.delete(field)) next.add(field);
      return next;
    });
  }

  const discard = (
    <>
      The version you don't choose is discarded.
      {hasSecret && " A secret (password or key) in it is lost with it."}
    </>
  );

  return (
    <article className="conflict-card" aria-label={title}>
      <header className="conflict-head">
        <h2>{title}</h2>
        <span className="muted small">Found {foundAt(conflict)}</span>
      </header>
      <ConflictDiff conflict={conflict} name={name} picking={picking} onToggle={toggle} />
      {error && !pending && (
        <p role="alert" className="form-error">
          {error}
        </p>
      )}
      {picking ? (
        <div className="row conflict-actions">
          <button
            type="button"
            className="primary"
            disabled={busy || picking.size === 0}
            onClick={() => settle({ fields: [...picking].sort() }, "other", picking)}
          >
            Apply
          </button>
          <button type="button" disabled={busy} onClick={() => setPicking(null)}>
            Cancel
          </button>
          <span className="muted small">Tick the fields to take from the other version. The rest of it is discarded.</span>
        </div>
      ) : (
        <div className="row conflict-actions">
          <button type="button" className="primary" disabled={busy} onClick={() => settle({ keepCurrent: true }, "other")}>
            Keep current
          </button>
          <button type="button" disabled={busy} onClick={() => settle({ useOther: true }, "current")}>
            Use other version
          </button>
          {canPick && (
            <button type="button" disabled={busy} onClick={() => setPicking(new Set())}>
              Pick fields
            </button>
          )}
          <span className="muted small">
            {discard}
            {hint && ` ${hint}`}
          </span>
        </div>
      )}
      {pending && (
        <ConfirmDialog
          title={confirmText(pending).title}
          confirmLabel={confirmText(pending).confirm}
          danger
          busy={busy}
          error={error}
          onConfirm={() => void resolve(pending.resolution)}
          onCancel={() => {
            setError(null);
            setPending(null);
          }}
        >
          {pending.replaced && (
            <p>
              Taking the {pending.replaced} from the other version of <strong>{title}</strong> replaces the current
              version's {pending.replaced}. Once this conflict is settled the current one is gone for good.
            </p>
          )}
          {pending.discarded && (
            <p>
              The {pending.discarded.side} version of <strong>{title}</strong> holds a {pending.discarded.secrets}. Once
              this conflict is settled that version is gone for good.
            </p>
          )}
        </ConfirmDialog>
      )}
    </article>
  );
}

/**
 * Records two devices changed at once (or one edited while the other
 * deleted). The version that lost is kept here until you settle it: keep the
 * current one, take the other, or pick fields from it.
 */
export function ConflictsPage() {
  const { conflicts, error } = useConflicts();
  const [names, setNames] = useState<Names | null>(null);
  const [namesError, setNamesError] = useState<string | null>(null);
  const heading = useRef<HTMLHeadingElement>(null);
  // Bumped when sync changed records: a name may have changed.
  const [pulled, setPulled] = useState(0);
  useVaultChanged(() => setPulled((n) => n + 1));

  useEffect(() => {
    void refreshConflicts();
  }, []);

  // Names are read again when the set of conflicts changes (not on every identical refresh) and
  // when sync changed records.
  const key = conflicts === null ? null : `${pulled}:${conflicts.map((c) => c.id).join(",")}`;
  useEffect(() => {
    if (key === null) return;
    let cancelled = false;
    loadNames().then(
      (n) => {
        if (cancelled) return;
        setNames(n);
        setNamesError(null);
      },
      (e) => {
        if (!cancelled) setNamesError(describeError(e));
      },
    );
    return () => {
      cancelled = true;
    };
  }, [key]);

  const name = useCallback<NameOf>((kind, id) => names?.get(`${kind}:${id}`), [names]);
  const sorted = useMemo(() => [...(conflicts ?? [])].sort((a, b) => a.detectedAt - b.detectedAt), [conflicts]);

  const resolved = useCallback(() => {
    // The card goes away with its buttons: keep the keyboard on the page.
    heading.current?.focus();
    void refreshConflicts();
  }, []);

  const shownError = error ?? namesError;
  return (
    <section className="page conflicts-page" aria-labelledby="conflicts-title">
      <header className="page-head">
        <h1 id="conflicts-title" ref={heading} tabIndex={-1}>
          Conflicts
        </h1>
      </header>
      <p className="muted">
        Two devices changed the same record. The newer change is the one in use; the other version is kept here until you
        settle it.
      </p>
      {shownError && (
        <p role="alert" className="form-error">
          {shownError}
        </p>
      )}
      {conflicts !== null && sorted.length === 0 ? (
        <p className="muted empty">No conflicts.</p>
      ) : conflicts === null || names === null ? (
        !shownError && (
          <p role="status" className="muted">
            Loading…
          </p>
        )
      ) : (
        <ul className="conflict-list" aria-label="Conflicts">
          {sorted.map((c) => (
            <li key={c.id}>
              <ConflictCard conflict={c} name={name} onResolved={resolved} />
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
