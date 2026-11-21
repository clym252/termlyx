import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api } from "../api";
import type { Direction, Host, Id } from "../api/types";
import { DEVICE_NOUN, IS_ANDROID, IS_IOS, IS_MOBILE, IS_WINDOWS } from "../platform";
import { useVaultChanged } from "../sync/vaultChanged";
import { describeError } from "../ui/errors";
import { Pane, type PaneBackend } from "./Pane";
import { baseName, driveEntries, escapeName, normalizePath, parentPath, THIS_PC, type FileEntry, type Side } from "./paths";
import { startTransfer } from "./plan";
import { TransferBar } from "./TransferBar";
import { transferStore } from "./transfers";
import { APP_NAME } from "../app/brand";

/** The host shown last, so coming back to the page reopens it. */
let lastHostId: Id | null = null;

type Status = { state: "idle" } | { state: "connecting" } | { state: "ready" } | { state: "error"; message: string };

/** The local pane's title: on iOS it is Tether's own folder, which the Files app shows under this name. */
const LOCAL_TITLE = IS_IOS ? `On My ${DEVICE_NOUN} › ${APP_NAME}` : IS_MOBILE ? `This ${DEVICE_NOUN} › ${APP_NAME}` : "This computer";

/** How long finished transfers wait before the panes reload (a folder of files finishes in bursts). */
const REFRESH_DELAY_MS = 250;

/**
 * Dual-pane file manager: this computer on the left, a vault host's SFTP
 * on the right. Drag files (or folders) from one pane to the other, or use
 * the Upload / Download button; a folder becomes one transfer per file.
 *
 * SFTP stays open when the page is left, so transfers carry on; it closes
 * when another host is chosen and nothing of the old one is running.
 */
export function SftpPage() {
  const [hosts, setHosts] = useState<Host[] | null>(null);
  const [hostId, setHostId] = useState<Id | null>(lastHostId);
  const [status, setStatus] = useState<Status>({ state: "idle" });
  const [error, setError] = useState<string | null>(null);
  // Android: a system picker is open or its copy runs. A second one would take over Tauri's one pending result, and
  // the first call would never finish.
  const [picking, setPicking] = useState(false);
  const [paths, setPaths] = useState<Record<Side, string>>({ local: "", remote: "" });
  const [tokens, setTokens] = useState<Record<Side, number>>({ local: 0, remote: 0 });
  const [notice, setNotice] = useState<string | null>(null);
  const [localRoot, setLocalRoot] = useState<string | undefined>(undefined);
  const [preparing, setPreparing] = useState(false);
  const [problems, setProblems] = useState<string[]>([]);
  const drag = useRef<{ side: Side; entries: FileEntry[] } | null>(null);
  const pathsRef = useRef(paths);
  pathsRef.current = paths;
  const openSeq = useRef(0);

  useEffect(() => {
    transferStore.listen(api);
  }, []);

  const loadHosts = useCallback(async () => {
    try {
      setHosts(await api.listEntities("host"));
    } catch (e) {
      setError(describeError(e));
    }
  }, []);
  useEffect(() => {
    void loadHosts();
  }, [loadHosts]);
  useVaultChanged(loadHosts);

  // iOS, Android: the local pane cannot go above Tether's folder.
  useEffect(() => {
    if (!IS_MOBILE) return;
    api
      .localResolve("~")
      .then((root) => setLocalRoot(normalizePath("local", root)))
      .catch((e: unknown) => setError(describeError(e)));
  }, []);

  const hostList = useMemo(
    () => [...(hosts ?? [])].sort((a, b) => a.label.localeCompare(b.label, undefined, { sensitivity: "base" })),
    [hosts],
  );
  const host = hostList.find((h) => h.id === hostId) ?? null;

  const connect = useCallback(async (id: Id) => {
    const seq = ++openSeq.current;
    setStatus({ state: "connecting" });
    try {
      await api.sftpOpen(id);
      if (seq === openSeq.current) setStatus({ state: "ready" });
    } catch (e) {
      if (seq === openSeq.current) setStatus({ state: "error", message: describeError(e) });
    }
  }, []);

  // Coming back to the page reopens the host it showed (a no-op if SFTP is still open).
  const resumed = useRef(false);
  useEffect(() => {
    if (resumed.current || !host) return;
    resumed.current = true;
    void connect(host.id);
  }, [host, connect]);

  function chooseHost(id: Id | "") {
    const previous = hostId;
    if (previous && previous !== id && !transferStore.hasLive(previous)) {
      api.sftpClose(previous).catch((e: unknown) => console.debug("sftp_close failed", e));
    }
    resumed.current = true;
    setProblems([]);
    setPaths({ local: "", remote: "" });
    if (!id) {
      openSeq.current++;
      lastHostId = null;
      setHostId(null);
      setStatus({ state: "idle" });
      return;
    }
    lastHostId = id;
    setHostId(id);
    void connect(id);
  }

  // A finished transfer reloads the pane that now has the file.
  useEffect(() => {
    if (status.state !== "ready" || !hostId) return;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const stale = new Set<Side>();
    const off = transferStore.onDone((done) => {
      if (done.hostId !== hostId) return;
      const side: Side = done.direction === "upload" ? "remote" : "local";
      const parent = parentPath(side, done.dest);
      if (parent === null || normalizePath(side, parent) !== pathsRef.current[side]) return;
      stale.add(side);
      clearTimeout(timer);
      timer = setTimeout(() => {
        const sides = [...stale];
        stale.clear();
        setTokens((t) => ({ ...t, ...Object.fromEntries(sides.map((s) => [s, t[s] + 1])) }));
      }, REFRESH_DELAY_MS);
    });
    return () => {
      off();
      clearTimeout(timer);
    };
  }, [status.state, hostId]);

  const transfer = useCallback(
    async (direction: Direction, entries: FileEntry[]) => {
      if (!hostId || entries.length === 0) return;
      const destDir = direction === "upload" ? pathsRef.current.remote : pathsRef.current.local;
      // Folders are shown by their resolved path; one not open yet (or a `~`) is no destination.
      if (destDir === "" || destDir.startsWith("~")) {
        setProblems(["The destination folder is not open yet."]);
        return;
      }
      if (IS_WINDOWS && destDir === THIS_PC) {
        setProblems(["Open a drive and a folder on it first."]);
        return;
      }
      setPreparing(true);
      setProblems([]);
      try {
        const result = await startTransfer(api, transferStore, hostId, direction, entries, destDir);
        setProblems(result.problems);
        // Folders made on the way show up.
        setTokens((t) => ({ ...t, [direction === "upload" ? "remote" : "local"]: t[direction === "upload" ? "remote" : "local"] + 1 }));
      } catch (e) {
        setProblems([describeError(e)]);
      } finally {
        setPreparing(false);
      }
    },
    [hostId],
  );

  // iOS: pick a file in Files; it is moved into Tether's folder.
  const importFromFiles = useCallback(async () => {
    setNotice(null);
    try {
      const picked = await api.pickFile({ title: "Import from Files" });
      if (picked === null) return;
      setError(null);
      const dest = await api.localImport(picked);
      setNotice(`Imported ${escapeName(baseName(dest))} into ${APP_NAME}'s folder.`);
      setTokens((t) => ({ ...t, local: t.local + 1 }));
    } catch (e) {
      setError(describeError(e));
    }
  }, []);

  // Android: pick files in the system picker; they are copied into the shown folder.
  const importFromDevice = useCallback(async () => {
    setNotice(null);
    setPicking(true);
    try {
      const names = await api.importFromDevice(pathsRef.current.local || "~");
      if (names.length > 0) {
        setError(null);
        setNotice(names.length === 1 ? `Imported ${escapeName(names[0])}.` : `Imported ${names.length} files.`);
      }
    } catch (e) {
      setError(describeError(e));
    } finally {
      setPicking(false);
    }
    // Files copied before a failure show too.
    setTokens((t) => ({ ...t, local: t.local + 1 }));
  }, []);

  // Android: the system "save as" picker; a local file goes to Downloads or any folder the user picks.
  const saveToDevice = useCallback(async (entry: FileEntry) => {
    setNotice(null);
    setPicking(true);
    try {
      if (await api.saveToDevice(entry.path)) {
        setError(null);
        setNotice(`Saved ${escapeName(entry.name)} to your ${DEVICE_NOUN}.`);
      }
    } catch (e) {
      setError(describeError(e));
    } finally {
      setPicking(false);
    }
  }, []);

  const setLocalPath = useCallback((p: string) => setPaths((s) => (s.local === p ? s : { ...s, local: p })), []);
  const setRemotePath = useCallback((p: string) => setPaths((s) => (s.remote === p ? s : { ...s, remote: p })), []);

  const localBackend = useMemo<PaneBackend>(
    () => ({
      resolve: async (path) => (IS_WINDOWS && path === THIS_PC ? path : normalizePath("local", await api.localResolve(path))),
      list: async (path) => (IS_WINDOWS && path === THIS_PC ? driveEntries(await api.localDrives()) : api.localList(path)),
      mkdir: (path) => api.localMkdir(path),
    }),
    [],
  );
  const remoteBackend = useMemo<PaneBackend | null>(
    () =>
      hostId
        ? {
            resolve: async (path) => {
              let target = path;
              if (target === "~" || target.startsWith("~/")) {
                target = `${await api.sftpRealpath(hostId, ".")}${target.slice(1)}`;
              }
              // Dot segments and relative paths are the server's to resolve (links may be involved).
              const plain = target.startsWith("/") && !/(^|\/)\.\.?(\/|$)/.test(target);
              return plain ? normalizePath("remote", target) : api.sftpRealpath(hostId, target);
            },
            list: (path) => api.sftpList(hostId, path),
            mkdir: (path) => api.sftpMkdir(hostId, path),
            rename: (from, to) => api.sftpRename(hostId, from, to),
            remove: (entry) => api.sftpRemove(hostId, entry.path, entry.isDir && !entry.isSymlink),
            chmod: (path, mode) => api.sftpChmod(hostId, path, mode),
          }
        : null,
    [hostId],
  );

  const ready = status.state === "ready" && host && remoteBackend;

  return (
    <section className="page sftp-page" aria-labelledby="sftp-title">
      <header className="page-head">
        <h1 id="sftp-title">SFTP</h1>
      </header>

      {error && (
        <p role="alert" className="form-error">
          {error}
        </p>
      )}

      {hosts && hostList.length === 0 ? (
        <p className="muted empty">No hosts yet. Add a host first; files are transferred over its connection.</p>
      ) : (
        <div className="field filter-field">
          <label htmlFor="sftp-host">Host</label>
          <select id="sftp-host" value={host?.id ?? ""} onChange={(e) => chooseHost(e.target.value)} disabled={!hosts}>
            <option value="">Choose a host…</option>
            {hostList.map((h) => (
              <option key={h.id} value={h.id}>
                {h.label}
              </option>
            ))}
          </select>
        </div>
      )}

      {notice && (
        <p role="status" className="muted">
          {notice}
        </p>
      )}
      {status.state === "connecting" && (
        <p role="status" className="muted">
          Connecting…
        </p>
      )}
      {status.state === "error" && host && (
        <div className="row">
          <p role="alert" className="form-error">
            Could not open SFTP on {host.label}: {status.message}
          </p>
          <button type="button" onClick={() => void connect(host.id)}>
            Try again
          </button>
        </div>
      )}
      {preparing && (
        <p role="status" className="muted">
          Preparing transfer…
        </p>
      )}
      {problems.length > 0 && (
        <div role="alert" className="form-error transfer-problems">
          <p>
            {problems.length === 1 ? "A problem" : `${problems.length} problems`} while preparing the transfer:
          </p>
          <ul className="plain-list">
            {problems.slice(0, 5).map((p, i) => (
              <li key={i} className="prewrap">
                {p}
              </li>
            ))}
            {problems.length > 5 && <li>…and {problems.length - 5} more</li>}
          </ul>
          <button type="button" onClick={() => setProblems([])}>
            Dismiss
          </button>
        </div>
      )}

      {ready && (
        <div className="sftp-panes">
          <Pane
            key={`local-${host.id}`}
            side="local"
            title={LOCAL_TITLE}
            backend={localBackend}
            initialPath="~"
            root={localRoot}
            actions={(selection) => (
              <>
                {IS_IOS && (
                  <button type="button" onClick={() => void importFromFiles()}>
                    Import from Files
                  </button>
                )}
                {IS_ANDROID && (
                  <>
                    <button type="button" disabled={picking} onClick={() => void importFromDevice()}>
                      Import from device
                    </button>
                    <button
                      type="button"
                      disabled={picking || selection.length !== 1 || selection[0].isDir}
                      onClick={() => void saveToDevice(selection[0])}
                    >
                      Save to device…
                    </button>
                  </>
                )}
              </>
            )}
            refreshToken={tokens.local}
            onPathChange={setLocalPath}
            transferLabel="Upload"
            onTransfer={(entries) => void transfer("upload", entries)}
            onDragEntries={(entries) => (drag.current = entries ? { side: "local", entries } : null)}
            acceptsDrop={() => drag.current?.side === "remote"}
            onDropEntries={() => {
              const dragged = drag.current;
              drag.current = null;
              if (dragged) void transfer("download", dragged.entries);
            }}
          />
          <Pane
            key={`remote-${host.id}`}
            side="remote"
            title={host.label}
            backend={remoteBackend}
            initialPath="."
            refreshToken={tokens.remote}
            onPathChange={setRemotePath}
            transferLabel="Download"
            onTransfer={(entries) => void transfer("download", entries)}
            onDragEntries={(entries) => (drag.current = entries ? { side: "remote", entries } : null)}
            acceptsDrop={() => drag.current?.side === "local"}
            onDropEntries={() => {
              const dragged = drag.current;
              drag.current = null;
              if (dragged) void transfer("upload", dragged.entries);
            }}
          />
        </div>
      )}

      <TransferBar />
    </section>
  );
}
