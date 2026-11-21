import { useCallback, useEffect, useState } from "react";
import { api } from "../api";
import type { Snippet } from "../api/types";
import { useVaultChanged } from "../sync/vaultChanged";
import { describeError } from "../ui/errors";

/** Lists open now, told when this device saves or deletes a snippet (sync's changes come as vault-changed). */
const lists = new Set<() => void>();

/** Every snippet list reads again. */
export function snippetsChanged() {
  lists.forEach((reload) => void reload());
}

/** The vault's snippets, by label. `reload` after any change; it also reloads when sync changes the vault. */
export function useSnippets() {
  const [snippets, setSnippets] = useState<Snippet[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  const reload = useCallback(async () => {
    try {
      const all = await api.listEntities("snippet");
      setSnippets(all.sort((a, b) => a.label.localeCompare(b.label)));
      setError(null);
    } catch (e) {
      setError(describeError(e));
    }
  }, []);

  useEffect(() => {
    void reload();
  }, [reload]);
  useVaultChanged(reload);
  useEffect(() => {
    lists.add(reload);
    return () => {
      lists.delete(reload);
    };
  }, [reload]);

  return { snippets, error, reload };
}
