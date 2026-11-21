import type { LocalEntry } from "../api/types";

/** Which file system a pane shows. */
export type Side = "local" | "remote";

/** What both panes list: a remote entry fits it too (`mode` 0 means unknown there). */
export type FileEntry = Omit<LocalEntry, "mode"> & { mode: number | null };

export type SortKey = "name" | "size" | "mtime";
export type SortDir = "asc" | "desc";

const S_IFMT = 0o170000;
const S_IFDIR = 0o040000;
const S_IFREG = 0o100000;
const S_IFLNK = 0o120000;

/** The separator a local path uses (Windows paths have backslashes only). */
function localSep(path: string): string {
  return path.includes("\\") && !path.includes("/") ? "\\" : "/";
}

/** The local pane's top level on Windows: the list of drives. It is shown, not a path on disk. */
export const THIS_PC = "This PC";

/** Whether `path` is a bare drive root (`C:\`), the folder above which is the drive list. */
export function isDriveRoot(path: string): boolean {
  return /^[A-Za-z]:[\\/]?$/.test(path);
}

/** The drive list as folders the pane can show and open. */
export function driveEntries(roots: readonly string[]): FileEntry[] {
  return roots.map((root) => ({ name: root, path: root, isDir: true, size: 0, mtime: 0, mode: null, isSymlink: false }));
}

/** `dir` and `name` joined. Paths are never quoted or normalised: they go to the backend as they are. */
export function joinPath(side: Side, dir: string, name: string): string {
  const sep = side === "remote" ? "/" : localSep(dir);
  if (dir === "") return name;
  return dir.endsWith(sep) ? `${dir}${name}` : `${dir}${sep}${name}`;
}

/** The directory above `path`, or `null` at the top (`/`, `C:\`, a bare `~`). */
export function parentPath(side: Side, path: string): string | null {
  if (side === "local" && path === "~") return null;
  const sep = side === "remote" ? "/" : localSep(path);
  let trimmed = path;
  while (trimmed.length > 1 && trimmed.endsWith(sep)) trimmed = trimmed.slice(0, -1);
  if (trimmed === sep || /^[A-Za-z]:\\?$/.test(trimmed)) return null;
  const i = trimmed.lastIndexOf(sep);
  if (i < 0) return side === "remote" ? "." : null;
  if (i === 0) return sep;
  const parent = trimmed.slice(0, i);
  return /^[A-Za-z]:$/.test(parent) ? `${parent}${sep}` : parent;
}

/**
 * `path` in plain form: repeated separators and a trailing one dropped, `.`
 * and `..` resolved (lexically). Only for paths already expanded (no `~`).
 * The root stays `/` (`C:\\` for a drive); an empty relative path is `.`.
 */
export function normalizePath(side: Side, path: string): string {
  const sep = side === "remote" ? "/" : localSep(path);
  const drive = side === "local" ? /^[A-Za-z]:(?=[\\/]|$)/.exec(path)?.[0] ?? "" : "";
  const rest = path.slice(drive.length);
  const absolute = drive !== "" || rest.startsWith(sep);
  const parts: string[] = [];
  for (const part of rest.split(sep)) {
    if (part === "" || part === ".") continue;
    if (part === "..") {
      if (parts.length > 0 && parts[parts.length - 1] !== "..") parts.pop();
      else if (!absolute) parts.push(part);
    } else parts.push(part);
  }
  const body = parts.join(sep);
  if (absolute) return `${drive}${sep}${body}`;
  return body === "" ? "." : body;
}

/**
 * `path` as a pane confined to `root` shows it (iOS: Tether's folder): `/`
 * for the root and `/a/b` below it, so the app's container path never shows.
 * Without a root, or for a path outside it, the path itself.
 */
export function shownPath(path: string, root: string | undefined): string {
  if (root === undefined) return path;
  if (path === root) return "/";
  return path.startsWith(`${root}/`) ? path.slice(root.length) : path;
}

/**
 * What the user typed in a confined pane's path box, as a real path: `/x` or
 * `x` means `x` under `root`; `~` (which the backend expands to the root) is
 * passed through. Without a root, the text itself.
 */
export function typedPath(text: string, root: string | undefined): string {
  if (root === undefined || text.startsWith("~")) return text;
  const rest = text.replace(/^\/+/, "");
  return rest === "" ? root : `${root}/${rest}`;
}

/** The last component of `path`. */
export function baseName(path: string): string {
  const trimmed = path.replace(/[\\/]+$/, "");
  const i = Math.max(trimmed.lastIndexOf("/"), trimmed.lastIndexOf("\\"));
  return i < 0 ? trimmed : trimmed.slice(i + 1);
}

/** Whether `name` has a control character (a line break, a tab, ...). */
export function hasControlChars(name: string): boolean {
  // eslint-disable-next-line no-control-regex
  return /[\u0000-\u001f\u007f]/.test(name);
}

/** A name for display: control characters become visible escapes (`\n`), so a line break cannot hide in a list. */
export function escapeName(name: string): string {
  // eslint-disable-next-line no-control-regex
  return name.replace(/[\u0000-\u001f\u007f]/g, (c) => {
    switch (c) {
      case "\n":
        return "\\n";
      case "\r":
        return "\\r";
      case "\t":
        return "\\t";
      default:
        return `\\x${c.charCodeAt(0).toString(16).padStart(2, "0")}`;
    }
  });
}

export function isHidden(name: string): boolean {
  return name.startsWith(".");
}

export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let value = n / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit++;
  }
  return `${value.toFixed(1)} ${units[unit]}`;
}

export function formatTime(unixSecs: number): string {
  if (!unixSecs) return "—";
  return new Date(unixSecs * 1000).toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" });
}

/** `drwxr-xr-x`, like `ls -l`; `—` when the mode is unknown. */
export function formatMode(mode: number | null): string {
  if (!mode) return "—";
  const type =
    { [S_IFDIR]: "d", [S_IFLNK]: "l", [S_IFREG]: "-", 0o010000: "p", 0o140000: "s", 0o060000: "b", 0o020000: "c" }[
      mode & S_IFMT
    ] ?? "?";
  const bit = (mask: number, ch: string) => (mode & mask ? ch : "-");
  const exec = (x: number, special: number, on: string, off: string) =>
    mode & special ? (mode & x ? on : off) : bit(x, "x");
  return (
    type +
    bit(0o400, "r") +
    bit(0o200, "w") +
    exec(0o100, 0o4000, "s", "S") +
    bit(0o040, "r") +
    bit(0o020, "w") +
    exec(0o010, 0o2000, "s", "S") +
    bit(0o004, "r") +
    bit(0o002, "w") +
    exec(0o001, 0o1000, "t", "T")
  );
}

/**
 * Regular files and folders can be transferred; sockets, devices and pipes
 * cannot. A link whose target is unknown (a remote link reports its own
 * mode) is tried: the server resolves it, and a dangling one fails alone.
 */
export function isTransferable(entry: FileEntry): boolean {
  if (entry.isDir || !entry.mode) return true;
  const type = entry.mode & S_IFMT;
  return type === S_IFREG || type === S_IFLNK;
}

const collator = new Intl.Collator(undefined, { numeric: true, sensitivity: "base" });

function byName(a: FileEntry, b: FileEntry): number {
  return collator.compare(a.name, b.name) || (a.name < b.name ? -1 : a.name > b.name ? 1 : 0);
}

/** Folders first, then by `key`; `dir` reverses only the key's order. */
export function sortEntries(entries: readonly FileEntry[], key: SortKey, dir: SortDir): FileEntry[] {
  const sign = dir === "asc" ? 1 : -1;
  return [...entries].sort((a, b) => {
    if (a.isDir !== b.isDir) return a.isDir ? -1 : 1;
    const primary = key === "name" ? byName(a, b) : key === "size" ? a.size - b.size : a.mtime - b.mtime;
    return primary * sign || byName(a, b);
  });
}
