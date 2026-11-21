/** Pure helpers behind the Hosts page's folder view: what shows at a level, how it is filtered and labelled. */

import type { Group, Host, Id, Identity } from "../api/types";
import { groupChain, inheritedSettings } from "./effective";
import { matchesTags } from "./filter";
import { sortHosts, stableHash, subtreeIds } from "./model";

const byName = (a: Group, b: Group) => a.name.localeCompare(b.name, undefined, { sensitivity: "base" });

/** Whether following parent links from `group` comes back to it. */
function inCycle(group: Group, byId: Map<Id, Group>): boolean {
  const seen = new Set<Id>([group.id]);
  for (let cur = group.parentId; cur !== null; ) {
    if (cur === group.id) return true;
    const next = byId.get(cur);
    if (!next || seen.has(cur)) return false;
    seen.add(cur);
    cur = next.parentId;
  }
  return false;
}

/**
 * Direct children of `parent`, sorted by name. At `null`: the top level, i.e.
 * groups with no parent, a missing parent, or sitting in a parent cycle (sync can
 * produce both; `buildTree` drops cycle members, so they could not be reached).
 */
export function childGroups(groups: Group[], parent: Id | null): Group[] {
  const byId = new Map(groups.map((g) => [g.id, g]));
  const list =
    parent === null
      ? groups.filter((g) => g.parentId === null || !byId.has(g.parentId) || inCycle(g, byId))
      : groups.filter((g) => g.parentId === parent);
  return list.sort(byName);
}

/**
 * Hosts to list at a level, sorted. At `null` (All hosts): every host, wherever it
 * lives (including one whose group is missing, which sync can leave). Otherwise only
 * the hosts directly in that group, not those of its subgroups.
 */
export function hostsAt(hosts: Host[], groupId: Id | null): Host[] {
  return sortHosts(groupId === null ? hosts : hosts.filter((h) => h.groupId === groupId));
}

/** Groups from the top level down to `current`; terminates on a cycle. */
export function breadcrumb(groups: Group[], current: Id | null): Group[] {
  return groupChain(groups, current);
}

/** The remembered group if it still exists, else the deepest surviving ancestor from `ancestry`, else the top level. */
export function resolveCurrentGroup(groups: Group[], remembered: Id | null, ancestry: Id[]): Id | null {
  const exists = (id: Id) => groups.some((g) => g.id === id);
  if (remembered !== null && exists(remembered)) return remembered;
  for (let i = ancestry.length - 1; i >= 0; i--) if (exists(ancestry[i])) return ancestry[i];
  return null;
}

/** The username and port a host connects with, after group inheritance. */
export function effectiveLogin(
  host: Host,
  groups: Group[],
  identities: Identity[],
): { username: string | null; port: number } {
  const inherited = inheritedSettings(groups, host.groupId);
  const identityId = host.settings.identityId ?? inherited.identityId.value;
  const username = identityId === null ? null : (identities.find((i) => i.id === identityId)?.username ?? null);
  return { username, port: host.settings.port ?? inherited.port.value };
}

/** `user@address`, or just `address`; `:port` appended when it is not 22. */
export function hostSubtitle(host: Host, groups: Group[], identities: Identity[]): string {
  const { username, port } = effectiveLogin(host, groups, identities);
  return `${username ? `${username}@` : ""}${host.address}${port === 22 ? "" : `:${port}`}`;
}

/** Whether the filter box or tag chips narrow anything. */
export function filterActive(text: string, tags: ReadonlySet<Id>): boolean {
  return text.trim() !== "" || tags.size > 0;
}

/** Hosts under `scope` (`null` = everywhere) matching every word of `text` and every selected tag, sorted. */
export function filterHosts(
  hosts: Host[],
  groups: Group[],
  identities: Identity[],
  scope: Id | null,
  text: string,
  tags: ReadonlySet<Id>,
): Host[] {
  const inScope = scope === null ? null : subtreeIds(groups, scope);
  const words = text.toLowerCase().split(/\s+/).filter(Boolean);
  return sortHosts(
    hosts.filter((h) => {
      if (inScope && (h.groupId === null || !inScope.has(h.groupId))) return false;
      if (!matchesTags(h, tags)) return false;
      if (words.length === 0) return true;
      const fields = [h.label, h.address, effectiveLogin(h, groups, identities).username ?? ""].map((s) =>
        s.toLowerCase(),
      );
      return words.every((w) => fields.some((f) => f.includes(w)));
    }),
  );
}

const segmenter = new Intl.Segmenter(undefined, { granularity: "grapheme" });
const graphemes = (text: string) => Array.from(segmenter.segment(text), (s) => s.segment);

/** One or two characters for a host's badge: word starts of the label, else its first characters; `?` if there are none. */
export function initials(label: string): string {
  const parts = label.split(/[\s\-_.]+/).filter(Boolean);
  const picked =
    parts.length >= 2 ? [graphemes(parts[0])[0], graphemes(parts[1])[0]] : graphemes(parts[0] ?? "").slice(0, 2);
  return picked.join("").toUpperCase() || "?";
}

/** Which of the 8 badge colours a host gets; the same hash as `tagColor`. */
export function badgeIndex(hostId: Id): number {
  return stableHash(hostId) % 8;
}
