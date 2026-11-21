import { useState, useSyncExternalStore } from "react";
import { api as appApi } from "../api";
import type { TetherApi } from "../api/types";
import { baseName, escapeName, formatBytes } from "./paths";
import { groupTransfers, isFinished, transferStore, type TransferItem, type TransferRow } from "./transfers";

export function useTransferItems(): readonly TransferItem[] {
  return useSyncExternalStore(transferStore.subscribe, transferStore.getSnapshot);
}

function describe(item: TransferItem): string {
  switch (item.status) {
    case "queued":
      return "Queued";
    case "running": {
      const pct = item.total > 0 ? Math.min(100, Math.floor((item.bytes / item.total) * 100)) : 0;
      return `${formatBytes(item.bytes)} of ${formatBytes(item.total)} (${pct}%)`;
    }
    case "done":
      return "Done";
    case "skipped":
      return "Skipped";
    case "cancelled":
      return "Cancelled";
    case "failed":
      return `Failed: ${item.error ?? "unknown error"}`;
  }
}

/** "3 of 1,480 files done, 2 failed, 5 cancelled" for a batch row. */
function describeBatch(items: readonly TransferItem[]): string {
  const count = (status: TransferItem["status"]) => items.filter((i) => i.status === status).length;
  const n = (v: number) => v.toLocaleString("en-US");
  const parts = [`${n(count("done"))} of ${n(items.length)} files done`];
  const failed = count("failed");
  const cancelled = count("cancelled");
  const skipped = count("skipped");
  if (failed) parts.push(`${n(failed)} failed`);
  if (cancelled) parts.push(`${n(cancelled)} cancelled`);
  if (skipped) parts.push(`${n(skipped)} skipped`);
  return parts.join(", ");
}

function cancelJobs(api: TetherApi, jobIds: readonly string[]) {
  for (const id of jobIds) {
    api.transferCancel(id).catch((e: unknown) => console.debug("transfer_cancel failed", e));
  }
}

/** The source path; a download saved under another name than the remote one also says where it goes. */
function itemTitle(item: TransferItem): string {
  if (item.direction === "upload") return escapeName(item.local);
  const from = escapeName(item.remote);
  return item.name === baseName(item.remote) ? from : `${from} \u2192 ${escapeName(item.name)}`;
}

function JobRow({ item, api, nested = false }: { item: TransferItem; api: TetherApi; nested?: boolean }) {
  const name = escapeName(item.name);
  return (
    <li className={nested ? "transfer-item nested" : "transfer-item"} data-status={item.status}>
      <span className="transfer-dir">{item.direction === "upload" ? "Upload" : "Download"}</span>
      <span className="transfer-name" title={itemTitle(item)}>
        {name}
      </span>
      <progress
        aria-label={name}
        max={Math.max(item.total, 1)}
        value={item.status === "done" ? Math.max(item.total, 1) : item.bytes}
      />
      <span className="transfer-state small" data-status={item.status}>
        {describe(item)}
      </span>
      {!isFinished(item.status) ? (
        <button type="button" aria-label={`Cancel ${name}`} onClick={() => cancelJobs(api, [item.jobId])}>
          Cancel
        </button>
      ) : (
        <span aria-hidden="true" />
      )}
    </li>
  );
}

function BatchRows({
  row,
  api,
  open,
  onToggle,
}: {
  row: Extract<TransferRow, { kind: "batch" }>;
  api: TetherApi;
  open: boolean;
  onToggle: () => void;
}) {
  const label = escapeName(row.label);
  const live = row.items.filter((i) => !isFinished(i.status)).map((i) => i.jobId);
  const finished = row.items.length - live.length;
  return (
    <>
      <li className="transfer-item transfer-group" data-status={live.length ? "running" : "done"}>
        <button
          type="button"
          className="transfer-dir transfer-toggle"
          aria-expanded={open}
          aria-label={`${open ? "Hide" : "Show"} files of ${label}`}
          onClick={onToggle}
        >
          {open ? "▾" : "▸"} {row.direction === "upload" ? "Upload" : "Download"}
        </button>
        <span className="transfer-name" title={label}>
          {label}
        </span>
        <progress aria-label={label} max={Math.max(row.items.length, 1)} value={finished} />
        <span className="transfer-state small">{describeBatch(row.items)}</span>
        {live.length > 0 ? (
          <button
            type="button"
            aria-label={`Cancel ${label}`}
            onClick={() => {
              transferStore.cancelBatch(row.batchId);
              cancelJobs(api, live);
            }}
          >
            Cancel
          </button>
        ) : (
          <span aria-hidden="true" />
        )}
      </li>
      {open && row.items.map((item) => <JobRow key={item.jobId} item={item} api={api} nested />)}
    </>
  );
}

/**
 * Queued, running and finished transfers with progress, a Cancel for each
 * live one, Cancel all, and Clear finished. The files of one folder drag
 * (a batch) share a row with its own Cancel, expandable to the files.
 * Finished rows stay until cleared.
 */
export function TransferBar({ api = appApi }: { api?: TetherApi }) {
  const items = useTransferItems();
  const [expanded, setExpanded] = useState<ReadonlySet<string>>(new Set());
  const live = items.filter((i) => !isFinished(i.status));
  const failed = items.filter((i) => i.status === "failed").length;
  const finished = items.length - live.length;
  const rows = groupTransfers(items, (b) => transferStore.batchLabel(b));
  const toggle = (batchId: string) =>
    setExpanded((prev) => {
      const next = new Set(prev);
      if (!next.delete(batchId)) next.add(batchId);
      return next;
    });

  return (
    <section className="transfer-bar" aria-label="Transfers">
      <header className="transfer-head">
        <h2>Transfers</h2>
        <span className="muted small" role="status">
          {items.length === 0
            ? "Nothing queued"
            : `${live.length} in progress, ${finished - failed} finished${failed ? `, ${failed} failed` : ""}`}
        </span>
        <button
          type="button"
          disabled={live.length === 0}
          onClick={() => {
            transferStore.stopQueuing();
            cancelJobs(api, transferStore.liveJobIds());
          }}
        >
          Cancel all
        </button>
        <button type="button" disabled={finished === 0} onClick={() => transferStore.clearFinished()}>
          Clear finished
        </button>
      </header>
      {items.length > 0 && (
        <ul className="transfer-list" aria-label="Transfer list">
          {rows.map((row) =>
            row.kind === "job" ? (
              <JobRow key={row.item.jobId} item={row.item} api={api} />
            ) : (
              <BatchRows
                key={row.batchId}
                row={row}
                api={api}
                open={expanded.has(row.batchId)}
                onToggle={() => toggle(row.batchId)}
              />
            ),
          )}
        </ul>
      )}
    </section>
  );
}
