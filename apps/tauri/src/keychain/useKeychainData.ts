import { useCallback, useEffect, useState } from "react";
import { api } from "../api";
import type { Group, Host, Identity, SshKey } from "../api/types";
import { useVaultChanged } from "../sync/vaultChanged";
import { describeError } from "../ui/errors";

export interface KeychainData {
  keys: SshKey[];
  identities: Identity[];
  hosts: Host[];
  groups: Group[];
}

/** Keys and identities, plus the hosts and groups that can reference them. `reload` after any change; it also reloads when sync changes the vault. */
export function useKeychainData() {
  const [data, setData] = useState<KeychainData | null>(null);
  const [error, setError] = useState<string | null>(null);

  const reload = useCallback(async () => {
    try {
      const [keys, identities, hosts, groups] = await Promise.all([
        api.listEntities("ssh_key"),
        api.listEntities("identity"),
        api.listEntities("host"),
        api.listEntities("group"),
      ]);
      // Own logins of hosts and groups live in their editors, not here.
      setData({ keys, identities: identities.filter((i) => i.owner === null), hosts, groups });
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
