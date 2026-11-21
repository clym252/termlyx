import { useCallback, useEffect, useState } from "react";
import { api } from "../api";
import type { Group, Host, Identity, Tag } from "../api/types";
import { useVaultChanged } from "../sync/vaultChanged";
import { describeError } from "../ui/errors";

export interface HostsData {
  groups: Group[];
  hosts: Host[];
  tags: Tag[];
  identities: Identity[];
}

/** Loads everything the hosts page shows; `reload` after any change. It also reloads when sync changes the vault. */
export function useHostsData() {
  const [data, setData] = useState<HostsData | null>(null);
  const [error, setError] = useState<string | null>(null);

  const reload = useCallback(async () => {
    try {
      const [groups, hosts, tags, identities] = await Promise.all([
        api.listEntities("group"),
        api.listEntities("host"),
        api.listEntities("tag"),
        api.listEntities("identity"),
      ]);
      setData({ groups, hosts, tags, identities });
      setError(null);
    } catch (e) {
      setError(describeError(e));
    }
  }, []);

  useEffect(() => {
    void reload();
  }, [reload]);
  useVaultChanged(reload);

  return { data, error, reload };
}
