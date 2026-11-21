/** Pure helpers over the entity lists the hosts page works with. */

import type { Group, Host, Id, Tag } from "../api/types";

const byName = (a: string, b: string) => a.localeCompare(b, undefined, { sensitivity: "base" });

export interface GroupNode {
  group: Group;
  children: GroupNode[];
}

/** Groups as a forest, siblings sorted by name. A group whose parent is missing is a root. */
export function buildTree(groups: Group[]): GroupNode[] {
  const nodes = new Map<Id, GroupNode>(groups.map((g) => [g.id, { group: g, children: [] }]));
  const roots: GroupNode[] = [];
  for (const node of nodes.values()) {
    const parent = node.group.parentId === null ? undefined : nodes.get(node.group.parentId);
    (parent ? parent.children : roots).push(node);
  }
  const sort = (list: GroupNode[]) => {
    list.sort((a, b) => byName(a.group.name, b.group.name));
    list.forEach((n) => sort(n.children));
  };
  sort(roots);
  return roots;
}

/** `Production / Web`; empty for no group. Guards against cycles. */
export function groupPath(groups: Group[], id: Id | null): string {
  const byId = new Map(groups.map((g) => [g.id, g]));
  const names: string[] = [];
  const seen = new Set<Id>();
  for (let cur = id; cur !== null && !seen.has(cur); ) {
    seen.add(cur);
    const g = byId.get(cur);
    if (!g) break;
    names.unshift(g.name);
    cur = g.parentId;
  }
  return names.join(" / ");
}

/** The group and everything below it. */
export function subtreeIds(groups: Group[], root: Id): Set<Id> {
  const found = new Set<Id>([root]);
  let grew = true;
  while (grew) {
    grew = false;
    for (const g of groups) {
      if (g.parentId !== null && found.has(g.parentId) && !found.has(g.id)) {
        found.add(g.id);
        grew = true;
      }
    }
  }
  return found;
}

export interface GroupOption {
  id: Id;
  path: string;
}

/** Every group (optionally minus some ids) with its full path, in tree order. */
/** Hosts in each group, counting those in its subgroups too. */
export function hostCounts(groups: Group[], hosts: Host[]): Map<Id, number> {
  const parent = new Map(groups.map((g) => [g.id, g.parentId]));
  const counts = new Map(groups.map((g) => [g.id, 0]));
  for (const h of hosts) {
    const seen = new Set<Id>();
    for (let id = h.groupId; id !== null && counts.has(id) && !seen.has(id); id = parent.get(id) ?? null) {
      seen.add(id);
      counts.set(id, (counts.get(id) ?? 0) + 1);
    }
  }
  return counts;
}

export function groupOptions(groups: Group[], exclude: ReadonlySet<Id> = new Set()): GroupOption[] {
  const out: GroupOption[] = [];
  const walk = (nodes: GroupNode[]) => {
    for (const n of nodes) {
      if (exclude.has(n.group.id)) continue;
      out.push({ id: n.group.id, path: groupPath(groups, n.group.id) });
      walk(n.children);
    }
  };
  walk(buildTree(groups));
  return out;
}

/** What deleting a group would take with it: groups below it and hosts anywhere in its subtree. */
export function groupContents(groups: Group[], hosts: Host[], id: Id) {
  const ids = subtreeIds(groups, id);
  return {
    subgroups: ids.size - 1,
    hosts: hosts.filter((h) => h.groupId !== null && ids.has(h.groupId)).length,
  };
}

export function sortHosts(hosts: Host[]): Host[] {
  return [...hosts].sort((a, b) => byName(a.label, b.label) || byName(a.address, b.address));
}

export function tagNames(host: Host, tags: Tag[]): string[] {
  const byId = new Map(tags.map((t) => [t.id, t.name]));
  return host.tagIds.flatMap((id) => byId.get(id) ?? []);
}

const TAG_COLORS = ["#5b9dff", "#4ec9a0", "#e5a64a", "#e06c8b", "#a78bfa", "#56c2d6", "#9acd5f", "#d98c5f"];

/** A small deterministic hash of a string; the source of every "stable pick from a palette of 8". */
export function stableHash(text: string): number {
  let h = 0;
  for (const ch of text) h = (h * 31 + (ch.codePointAt(0) ?? 0)) >>> 0;
  return h;
}

/** A stable colour for a new tag, from its name. */
export function tagColor(name: string): string {
  return TAG_COLORS[stableHash(name) % TAG_COLORS.length];
}
