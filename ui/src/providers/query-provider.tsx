"use client"
import { PersistQueryClientProvider } from "@tanstack/react-query-persist-client"
import { createIDBPersister } from "@/lib/idb-persister"
import { queryClient } from "@/lib/query-client"

const persister = createIDBPersister()

export function QueryProvider({ children }: { children: React.ReactNode }) {
  return (
    <PersistQueryClientProvider
      client={queryClient}
      persistOptions={{
        persister,
        maxAge: 24 * 60 * 60 * 1000,
        // Cache-shape version. Restore DROPS persisted state whose buster
        // differs — bump this whenever a persisted query's data shape changes.
        // v1: sessions moved to useInfiniteQuery ({pages,pageParams}); tabs
        // still running the pre-infinite bundle keep writing the old flat
        // shape into the shared IDB key, and rehydrating it into the new
        // InfiniteQueryObserver crashes inside TanStack (`data.pages.length`).
        buster: "sessions-infinite-v1",
        dehydrateOptions: {
          // Only persist session-related queries — admin page caches not needed across refreshes
          shouldDehydrateQuery: (query) =>
            (query.queryKey as unknown[])[0] === "sessions",
        },
      }}
    >
      {children}
    </PersistQueryClientProvider>
  )
}
