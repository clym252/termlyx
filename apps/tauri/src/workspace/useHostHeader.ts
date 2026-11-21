import { useCallback, useEffect, useState } from "react";
import { api } from "../api";
import type { Group, Host, Identity } from "../api/types";
import { badgeIndex, effectiveLogin, initials } from "../hosts/browse";
import { useVaultChanged } from "../sync/vaultChanged";
import type { SessionHeader } from "./SessionStatus";
import type { TargetedPane } from "./model";

/** What a session card shows about a host, and where it connects (to match its prompts). */
export interface HostHeader {
  header: SessionHeader;
  host: string;
  port: number;
}

/** The card header of a host: badge as on the Hosts page, label, and `user@host:port`. */
export function hostHeader(host: Host, groups: Group[], identities: Identity[]): HostHeader {
  const { username, port } = effectiveLogin(host, groups, identities);
  return {
    header: {
      label: host.label,
      address: `${username ? `${username}@` : ""}${host.address}:${port}`,
      badge: { text: initials(host.label), colorIndex: badgeIndex(host.id) },
    },
    host: host.address,
    port,
  };
}

/** The header for a host pane (`null` for a local terminal, a DEV direct connect, or while loading). */
export function useHostHeader(pane: TargetedPane): HostHeader | null {
  const [found, setFound] = useState<HostHeader | null>(null);
  const hostId = pane.target.kind === "host" ? pane.target.hostId : null;
  const load = useCallback(async () => {
    if (!hostId) return;
    try {
      const [hosts, groups, identities] = await Promise.all([
        api.listEntities("host"),
        api.listEntities("group"),
        api.listEntities("identity"),
      ]);
      const host = hosts.find((h) => h.id === hostId);
      setFound(host ? hostHeader(host, groups, identities) : null);
    } catch (e) {
      console.debug("host details unavailable", e);
    }
  }, [hostId]);
  useEffect(() => {
    void load();
  }, [load]);
  useVaultChanged(load);
  return hostId ? found : null;
}
