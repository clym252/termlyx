import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api } from "../api";
import type { ForwardRule, ForwardStatus, Host, Id } from "../api/types";
import { useVaultChanged } from "../sync/vaultChanged";
import { ConfirmDialog } from "../ui/Dialog";
import { describeError } from "../ui/errors";
import { useRemembered } from "../ui/remembered";
import { KIND_LABELS, RuleEditor } from "./RuleEditor";
import { useForwardStatuses } from "./useForwardStatuses";

const ALL_HOSTS = "";

const byText = (a: string, b: string) => a.localeCompare(b, undefined, { sensitivity: "base" });

/** `127.0.0.1:15432 → db.internal:5432`; a dynamic rule has no destination. */
export function ruleSummary(r: ForwardRule): string {
  const bind = `${r.bindAddr}:${r.bindPort}`;
  return r.kind === "dynamic" ? `${bind} (SOCKS proxy)` : `${bind} → ${r.destHost ?? ""}:${r.destPort ?? ""}`;
}

type StatusView = { text: string; state: ForwardStatus["state"] };

function describeStatus(status: ForwardStatus | undefined): StatusView {
  switch (status?.state) {
    case "starting":
      return { text: "Starting…", state: "starting" };
    case "active":
      return { text: `Active · ${status.detail.bound}`, state: "active" };
    case "error":
      return { text: `Error: ${status.detail}`, state: "error" };
    default:
      return { text: "Stopped", state: "stopped" };
  }
}

type Editing = { mode: "new" } | { mode: "edit"; id: Id } | null;

/** Port forwarding rules per host, with Start/Stop and the live status of each. */
export function ForwardingPage() {
  const [hosts, setHosts] = useState<Host[] | null>(null);
  const [rules, setRules] = useState<ForwardRule[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [hostFilter, setHostFilter] = useState<Id>(ALL_HOSTS);
  const [editing, setEditing] = useState<Editing>(null);
  const [deleting, setDeleting] = useState<ForwardRule | null>(null);
  const [deleteError, setDeleteError] = useState<string | null>(null);
  const [working, setWorking] = useState<ReadonlySet<Id>>(new Set());
  // A start that failed before the backend reported a status (e.g. the host
  // refused the login), with the rule's status at that moment: only a newer
  // status of that rule replaces it.
  const [startFailures, setStartFailures] = useState<
    Readonly<Record<Id, { message: string; status: ForwardStatus | undefined }>>
  >({});
  const statuses = useForwardStatuses();
  const statusesRef = useRef(statuses);
  statusesRef.current = statuses;

  const reload = useCallback(async () => {
    try {
      const [h, r] = await Promise.all([api.listEntities("host"), api.listEntities("forward_rule")]);
      setHosts(h);
      setRules(r);
      setError(null);
    } catch (e) {
      setError(describeError(e));
    }
  }, []);

  useEffect(() => {
    void reload();
  }, [reload]);
  useVaultChanged(reload);

  // A new event for a rule supersedes a start failure shown for it; other rules' events do not.
  useEffect(() => {
    setStartFailures((prev) => {
      const stale = Object.keys(prev).filter((id) => statuses[id] !== prev[id].status);
      if (stale.length === 0) return prev;
      const next = { ...prev };
      for (const id of stale) delete next[id];
      return next;
    });
  }, [statuses]);

  const hostList = useMemo(() => [...(hosts ?? [])].sort((a, b) => byText(a.label, b.label)), [hosts]);
  const hostLabel = useCallback(
    (id: Id) => hostList.find((h) => h.id === id)?.label ?? "Unknown host",
    [hostList],
  );
  // A filter on a host that no longer exists falls back to all hosts.
  const filter = hostList.some((h) => h.id === hostFilter) ? hostFilter : ALL_HOSTS;
  const visible = useMemo(
    () =>
      rules
        .filter((r) => filter === ALL_HOSTS || r.hostId === filter)
        .sort((a, b) => byText(hostLabel(a.hostId), hostLabel(b.hostId)) || byText(a.label, b.label)),
    [rules, filter, hostLabel],
  );

  const isRunning = (id: Id) => statuses[id]?.state === "active" || statuses[id]?.state === "starting";

  function mark(id: Id, on: boolean) {
    setWorking((prev) => {
      const next = new Set(prev);
      if (on) next.add(id);
      else next.delete(id);
      return next;
    });
  }

  async function start(rule: ForwardRule) {
    mark(rule.id, true);
    setStartFailures((prev) => Object.fromEntries(Object.entries(prev).filter(([id]) => id !== rule.id)));
    try {
      // The status itself arrives as `forward` events.
      await api.forwardStart(rule.id);
    } catch (e) {
      const failure = { message: describeError(e), status: statusesRef.current[rule.id] };
      setStartFailures((prev) => ({ ...prev, [rule.id]: failure }));
    } finally {
      mark(rule.id, false);
    }
  }

  async function stop(rule: ForwardRule) {
    mark(rule.id, true);
    try {
      await api.forwardStop(rule.id);
    } catch (e) {
      setError(describeError(e));
    } finally {
      mark(rule.id, false);
    }
  }

  async function remove(rule: ForwardRule) {
    mark(rule.id, true);
    setDeleteError(null);
    try {
      if (isRunning(rule.id)) await api.forwardStop(rule.id);
      await api.deleteEntity("forward_rule", rule.id);
      setDeleting(null);
      await reload();
    } catch (e) {
      setDeleteError(describeError(e));
    } finally {
      mark(rule.id, false);
    }
  }

  // A rule deleted on another device keeps its open editor (and what was typed in it).
  const editRule = useRemembered(
    editing?.mode === "edit" ? editing.id : null,
    editing?.mode === "edit" ? rules.find((r) => r.id === editing.id) : undefined,
  );

  return (
    <section className="page" aria-labelledby="forwarding-title">
      <header className="page-head">
        <h1 id="forwarding-title">Port Forwarding</h1>
        <button
          type="button"
          className="primary"
          disabled={hostList.length === 0}
          onClick={() => setEditing({ mode: "new" })}
        >
          New rule
        </button>
      </header>

      {error && (
        <p role="alert" className="form-error">
          {error}
        </p>
      )}

      {!hosts ? (
        !error && (
          <p role="status" className="muted">
            Loading…
          </p>
        )
      ) : hostList.length === 0 ? (
        <p className="muted empty">No hosts yet. Add a host first; forwarding rules run over a host's connection.</p>
      ) : (
        <>
          <div className="field filter-field">
            <label htmlFor="forwarding-host-filter">Show rules of</label>
            <select
              id="forwarding-host-filter"
              value={filter}
              onChange={(e) => setHostFilter(e.target.value)}
            >
              <option value={ALL_HOSTS}>All hosts</option>
              {hostList.map((h) => (
                <option key={h.id} value={h.id}>
                  {h.label}
                </option>
              ))}
            </select>
          </div>

          {visible.length === 0 ? (
            <p className="muted empty">No forwarding rules yet. Use New rule to add one.</p>
          ) : (
            <ul className="item-list" aria-label="Forwarding rules">
              {visible.map((r) => {
                const failure = startFailures[r.id];
                const shown: StatusView = failure
                  ? { text: `Error: ${failure.message}`, state: "error" }
                  : describeStatus(statuses[r.id]);
                const running = isRunning(r.id);
                const busy = working.has(r.id);
                return (
                  <li key={r.id} className="item-row">
                    <div className="item-main">
                      <span className="host-label">{r.label}</span>
                      <span className="item-meta">
                        <span>{KIND_LABELS[r.kind].split(" ")[0]}</span>
                        <span className="mono">{ruleSummary(r)}</span>
                        <span>{hostLabel(r.hostId)}</span>
                        {r.autostart && <span className="tag-pill">Autostart</span>}
                      </span>
                    </div>
                    <span className="fwd-status" data-state={shown.state} role="status">
                      {shown.text}
                    </span>
                    <div className="item-actions">
                      {running ? (
                        <button type="button" onClick={() => void stop(r)} disabled={busy && shown.state !== "starting"}>
                          Stop
                        </button>
                      ) : (
                        <button type="button" className="primary" onClick={() => void start(r)} disabled={busy}>
                          Start
                        </button>
                      )}
                      <button
                        type="button"
                        aria-label={`Edit rule ${r.label}`}
                        onClick={() => setEditing({ mode: "edit", id: r.id })}
                      >
                        Edit
                      </button>
                      <button
                        type="button"
                        className="danger-text"
                        aria-label={`Delete rule ${r.label}`}
                        onClick={() => {
                          setDeleteError(null);
                          setDeleting(r);
                        }}
                      >
                        Delete
                      </button>
                    </div>
                  </li>
                );
              })}
            </ul>
          )}
        </>
      )}

      {hosts && (editing?.mode === "new" || editRule) && (
        <RuleEditor
          rule={editRule ?? null}
          hosts={hostList}
          defaultHostId={filter || null}
          running={editRule ? isRunning(editRule.id) : false}
          onClose={() => setEditing(null)}
          onSaved={() => {
            setEditing(null);
            void reload();
          }}
        />
      )}

      {deleting && (
        <ConfirmDialog
          title={`Delete rule ${deleting.label}`}
          confirmLabel="Delete"
          danger
          busy={working.has(deleting.id)}
          error={deleteError}
          onConfirm={() => void remove(deleting)}
          onCancel={() => setDeleting(null)}
        >
          <p>
            Delete the rule <strong>{deleting.label}</strong>
            {isRunning(deleting.id) ? " and stop it" : ""}? This cannot be undone.
          </p>
        </ConfirmDialog>
      )}
    </section>
  );
}
