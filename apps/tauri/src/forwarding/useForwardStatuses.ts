import { useEffect, useState } from "react";
import { api } from "../api";
import type { ForwardStatus, Id } from "../api/types";

/**
 * Live status of every forward rule, keyed by rule id. It follows the
 * `forward` events (each carries one rule's status and touches only that
 * rule's entry) and starts from the backend's current statuses, which never
 * override an event that arrived first. A rule not in the map is stopped.
 */
export function useForwardStatuses(): Readonly<Record<Id, ForwardStatus>> {
  const [statuses, setStatuses] = useState<Record<Id, ForwardStatus>>({});

  useEffect(() => {
    let live = true;
    let unlisten: (() => void) | undefined;
    void api
      .onForward((e) => {
        if (live) setStatuses((prev) => ({ ...prev, [e.ruleId]: e.status }));
      })
      .then(async (off) => {
        if (!live) {
          off();
          return;
        }
        unlisten = off;
        try {
          const current = await api.forwardStatuses();
          if (!live) return;
          setStatuses((prev) => {
            const next = { ...prev };
            for (const entry of current) if (!(entry.ruleId in next)) next[entry.ruleId] = entry.status;
            return next;
          });
        } catch {
          // Events still keep the statuses current.
        }
      });
    return () => {
      live = false;
      unlisten?.();
    };
  }, []);

  return statuses;
}
