import { useEffect, useRef, useState } from "react";
import { api } from "../api";
import type { Conflict, ConflictSide } from "../api/types";
import { describeError } from "../ui/errors";
import { fieldRows, type Cell, type NameOf, type Row } from "./model";

/** How long a revealed secret stays on screen. */
export const REVEAL_MS = 30_000;

const SIDE_NAME: Record<ConflictSide, string> = { current: "current", other: "other" };

/** When the conflict was found, for people. */
export const foundAt = (c: Conflict) => new Date(c.detectedAt * 1000).toLocaleString();

/**
 * One secret of one version: "••••" and a Reveal button. The value is fetched
 * on click, lives only in this component's state, and is dropped again on
 * Hide, after `REVEAL_MS`, and when the component goes away (the conflict was
 * resolved, the page left, the vault locked). It is never logged.
 */
function SecretValue({ conflict, side, row }: { conflict: Conflict; side: ConflictSide; row: Row }) {
  const [value, setValue] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  const alive = useRef(true);

  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
      clearTimeout(timer.current);
    };
  }, []);

  function hide() {
    clearTimeout(timer.current);
    setValue(null);
  }

  async function reveal() {
    setBusy(true);
    setError(null);
    try {
      const secret = await api.conflicts.reveal(conflict.id, side, row.field);
      if (!alive.current) return;
      setValue(secret);
      clearTimeout(timer.current);
      timer.current = setTimeout(hide, REVEAL_MS);
    } catch (e) {
      if (alive.current) setError(describeError(e));
    } finally {
      if (alive.current) setBusy(false);
    }
  }

  const what = `${row.label} of the ${SIDE_NAME[side]} version`;
  return (
    <div className="secret-cell">
      {value === null ? (
        <span className="mono" aria-label="hidden">
          ••••
        </span>
      ) : (
        <code className="secret-value">{value}</code>
      )}
      <button
        type="button"
        className="link"
        aria-label={`${value === null ? "Reveal" : "Hide"} ${what}`}
        disabled={busy}
        onClick={() => (value === null ? void reveal() : hide())}
      >
        {value === null ? "Reveal" : "Hide"}
      </button>
      {error && (
        <span role="alert" className="form-error small">
          {error}
        </span>
      )}
    </div>
  );
}

function CellView({ cell, conflict, side, row }: { cell: Cell; conflict: Conflict; side: ConflictSide; row: Row }) {
  switch (cell.kind) {
    case "absent":
      return <span className="muted">—</span>;
    case "secret":
      return <SecretValue conflict={conflict} side={side} row={row} />;
    case "value":
      return (
        <>
          {cell.lines.map((line, i) => (
            <div key={i} className="cell-line">
              {line}
            </div>
          ))}
        </>
      );
  }
}

/** What a missing version means, from what the backend tells us. */
function DeletedNote({ conflict, side }: { conflict: Conflict; side: ConflictSide }) {
  if (conflict[side] !== null) return null;
  // `other` is a deletion the other device made; `current` is gone from this vault since.
  const text =
    side === "other"
      ? `Deleted on another device, found ${foundAt(conflict)}`
      : "Deleted (no longer in your vault)";
  return (
    <div className="deleted-note" data-testid="deleted-side">
      {text}
    </div>
  );
}

interface ConflictDiffProps {
  conflict: Conflict;
  name: NameOf;
  /** Pick fields is on: the fields ticked so far. */
  picking: ReadonlySet<string> | null;
  onToggle(field: string): void;
}

/** The two versions of a record side by side, changed fields marked. */
export function ConflictDiff({ conflict, name, picking, onToggle }: ConflictDiffProps) {
  const rows = fieldRows(conflict.current, conflict.other, conflict.changedFields, { kind: conflict.kind, name });
  return (
    <div className="table-wrap">
      <table className="data-table conflict-table" aria-label="Versions">
        <thead>
          <tr>
            <th scope="col">Field</th>
            <th scope="col">
              Current version
              <DeletedNote conflict={conflict} side="current" />
            </th>
            <th scope="col">
              Other version
              <DeletedNote conflict={conflict} side="other" />
            </th>
          </tr>
        </thead>
        <tbody>
          {rows.map((row) => (
            <tr key={row.field} data-changed={row.changed ? "true" : "false"} className={row.changed ? "changed" : undefined}>
              <th scope="row">
                {picking && row.changed ? (
                  <label className="check-row">
                    <input
                      type="checkbox"
                      aria-label={`Take ${row.label} from the other version`}
                      checked={picking.has(row.field)}
                      onChange={() => onToggle(row.field)}
                    />
                    <span>{row.label}</span>
                  </label>
                ) : (
                  row.label
                )}
                {row.changed && <span className="changed-tag"> changed</span>}
              </th>
              <td>
                <CellView cell={row.current} conflict={conflict} side="current" row={row} />
              </td>
              <td>
                <CellView cell={row.other} conflict={conflict} side="other" row={row} />
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
