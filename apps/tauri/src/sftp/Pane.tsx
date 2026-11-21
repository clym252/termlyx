import {
  useCallback,
  useEffect,
  useId,
  useMemo,
  useRef,
  useState,
  type DragEvent,
  type KeyboardEvent,
  type MouseEvent,
  type ReactNode,
} from "react";
import { IS_WINDOWS } from "../platform";
import { ConfirmDialog } from "../ui/Dialog";
import { describeError } from "../ui/errors";
import { ChmodDialog, NameDialog } from "./dialogs";
import { validateLocalName } from "./localNames";
import {
  escapeName,
  formatBytes,
  formatMode,
  formatTime,
  hasControlChars,
  isDriveRoot,
  isHidden,
  joinPath,
  parentPath,
  shownPath,
  sortEntries,
  THIS_PC,
  typedPath,
  type FileEntry,
  type Side,
  type SortDir,
  type SortKey,
} from "./paths";

/** What a pane needs from its file system. Remote-only operations are left out for the local pane. */
export interface PaneBackend {
  /**
   * The plain absolute form of a path the user typed or a link led to
   * (`~` expanded, relative paths resolved, `//` and a trailing `/` gone).
   * The pane shows, lists and reports only what this returns, so a job
   * never gets a `~` or a relative path.
   */
  resolve(path: string): Promise<string>;
  list(path: string): Promise<FileEntry[]>;
  mkdir(path: string): Promise<void>;
  rename?(from: string, to: string): Promise<void>;
  remove?(entry: FileEntry): Promise<void>;
  chmod?(path: string, mode: number): Promise<void>;
}

interface PaneProps {
  side: Side;
  title: string;
  backend: PaneBackend;
  initialPath: string;
  /** The top folder of a pane that cannot go above one (iOS: Tether's folder): Up stops there. */
  root?: string;
  /** More buttons for the toolbar, after the built-in ones (given the selection, if a function). */
  actions?: ReactNode | ((selection: FileEntry[]) => ReactNode);
  /** Changes to reload the listing (a transfer finished here). */
  refreshToken: number;
  /** The shown folder changed (so the other pane knows where a drop goes). */
  onPathChange(path: string): void;
  /** The Upload / Download button, with its label; gets the selection. */
  transferLabel: string;
  onTransfer(entries: FileEntry[]): void;
  /** A drag from this pane began / ended; entries are what is dragged. */
  onDragEntries(entries: FileEntry[] | null): void;
  /** Whether a drag now in progress may be dropped here. */
  acceptsDrop(): boolean;
  onDropEntries(): void;
}

type Dialog =
  | { kind: "rename"; entry: FileEntry }
  | { kind: "mkdir" }
  | { kind: "delete"; entries: FileEntry[] }
  | { kind: "chmod"; entry: FileEntry };

const COLUMNS: { key: SortKey; label: string }[] = [
  { key: "name", label: "Name" },
  { key: "size", label: "Size" },
  { key: "mtime", label: "Modified" },
];

function kindOf(entry: FileEntry): { label: string; glyph: string } {
  if (entry.isSymlink) return { label: "Symbolic link", glyph: "↪" };
  if (entry.isDir) return { label: "Folder", glyph: "▸" };
  return { label: "File", glyph: "·" };
}

/**
 * One side of the file manager: a path bar, a sortable list, and the
 * actions that side supports. Rows are selectable (click, Ctrl/Cmd+click,
 * Shift+click, arrow keys, Space), open with a double-click or Enter, and
 * draggable to the other pane.
 *
 * A failed navigation reports the error and keeps the folder shown. The
 * list text escapes line breaks (`\n`) but every path in a request is the
 * entry's real path.
 */
export function Pane(props: PaneProps) {
  const { side, title, backend, initialPath, refreshToken, onPathChange } = props;
  const [path, setPath] = useState(initialPath);
  const [entries, setEntries] = useState<FileEntry[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [draft, setDraft] = useState<string | null>(null);
  const [sort, setSort] = useState<{ key: SortKey; dir: SortDir }>({ key: "name", dir: "asc" });
  const [showHidden, setShowHidden] = useState(false);
  const [selected, setSelected] = useState<ReadonlySet<string>>(new Set());
  const [anchor, setAnchor] = useState<string | null>(null);
  const [focused, setFocused] = useState<string | null>(null);
  const [dialog, setDialog] = useState<Dialog | null>(null);
  const [deleteError, setDeleteError] = useState<string | null>(null);
  const [dropTarget, setDropTarget] = useState(false);
  const navSeq = useRef(0);
  const pathRef = useRef(path);
  const rows = useRef(new Map<string, HTMLTableRowElement>());
  const pathId = useId();

  /**
   * Shows `target` (resolved by the backend first). `quiet`: a failure is
   * not reported (probing whether a link is a folder).
   */
  const navigate = useCallback(
    async (target: string, opts: { quiet?: boolean } = {}): Promise<boolean> => {
      const seq = ++navSeq.current;
      setLoading(true);
      try {
        const resolved = await backend.resolve(target);
        if (seq !== navSeq.current) return false;
        const list = await backend.list(resolved);
        if (seq !== navSeq.current) return false;
        pathRef.current = resolved;
        setPath(resolved);
        setEntries(list);
        setError(null);
        setDraft(null);
        setSelected(new Set());
        setAnchor(null);
        setFocused(null);
        onPathChange(resolved);
        return true;
      } catch (e) {
        if (seq === navSeq.current && !opts.quiet) {
          // The error names the path; the bar goes back to the folder still shown.
          setError(describeError(e));
          setDraft(null);
        }
        return false;
      } finally {
        if (seq === navSeq.current) setLoading(false);
      }
    },
    [backend, onPathChange],
  );

  /** Reloads the shown folder, keeping the selection that still exists. */
  const reload = useCallback(async () => {
    const seq = ++navSeq.current;
    setLoading(true);
    try {
      const list = await backend.list(pathRef.current);
      if (seq !== navSeq.current) return;
      setEntries(list);
      setError(null);
      const present = new Set(list.map((e) => e.path));
      setSelected((s) => new Set([...s].filter((p) => present.has(p))));
    } catch (e) {
      if (seq === navSeq.current) setError(describeError(e));
    } finally {
      if (seq === navSeq.current) setLoading(false);
    }
  }, [backend]);

  useEffect(() => {
    void navigate(initialPath);
    // The pane is mounted per host and starts once.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const firstToken = useRef(refreshToken);
  useEffect(() => {
    if (refreshToken !== firstToken.current) void reload();
  }, [refreshToken, reload]);

  const visible = useMemo(
    () => sortEntries((entries ?? []).filter((e) => showHidden || !isHidden(e.name)), sort.key, sort.dir),
    [entries, showHidden, sort],
  );
  const hiddenCount = (entries?.length ?? 0) - (entries ?? []).filter((e) => !isHidden(e.name)).length;
  const selection = visible.filter((e) => selected.has(e.path));
  const single = selection.length === 1 ? selection[0] : null;
  // The local pane is in Windows mode on Windows: the names it accepts follow Windows rules,
  // and above a drive root is the list of drives.
  const windows = side === "local" && IS_WINDOWS;
  const atThisPc = windows && path === THIS_PC;
  const up = windows && isDriveRoot(path) ? THIS_PC : path === props.root ? null : parentPath(side, path);

  // -- selection ---------------------------------------------------------------

  function selectRow(entry: FileEntry, mods: { toggle: boolean; range: boolean }) {
    const { toggle, range } = mods;
    if (range && anchor) {
      const a = visible.findIndex((v) => v.path === anchor);
      const b = visible.findIndex((v) => v.path === entry.path);
      if (a >= 0 && b >= 0) {
        const [from, to] = a < b ? [a, b] : [b, a];
        setSelected(new Set(visible.slice(from, to + 1).map((v) => v.path)));
        setFocused(entry.path);
        return;
      }
    }
    setAnchor(entry.path);
    setFocused(entry.path);
    setSelected((s) => {
      if (!toggle) return new Set([entry.path]);
      const next = new Set(s);
      if (!next.delete(entry.path)) next.add(entry.path);
      return next;
    });
  }

  function open(entry: FileEntry) {
    if (entry.isDir) void navigate(entry.path);
    // A remote link reports its own attributes: listing it says whether it leads to a folder.
    else if (entry.isSymlink) void navigate(entry.path, { quiet: true });
  }

  const modsOf = (e: MouseEvent | KeyboardEvent) => ({ toggle: e.ctrlKey || e.metaKey, range: e.shiftKey });

  function onRowKey(entry: FileEntry, index: number, e: KeyboardEvent<HTMLTableRowElement>) {
    const focusAt = (i: number) => {
      const next = visible[Math.max(0, Math.min(visible.length - 1, i))];
      if (!next) return;
      e.preventDefault();
      if (e.shiftKey) selectRow(next, { toggle: false, range: true });
      else {
        setFocused(next.path);
        setSelected(new Set([next.path]));
        setAnchor(next.path);
      }
      rows.current.get(next.path)?.focus();
    };
    switch (e.key) {
      case "ArrowDown":
        return focusAt(index + 1);
      case "ArrowUp":
        return focusAt(index - 1);
      case "Home":
        return focusAt(0);
      case "End":
        return focusAt(visible.length - 1);
      case "Enter":
        e.preventDefault();
        return open(entry);
      case " ":
        e.preventDefault();
        return selectRow(entry, { toggle: true, range: false });
    }
  }

  // -- drag and drop -----------------------------------------------------------

  function onDragStart(entry: FileEntry, e: DragEvent<HTMLTableRowElement>) {
    const dragged = selected.has(entry.path) ? selection : [entry];
    if (!selected.has(entry.path)) {
      setSelected(new Set([entry.path]));
      setAnchor(entry.path);
    }
    e.dataTransfer.effectAllowed = "copy";
    e.dataTransfer.setData("text/plain", dragged.map((d) => d.path).join("\n"));
    props.onDragEntries(dragged);
  }

  function onDragOver(e: DragEvent) {
    if (!props.acceptsDrop()) return;
    e.preventDefault();
    e.dataTransfer.dropEffect = "copy";
    setDropTarget(true);
  }

  // -- actions -----------------------------------------------------------------

  // Windows file names are case-insensitive.
  const nameTaken = (name: string) =>
    (entries ?? []).some((e) => (windows ? e.name.toLowerCase() === name.toLowerCase() : e.name === name));
  const validateName = (name: string, current?: string): string | null => {
    if (name === "" || name === "." || name === "..") return "Enter a name.";
    if (name.includes("/") || (side === "local" && name.includes("\\"))) return "A name can't contain a slash.";
    if (hasControlChars(name)) return "A name can't contain line breaks or control characters.";
    const reason = side === "local" ? validateLocalName(name, windows) : null;
    if (reason) return reason;
    if (name !== current && nameTaken(name)) return "Something with that name is already here.";
    return null;
  };

  async function doDelete(list: FileEntry[]) {
    setDeleteError(null);
    try {
      for (const entry of list) await backend.remove?.(entry);
      setDialog(null);
    } catch (e) {
      setDeleteError(describeError(e));
    }
    await reload();
  }

  const canRename = !!backend.rename && single !== null && !hasControlChars(single.name);
  const hasDir = (dialog?.kind === "delete" ? dialog.entries : []).some((e) => e.isDir);

  return (
    <section
      className="pane"
      aria-label={`${side === "local" ? "Local" : "Remote"} files`}
      data-drop={dropTarget ? "true" : undefined}
      onDragOver={onDragOver}
      onDragLeave={(e) => {
        if (!e.currentTarget.contains(e.relatedTarget as Node | null)) setDropTarget(false);
      }}
      onDrop={(e) => {
        setDropTarget(false);
        if (!props.acceptsDrop()) return;
        e.preventDefault();
        props.onDropEntries();
      }}
    >
      <header className="pane-head">
        <h2>{title}</h2>
        <label className="row small">
          <input type="checkbox" checked={showHidden} onChange={(e) => setShowHidden(e.target.checked)} />
          Show hidden files
        </label>
      </header>

      <form
        className="path-bar"
        onSubmit={(e) => {
          e.preventDefault();
          if (draft !== null && draft !== "") void navigate(typedPath(draft, props.root));
        }}
      >
        {windows && !atThisPc && (
          <button type="button" onClick={() => void navigate(THIS_PC)}>
            This PC
          </button>
        )}
        <button type="button" disabled={up === null} onClick={() => up && void navigate(up)}>
          Up
        </button>
        <label htmlFor={pathId} className="visually-hidden">
          Path
        </label>
        <input
          id={pathId}
          className="mono"
          spellCheck={false}
          autoCapitalize="off"
          autoCorrect="off"
          autoComplete="off"
          value={draft ?? escapeName(shownPath(path, props.root))}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Escape") setDraft(null);
          }}
        />
        <button type="submit" disabled={draft === null}>
          Go
        </button>
        <button type="button" onClick={() => void reload()}>
          Refresh
        </button>
      </form>

      <div className="pane-actions" role="toolbar" aria-label={`${side === "local" ? "Local" : "Remote"} actions`}>
        <button type="button" disabled={selection.length === 0 || atThisPc} onClick={() => props.onTransfer(selection)}>
          {props.transferLabel}
        </button>
        <button type="button" disabled={atThisPc} onClick={() => setDialog({ kind: "mkdir" })}>
          New folder
        </button>
        {backend.rename && (
          <button type="button" disabled={!canRename} onClick={() => single && setDialog({ kind: "rename", entry: single })}>
            Rename
          </button>
        )}
        {backend.remove && (
          <button
            type="button"
            disabled={selection.length === 0}
            onClick={() => {
              setDeleteError(null);
              setDialog({ kind: "delete", entries: selection });
            }}
          >
            Delete
          </button>
        )}
        {backend.chmod && (
          <button type="button" disabled={!single || !single.mode} onClick={() => single && setDialog({ kind: "chmod", entry: single })}>
            Permissions
          </button>
        )}
        {typeof props.actions === "function" ? props.actions(selection) : props.actions}
      </div>

      {error && (
        <p role="alert" className="form-error">
          {error}
        </p>
      )}

      <div className="pane-scroll" aria-busy={loading}>
        <table className="file-table" role="grid" aria-label={`${side === "local" ? "Local" : "Remote"} listing`} aria-multiselectable="true">
          <thead>
            <tr>
              <th scope="col" className="kind-col">
                <span className="visually-hidden">Type</span>
              </th>
              {COLUMNS.map((c) => (
                <th
                  key={c.key}
                  scope="col"
                  role="columnheader"
                  aria-sort={sort.key === c.key ? (sort.dir === "asc" ? "ascending" : "descending") : "none"}
                >
                  <button
                    type="button"
                    className="sort-button"
                    onClick={() =>
                      setSort((s) => (s.key === c.key ? { key: c.key, dir: s.dir === "asc" ? "desc" : "asc" } : { key: c.key, dir: "asc" }))
                    }
                  >
                    {c.label}
                    <span aria-hidden="true">{sort.key === c.key ? (sort.dir === "asc" ? " ▲" : " ▼") : ""}</span>
                  </button>
                </th>
              ))}
              <th scope="col">Mode</th>
            </tr>
          </thead>
          <tbody>
            {visible.map((entry, index) => {
              const kind = kindOf(entry);
              const isSelected = selected.has(entry.path);
              const rovingStop = focused ? focused === entry.path : index === 0;
              return (
                <tr
                  key={entry.path}
                  ref={(el) => {
                    if (el) rows.current.set(entry.path, el);
                    else rows.current.delete(entry.path);
                  }}
                  aria-selected={isSelected}
                  tabIndex={rovingStop ? 0 : -1}
                  draggable={!atThisPc}
                  data-kind={entry.isDir ? "folder" : "file"}
                  onClick={(e) => selectRow(entry, modsOf(e))}
                  onDoubleClick={() => open(entry)}
                  onKeyDown={(e) => onRowKey(entry, index, e)}
                  onFocus={() => setFocused(entry.path)}
                  onDragStart={(e) => onDragStart(entry, e)}
                  onDragEnd={() => props.onDragEntries(null)}
                >
                  <td className="kind-col">
                    <span role="img" aria-label={kind.label} title={kind.label}>
                      {kind.glyph}
                    </span>
                  </td>
                  <th scope="row" role="rowheader" className="name-col" title={escapeName(entry.path)}>
                    {escapeName(entry.name)}
                  </th>
                  <td className="num">{entry.isDir ? "" : formatBytes(entry.size)}</td>
                  <td>{formatTime(entry.mtime)}</td>
                  <td className="mono small">{formatMode(entry.mode)}</td>
                </tr>
              );
            })}
          </tbody>
        </table>
        {entries && visible.length === 0 && !error && (
          <p className="muted empty">
            {hiddenCount > 0 ? `Only hidden files here (${hiddenCount}). Turn on Show hidden files to see them.` : "This folder is empty."}
          </p>
        )}
      </div>

      {dialog?.kind === "mkdir" && (
        <NameDialog
          title="New folder"
          label="Folder name"
          confirmLabel="Create"
          validate={(n) => validateName(n)}
          onClose={() => setDialog(null)}
          onSubmit={async (name) => {
            await backend.mkdir(joinPath(side, path, name));
            setDialog(null);
            await reload();
          }}
        />
      )}
      {dialog?.kind === "rename" && backend.rename && (
        <NameDialog
          title="Rename"
          label="New name"
          confirmLabel="Rename"
          initial={dialog.entry.name}
          validate={(n) => validateName(n, dialog.entry.name)}
          onClose={() => setDialog(null)}
          onSubmit={async (name) => {
            if (name !== dialog.entry.name) await backend.rename?.(dialog.entry.path, joinPath(side, path, name));
            setDialog(null);
            setSelected(new Set());
            await reload();
          }}
        />
      )}
      {dialog?.kind === "chmod" && backend.chmod && (
        <ChmodDialog
          name={escapeName(dialog.entry.name)}
          mode={(dialog.entry.mode ?? 0) & 0o7777}
          onClose={() => setDialog(null)}
          onSubmit={async (mode) => {
            await backend.chmod?.(dialog.entry.path, mode);
            setDialog(null);
            await reload();
          }}
        />
      )}
      {dialog?.kind === "delete" && (
        <ConfirmDialog
          title={dialog.entries.length === 1 ? "Delete 1 item?" : `Delete ${dialog.entries.length} items?`}
          confirmLabel="Delete"
          danger
          error={deleteError}
          onConfirm={() => void doDelete(dialog.entries)}
          onCancel={() => setDialog(null)}
        >
          <p>This cannot be undone.</p>
          <ul className="plain-list">
            {dialog.entries.map((e) => (
              <li key={e.path} className="mono">
                {escapeName(e.name)}
                {e.isDir ? "/" : ""}
              </li>
            ))}
          </ul>
          {hasDir && <p className="muted small">A folder must be empty to be deleted.</p>}
        </ConfirmDialog>
      )}
    </section>
  );
}
