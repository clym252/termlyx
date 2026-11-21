import type { Host, Id, Tag } from "../api/types";
import { tagNames } from "./model";

/**
 * Free-text match used by global search: every whitespace-separated word of
 * the query must appear (case-insensitively) in the label, the address, or
 * one of the host's tag names. An empty query matches everything.
 */
export function matchesQuery(host: Host, tags: Tag[], query: string): boolean {
  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  if (words.length === 0) return true;
  const haystack = [host.label, host.address, ...tagNames(host, tags)].map((s) => s.toLowerCase());
  return words.every((w) => haystack.some((s) => s.includes(w)));
}

/** A host passes the tag filter when it carries every selected tag. */
export function matchesTags(host: Host, selected: ReadonlySet<Id>): boolean {
  for (const id of selected) if (!host.tagIds.includes(id)) return false;
  return true;
}
