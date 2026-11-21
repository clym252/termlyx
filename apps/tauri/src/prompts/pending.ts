import { useSyncExternalStore } from "react";
import type { PromptEvent } from "../api/types";

/*
 * The prompts waiting for an answer, as `PromptHost` queues them, for the
 * parts of the UI that say they are waiting for the user (a connecting
 * session's card while the host key dialog is open). `PromptHost` publishes;
 * nothing else writes.
 */

let pending: readonly PromptEvent[] = [];
const listeners = new Set<() => void>();

export function publishPending(queue: readonly PromptEvent[]): void {
  pending = queue;
  listeners.forEach((l) => l());
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/** Whether a host key prompt is waiting for `host:port` (`null`: no host, never). */
export function hostKeyPending(queue: readonly PromptEvent[], host: string | null, port: number): boolean {
  if (host === null) return false;
  const wanted = host.toLowerCase();
  return queue.some(
    ({ prompt }) =>
      (prompt.kind === "hostKeyUnknown" || prompt.kind === "hostKeyChanged") &&
      prompt.port === port &&
      prompt.host.toLowerCase() === wanted,
  );
}

export function useHostKeyPending(host: string | null, port: number): boolean {
  return useSyncExternalStore(subscribe, () => hostKeyPending(pending, host, port));
}
