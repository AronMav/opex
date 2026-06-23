import { get, set, del } from "idb-keyval";
import type { PersistedClient, Persister } from "@tanstack/query-persist-client-core";

/**
 * IndexedDB persister for React Query cache.
 * Stores the dehydrated cache under a single IDB key.
 */
export function createIDBPersister(key: IDBValidKey = "opex-rq"): Persister {
  return {
    persistClient: async (client: PersistedClient) => {
      try {
        await set(key, client);
      } catch {
        // DataCloneError when cache contains non-serializable values (e.g. Error objects from failed queries).
        // Silently skip — cache will be rebuilt on next page load.
      }
    },
    restoreClient: () => get<PersistedClient>(key),
    removeClient: () => del(key),
  };
}
