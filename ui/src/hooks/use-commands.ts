import { useQuery } from "@tanstack/react-query";
import { apiGet } from "@/lib/api";
import type { CommandInfo, CommandListResponse } from "@/types/api";

/** Fetches the server-side slash-command registry (GET /api/commands) for the
 *  given agent, used to drive the web composer's `/`-autocomplete dropdown. */
export function useCommands(agent: string) {
  return useQuery({
    queryKey: ["commands", agent],
    queryFn: async (): Promise<CommandInfo[]> => {
      const j = await apiGet<CommandListResponse>(`/api/commands?agent=${encodeURIComponent(agent)}`);
      return j.commands ?? [];
    },
    staleTime: 60_000,
  });
}
