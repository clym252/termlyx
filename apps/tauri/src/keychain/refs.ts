import type { Group, Host, Id, Identity } from "../api/types";

/** Records that can hold a reference to an identity or a key. */
export interface RefSources {
  hosts: Host[];
  groups: Group[];
  identities: Identity[];
}

/** `Host web-1`, `Group Production`, `Identity ops` for the ids a `referenced` error lists. */
export function describeRefs(refs: readonly Id[], from: RefSources): string[] {
  return refs.map((id) => {
    const host = from.hosts.find((h) => h.id === id);
    if (host) return `Host ${host.label}`;
    const group = from.groups.find((g) => g.id === id);
    if (group) return `Group ${group.name}`;
    const identity = from.identities.find((i) => i.id === id);
    if (identity) return `Identity ${identity.label}`;
    return `Record ${id}`;
  });
}
