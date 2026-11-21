import { useId, useState, type FormEvent } from "react";
import { api } from "../api";
import type { ForwardKind, ForwardRule, Host, Id } from "../api/types";
import { DEVICE_NOUN } from "../platform";
import { Dialog } from "../ui/Dialog";
import { describeError } from "../ui/errors";
import { APP_NAME } from "../app/brand";

export const KIND_LABELS: Record<ForwardKind, string> = {
  local: "Local",
  remote: "Remote",
  dynamic: "Dynamic (SOCKS)",
};

/** What "this computer" is called here: on iOS and Android the device itself. */
const THIS_DEVICE = `this ${DEVICE_NOUN}`;

const KIND_HELP: Record<ForwardKind, string> = {
  local: `Listens on ${THIS_DEVICE} and forwards connections through the host to the destination.`,
  remote: `Listens on the host and forwards connections back to the destination from ${THIS_DEVICE}.`,
  dynamic: `Listens on ${THIS_DEVICE} as a SOCKS proxy that connects through the host.`,
};

function isKind(v: string): v is ForwardKind {
  return v === "local" || v === "remote" || v === "dynamic";
}

function parsePort(text: string): number | null {
  const n = Number(text);
  return text.trim() !== "" && Number.isInteger(n) && n >= 0 && n <= 65535 ? n : null;
}

interface RuleEditorProps {
  /** The rule being edited, or `null` for a new one. */
  rule: ForwardRule | null;
  hosts: Host[];
  /** Host a new rule starts on. */
  defaultHostId: Id | null;
  /** The rule is running: edits apply the next time it starts. */
  running: boolean;
  onClose: () => void;
  onSaved: () => void;
}

/** Label, host, kind, bind address and port, destination, autostart. */
export function RuleEditor({ rule, hosts, defaultHostId, running, onClose, onSaved }: RuleEditorProps) {
  const hostId = useId();
  const kindId = useId();
  const [host, setHost] = useState<Id>(rule?.hostId ?? defaultHostId ?? hosts[0]?.id ?? "");
  const [label, setLabel] = useState(rule?.label ?? "");
  const [kind, setKind] = useState<ForwardKind>(rule?.kind ?? "local");
  const [bindAddr, setBindAddr] = useState(rule?.bindAddr ?? "127.0.0.1");
  const [bindPort, setBindPort] = useState(rule ? String(rule.bindPort) : "");
  const [destHost, setDestHost] = useState(rule?.destHost ?? "");
  const [destPort, setDestPort] = useState(rule?.destPort != null ? String(rule.destPort) : "");
  const [autostart, setAutostart] = useState(rule?.autostart ?? false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const needsDestination = kind !== "dynamic";

  async function submit(e: FormEvent) {
    e.preventDefault();
    const port = parsePort(bindPort);
    const dest = parsePort(destPort);
    if (!host) return setError("Choose the host the rule runs on.");
    if (!label.trim()) return setError("Enter a label for the rule.");
    if (!bindAddr.trim()) return setError("Enter the address to listen on.");
    if (port === null) return setError("The bind port must be a number from 0 to 65535.");
    if (needsDestination && (!destHost.trim() || dest === null || dest === 0)) {
      return setError("Enter the destination host and a port from 1 to 65535.");
    }
    setError(null);
    setBusy(true);
    try {
      await api.upsertEntity("forward_rule", {
        ...(rule ? { id: rule.id } : {}),
        hostId: host,
        label: label.trim(),
        kind,
        bindAddr: bindAddr.trim(),
        bindPort: port,
        destHost: needsDestination ? destHost.trim() : null,
        destPort: needsDestination ? dest : null,
        autostart,
      });
      onSaved();
    } catch (err) {
      setError(describeError(err));
      setBusy(false);
    }
  }

  return (
    <Dialog title={rule ? `Edit rule ${rule.label}` : "New rule"} onClose={busy ? undefined : onClose} wide>
      <form className="form" onSubmit={(e) => void submit(e)}>
        <div className="dialog-content">
          <div className="field">
            <label htmlFor={hostId}>Host</label>
            <select id={hostId} value={host} onChange={(e) => setHost(e.target.value)}>
              {hosts.map((h) => (
                <option key={h.id} value={h.id}>
                  {h.label}
                </option>
              ))}
            </select>
          </div>
          <label className="field">
            <span>Label</span>
            <input value={label} onChange={(e) => setLabel(e.target.value)} autoFocus />
          </label>
          <div className="field">
            <label htmlFor={kindId}>Type</label>
            <select
              id={kindId}
              value={kind}
              onChange={(e) => {
                if (isKind(e.target.value)) setKind(e.target.value);
              }}
            >
              {(Object.keys(KIND_LABELS) as ForwardKind[]).map((k) => (
                <option key={k} value={k}>
                  {KIND_LABELS[k]}
                </option>
              ))}
            </select>
            <p className="muted small">{KIND_HELP[kind]}</p>
          </div>
          <div className="row-fields">
            <label className="field grow">
              <span>Bind address</span>
              <input
                value={bindAddr}
                onChange={(e) => setBindAddr(e.target.value)}
                spellCheck={false}
                autoCapitalize="off"
              />
            </label>
            <label className="field port-field">
              <span>Bind port</span>
              <input
                inputMode="numeric"
                value={bindPort}
                onChange={(e) => setBindPort(e.target.value)}
              />
            </label>
          </div>
          {needsDestination && (
            <div className="row-fields">
              <label className="field grow">
                <span>Destination host</span>
                <input
                  value={destHost}
                  onChange={(e) => setDestHost(e.target.value)}
                  spellCheck={false}
                  autoCapitalize="off"
                />
              </label>
              <label className="field port-field">
                <span>Destination port</span>
                <input
                  inputMode="numeric"
                  value={destPort}
                  onChange={(e) => setDestPort(e.target.value)}
                />
              </label>
            </div>
          )}
          <label className="choice">
            <input type="checkbox" checked={autostart} onChange={(e) => setAutostart(e.target.checked)} />
            <span>Start automatically</span>
          </label>
          <p className="muted small">Saved with the rule; starting rules when {APP_NAME} opens is not available yet.</p>
          {running && <p className="muted small">This rule is running. Changes apply the next time it starts.</p>}
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
          <button type="submit" className="primary" disabled={busy}>
            Save rule
          </button>
        </div>
      </form>
    </Dialog>
  );
}
