import type { ConflictPolicy, Direction, Id, TetherApi, TransferEventPayload, Unlisten } from "../api/types";
import { baseName } from "./paths";

export type JobStatus = "queued" | "running" | "done" | "skipped" | "cancelled" | "failed";

export interface TransferItem {
  jobId: string;
  hostId: Id;
  /** Jobs enqueued by one drag or button press share a batch. */
  batchId: string | null;
  direction: Direction;
  local: string;
  remote: string;
  /** Last path component of the local file, for display. */
  name: string;
  status: JobStatus;
  bytes: number;
  total: number;
  error: string | null;
  /** Where the file ended up (a renamed upload differs from `remote`). */
  dest: string | null;
}

export const isFinished = (s: JobStatus) => s !== "queued" && s !== "running";

export interface JobMeta {
  jobId: string;
  hostId: Id;
  batchId: string | null;
  direction: Direction;
  local: string;
  remote: string;
}

/** A row of the transfers list: one job, or a batch of several (a folder drag). */
export type TransferRow =
  | { kind: "job"; item: TransferItem }
  | { kind: "batch"; batchId: string; label: string; direction: Direction; items: TransferItem[] };

/**
 * Groups jobs of the same batch into one row when the batch has more than
 * one job, in order of each row's first job. `labelOf` names a batch (else
 * "N files").
 */
export function groupTransfers(items: readonly TransferItem[], labelOf: (batchId: string) => string | null): TransferRow[] {
  const sizes = new Map<string, number>();
  for (const i of items) if (i.batchId) sizes.set(i.batchId, (sizes.get(i.batchId) ?? 0) + 1);
  const rows: TransferRow[] = [];
  const batches = new Map<string, Extract<TransferRow, { kind: "batch" }>>();
  for (const item of items) {
    const id = item.batchId;
    if (!id || (sizes.get(id) ?? 0) < 2) {
      rows.push({ kind: "job", item });
      continue;
    }
    let row = batches.get(id);
    if (!row) {
      row = { kind: "batch", batchId: id, label: labelOf(id) ?? `${sizes.get(id)} files`, direction: item.direction, items: [] };
      batches.set(id, row);
      rows.push(row);
    }
    row.items.push(item);
  }
  return rows;
}

type DoneListener = (done: { hostId: Id; direction: Direction; dest: string }) => void;

/**
 * The window's transfers, fed by `transfer` events, and the "apply to all
 * conflicts in this batch" answers. It outlives the SFTP page, so jobs
 * keep their rows (and their conflict answers) while another page shows.
 *
 * An event can arrive before the command that queued its job returns; the
 * row is made from the event and gets its details when the job is registered.
 */
export class TransferStore {
  private items = new Map<string, TransferItem>();
  private snapshot: readonly TransferItem[] = [];
  private listeners = new Set<() => void>();
  private doneListeners = new Set<DoneListener>();
  private batchOf = new Map<string, string>();
  private batchPolicy = new Map<string, ConflictPolicy>();
  private batchLabels = new Map<string, string>();
  /** Batches still being queued (see `startTransfer`), and those told to stop. */
  private queuing = new Set<string>();
  private stopped = new Set<string>();
  /** Answers chosen for a job whose batch is not registered yet. */
  private pendingPolicy = new Map<string, ConflictPolicy>();
  /** Rows made from an event whose job is not registered yet (its direction is unknown). */
  private unregistered = new Set<string>();
  private listening: { api: TetherApi; off: Promise<Unlisten> } | null = null;
  private batchCounter = 0;

  readonly subscribe = (cb: () => void): (() => void) => {
    this.listeners.add(cb);
    return () => this.listeners.delete(cb);
  };

  readonly getSnapshot = (): readonly TransferItem[] => this.snapshot;

  onDone(cb: DoneListener): () => void {
    this.doneListeners.add(cb);
    return () => this.doneListeners.delete(cb);
  }

  /** Starts following `api`'s transfer events (once per api; a different api replaces the first). */
  listen(api: TetherApi): void {
    if (this.listening?.api === api) return;
    const previous = this.listening;
    previous?.off.then((off) => off());
    this.listening = { api, off: api.onTransfer((e) => this.apply(e)) };
  }

  newBatchId(): string {
    return `batch-${Date.now().toString(36)}-${++this.batchCounter}`;
  }

  /** Names a batch for its group row (what was dragged: "photos", "3 items"). */
  nameBatch(batchId: string, label: string): void {
    this.batchLabels.set(batchId, label);
  }

  batchLabel(batchId: string): string | null {
    return this.batchLabels.get(batchId) ?? null;
  }

  /** `startTransfer` is queuing `batchId`'s jobs (`false`: done). */
  setQueuing(batchId: string, on: boolean): void {
    if (on) this.queuing.add(batchId);
    else {
      this.queuing.delete(batchId);
      this.stopped.delete(batchId);
    }
  }

  /** Whether `batchId` was told to stop queuing more jobs. */
  isStopped(batchId: string): boolean {
    return this.stopped.has(batchId);
  }

  /** Stops `batchId` from queuing more jobs (its queued ones are cancelled separately). */
  cancelBatch(batchId: string): void {
    if (this.queuing.has(batchId)) this.stopped.add(batchId);
  }

  /** Stops every batch that is still queuing (Cancel all, quit). */
  stopQueuing(): void {
    for (const batch of this.queuing) this.stopped.add(batch);
  }

  /** Queued and running jobs, oldest first; only those of `batchId` if given. */
  liveJobIds(batchId?: string): string[] {
    return [...this.items.values()]
      .filter((i) => !isFinished(i.status) && (batchId === undefined || i.batchId === batchId))
      .map((i) => i.jobId);
  }

  /** Adds a queued job's details (or completes the row an early event made). */
  register(meta: JobMeta): void {
    const existing = this.items.get(meta.jobId);
    const early = this.unregistered.delete(meta.jobId);
    this.items.set(meta.jobId, {
      bytes: 0,
      total: 0,
      error: null,
      dest: null,
      status: "queued",
      ...existing,
      ...meta,
      // The local file's name: a download is named as it is written here (Windows may have changed the remote name).
      name: baseName(meta.local),
    });
    if (meta.batchId) {
      this.batchOf.set(meta.jobId, meta.batchId);
      const chosen = this.pendingPolicy.get(meta.jobId);
      if (chosen) {
        this.pendingPolicy.delete(meta.jobId);
        this.batchPolicy.set(meta.batchId, chosen);
      }
    }
    this.publish();
    // A `done` that came first is announced now that its direction is known.
    const item = this.items.get(meta.jobId);
    if (early && item?.status === "done" && item.dest !== null) {
      this.notifyDone(item.hostId, item.direction, item.dest);
    }
  }

  /** The answer already given for `jobId`'s batch ("apply to all"), if any. */
  autoAnswer(jobId: string): ConflictPolicy | null {
    const batch = this.batchOf.get(jobId);
    return (batch && this.batchPolicy.get(batch)) || null;
  }

  /** Answers the remaining conflicts of `jobId`'s batch the same way. */
  rememberForBatch(jobId: string, policy: ConflictPolicy): void {
    const batch = this.batchOf.get(jobId);
    if (batch) this.batchPolicy.set(batch, policy);
    else this.pendingPolicy.set(jobId, policy);
  }

  clearFinished(): void {
    for (const [id, item] of this.items) {
      if (!isFinished(item.status)) continue;
      this.items.delete(id);
      this.batchOf.delete(id);
      this.pendingPolicy.delete(id);
    }
    // Forget the answers of batches with no row left.
    const live = new Set(this.batchOf.values());
    for (const batch of [...this.batchPolicy.keys()]) if (!live.has(batch)) this.batchPolicy.delete(batch);
    for (const batch of [...this.batchLabels.keys()]) if (!live.has(batch)) this.batchLabels.delete(batch);
    this.publish();
  }

  /** Whether any job of `hostId` is queued or running. */
  hasLive(hostId: Id): boolean {
    return [...this.items.values()].some((i) => i.hostId === hostId && !isFinished(i.status));
  }

  /** Applies a `transfer` event. */
  apply({ hostId, event }: TransferEventPayload): void {
    const known = this.items.get(event.job);
    const item: TransferItem = known ?? {
      jobId: event.job,
      hostId,
      batchId: null,
      direction: "upload",
      local: "",
      remote: "",
      name: "",
      status: "queued",
      bytes: 0,
      total: 0,
      error: null,
      dest: null,
    };
    // Nothing follows a job's final event.
    if (isFinished(item.status)) return;
    if (!known) this.unregistered.add(item.jobId);
    let done: { direction: Direction; dest: string } | null = null;
    switch (event.kind) {
      case "progress":
        Object.assign(item, { status: "running", bytes: event.bytes, total: event.total });
        break;
      case "done":
        Object.assign(item, { status: "done", dest: event.dest, bytes: item.total });
        done = { direction: item.direction, dest: event.dest };
        break;
      case "skipped":
        item.status = "skipped";
        break;
      case "cancelled":
        item.status = "cancelled";
        break;
      case "failed":
        Object.assign(item, { status: "failed", error: event.error });
        break;
    }
    this.items.set(item.jobId, item);
    this.publish();
    // Told once the job's direction is known (see `register`).
    if (done && known && !this.unregistered.has(item.jobId)) this.notifyDone(hostId, done.direction, done.dest);
  }

  private notifyDone(hostId: Id, direction: Direction, dest: string): void {
    for (const cb of [...this.doneListeners]) cb({ hostId, direction, dest });
  }

  private publish(): void {
    this.snapshot = [...this.items.values()].map((i) => ({ ...i }));
    for (const cb of [...this.listeners]) cb();
  }
}

/** The app's transfers. */
export const transferStore = new TransferStore();
