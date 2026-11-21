import type { Direction, Id, TetherApi } from "../api/types";
import { describeError } from "../ui/errors";
import { IS_WINDOWS } from "../platform";
import { windowsLocalName } from "./localNames";
import { escapeName, isTransferable, joinPath, type FileEntry, type Side } from "./paths";
import type { TransferStore } from "./transfers";

/** Deepest folder nesting followed when a folder is expanded. */
export const MAX_DEPTH = 32;
/** The most files one drag may expand to. */
export const MAX_JOBS = 10_000;

export interface PlannedJob {
  local: string;
  remote: string;
}

export interface Plan {
  jobs: PlannedJob[];
  /** Things left out or not done, for the user. */
  problems: string[];
}

class TooMany extends Error {}

/**
 * The name a transferred entry gets at the destination: a download's local
 * name is made Windows-legal on Windows; every other name is kept.
 */
export function localDestName(direction: Direction, windows: boolean, name: string): string {
  return direction === "download" && windows ? windowsLocalName(name) : name;
}

const sideOf = (direction: Direction, role: "source" | "dest"): Side =>
  (direction === "upload") === (role === "source") ? "local" : "remote";

/**
 * Turns the dragged entries into one job per file. A folder is walked:
 * the matching folder is created at the destination (existing ones are
 * reused), its files come first, then its sub-folders. Symbolic links to
 * folders met *inside* a folder are not followed (a link back up would
 * never end); a link to a file is transferred as the file. Sockets, devices
 * and pipes are left out. A folder that cannot be read or created is
 * reported in `problems` and the rest goes on.
 *
 * On Windows (`windows`) a download's local files and folders get Windows-legal
 * names (see `localDestName`); the remote side keeps its names. Remote names
 * that map to one local name (`a:b` and `a_b`) become jobs with the same
 * destination: the transfer's conflict handling (ask) decides for the second.
 */
export async function planTransfer(
  api: TetherApi,
  hostId: Id,
  direction: Direction,
  sources: readonly FileEntry[],
  destDir: string,
  windows: boolean = IS_WINDOWS,
): Promise<Plan> {
  const srcSide = sideOf(direction, "source");
  const dstSide = sideOf(direction, "dest");
  const destName = (name: string) => localDestName(direction, windows, name);
  const plan: Plan = { jobs: [], problems: [] };

  const listDir = (path: string): Promise<FileEntry[]> =>
    srcSide === "local" ? api.localList(path) : api.sftpList(hostId, path);

  /** Whether the destination folder exists after this: it is made, or was there already. */
  async function ensureDir(path: string): Promise<boolean> {
    try {
      if (dstSide === "local") await api.localMkdir(path);
      else await api.sftpMkdir(hostId, path);
      return true;
    } catch (e) {
      if (dstSide === "remote") {
        // mkdir on an existing folder fails; a listing says whether it is one.
        try {
          await api.sftpList(hostId, path);
          return true;
        } catch {
          // Report the mkdir failure below.
        }
      }
      plan.problems.push(`Could not create ${escapeName(path)}: ${describeError(e)}`);
      return false;
    }
  }

  function addFile(entry: FileEntry, dest: string) {
    if (plan.jobs.length >= MAX_JOBS) throw new TooMany();
    plan.jobs.push(
      direction === "upload" ? { local: entry.path, remote: dest } : { local: dest, remote: entry.path },
    );
  }

  /** A symlink the listing could not classify (remote links report their own attributes): does it lead to a folder? */
  async function linksToDir(entry: FileEntry): Promise<boolean> {
    if (!entry.isSymlink || entry.isDir) return entry.isDir;
    if (srcSide === "local") return false;
    try {
      await api.sftpList(hostId, entry.path);
      return true;
    } catch {
      return false;
    }
  }

  async function walk(dir: FileEntry, dest: string, depth: number): Promise<void> {
    if (depth > MAX_DEPTH) {
      plan.problems.push(`${escapeName(dir.path)}: folders nested more than ${MAX_DEPTH} deep were skipped`);
      return;
    }
    let children: FileEntry[];
    try {
      children = await listDir(dir.path);
    } catch (e) {
      plan.problems.push(`Could not read ${escapeName(dir.path)}: ${describeError(e)}`);
      return;
    }
    if (!(await ensureDir(dest))) return;
    const folders: FileEntry[] = [];
    for (const child of children) {
      if (child.isSymlink && (await linksToDir(child))) {
        plan.problems.push(`${escapeName(child.path)}: a link to a folder was not followed`);
      } else if (child.isDir) {
        folders.push(child);
      } else if (!isTransferable(child)) {
        plan.problems.push(`${escapeName(child.path)}: not a regular file, skipped`);
      } else {
        addFile(child, joinPath(dstSide, dest, destName(child.name)));
      }
    }
    for (const folder of folders) await walk(folder, joinPath(dstSide, dest, destName(folder.name)), depth + 1);
  }

  try {
    for (const entry of sources) {
      const dest = joinPath(dstSide, destDir, destName(entry.name));
      if (entry.isDir || (await linksToDir(entry))) {
        await walk(entry, dest, 0);
      } else if (isTransferable(entry)) {
        addFile(entry, dest);
      } else {
        plan.problems.push(`${escapeName(entry.path)}: not a regular file, skipped`);
      }
    }
  } catch (e) {
    if (!(e instanceof TooMany)) throw e;
    plan.problems.push(`More than ${MAX_JOBS} files: only the first ${MAX_JOBS} were queued`);
  }
  return plan;
}

/**
 * Plans and queues a transfer: every file becomes a job with policy `ask`,
 * all in one batch. Returns the problems met (planning and queuing).
 * `windows` is as for `planTransfer`.
 */
export async function startTransfer(
  api: TetherApi,
  store: TransferStore,
  hostId: Id,
  direction: Direction,
  sources: readonly FileEntry[],
  destDir: string,
  windows: boolean = IS_WINDOWS,
): Promise<{ queued: number; problems: string[] }> {
  const plan = await planTransfer(api, hostId, direction, sources, destDir, windows);
  const batchId = store.newBatchId();
  // Named as the destination will be, like the file rows.
  store.nameBatch(
    batchId,
    sources.length === 1 ? localDestName(direction, windows, sources[0].name) : `${sources.length} items`,
  );
  const problems = [...plan.problems];
  let queued = 0;
  store.setQueuing(batchId, true);
  for (const job of plan.jobs) {
    // Cancelled (its row, Cancel all, quit) while the rest was queuing.
    if (store.isStopped(batchId)) break;
    try {
      const { jobId } = await api.transferEnqueue(hostId, { direction, ...job, policy: "ask" });
      store.register({ jobId, hostId, batchId, direction, ...job });
      queued++;
    } catch (e) {
      problems.push(`Could not queue ${escapeName(direction === "upload" ? job.local : job.remote)}: ${describeError(e)}`);
      // The same cause (SFTP closed, ...) would stop the rest too.
      break;
    }
  }
  store.setQueuing(batchId, false);
  return { queued, problems };
}
