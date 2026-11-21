import { useRef } from "react";

/**
 * `found` (the record with `id` in the latest list), or, when it vanished
 * from the list (another device deleted it), the last version seen under
 * that `id`. An editor open on the record then stays open with what the
 * user typed; saving or deleting follows the backend's rules. `undefined`
 * if `id` is null or the record was never seen.
 */
export function useRemembered<T extends { id: string }>(id: string | null, found: T | undefined): T | undefined {
  const last = useRef<T | undefined>(undefined);
  if (found) last.current = found;
  if (id === null || last.current?.id !== id) {
    last.current = undefined;
    return found;
  }
  return found ?? last.current;
}
