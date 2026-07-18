import { useQuery } from "@tanstack/react-query";
import { apiGet, isBinaryFile } from "@/lib/api";
import { encodeWorkspacePath } from "@/app/(authenticated)/workspace/file-ops";
import type { WorkspaceFile } from "@/types/api";

export interface PromptEntry {
  title: string;
  body: string;
}

const PROMPTS_FILE_PATH = "prompts.md";

/** Per-agent starter-prompt library path within the workspace. Mirrors the
 *  shared `prompts.md` but scoped under the agent's own directory. */
export function agentPromptsPath(agent: string): string {
  return `agents/${agent}/prompts.md`;
}

/** Serialize prompt entries back into the `## Title` + body markdown format
 *  parsed by {@link parsePrompts}. Rows with a blank title AND blank body are
 *  dropped (an empty editor row shouldn't persist a stray heading). */
export function serializePrompts(entries: PromptEntry[]): string {
  return entries
    .filter((e) => e.title.trim() || e.body.trim())
    .map((e) => `## ${e.title.trim()}\n\n${e.body.trim()}\n`)
    .join("\n");
}

/** Parses the workspace prompt library (`workspace/prompts.md`) into a flat
 *  list of `{title, body}` entries. Each `## Heading` line starts a new
 *  prompt; its body is everything up to the next `## Heading` (or EOF).
 *  Sections with no non-whitespace body are dropped — a heading alone isn't
 *  a usable prompt template. Files with no `##` headings (or empty files)
 *  yield an empty list. */
export function parsePrompts(md: string): PromptEntry[] {
  if (!md) return [];
  const lines = md.split(/\r?\n/);
  const prompts: PromptEntry[] = [];
  let currentTitle: string | null = null;
  let currentBody: string[] = [];

  const flush = () => {
    if (currentTitle === null) return;
    const body = currentBody.join("\n").trim();
    if (body) prompts.push({ title: currentTitle.trim(), body });
  };

  for (const line of lines) {
    const heading = /^##\s+(.+?)\s*$/.exec(line);
    if (heading) {
      flush();
      currentTitle = heading[1];
      currentBody = [];
    } else if (currentTitle !== null) {
      currentBody.push(line);
    }
  }
  flush();

  return prompts;
}

/** Fetches and parses the workspace prompt library for the composer's slash
 *  autocomplete (prompt section) and the chat welcome screen suggestions.
 *  Fail-soft: a missing file (404) or any other fetch error resolves to an
 *  empty list rather than surfacing an error UI — the feature is optional. */
export function usePrompts(): { prompts: PromptEntry[]; isLoading: boolean } {
  const { data, isLoading } = useQuery({
    queryKey: ["prompts"],
    queryFn: async (): Promise<PromptEntry[]> => {
      try {
        const file = await apiGet<WorkspaceFile>(`/api/workspace/${encodeWorkspacePath(PROMPTS_FILE_PATH)}`);
        if (isBinaryFile(file)) return [];
        return parsePrompts(file.content);
      } catch {
        return [];
      }
    },
    staleTime: 60_000,
  });
  return { prompts: data ?? [], isLoading };
}

/** React-query key for an agent's per-agent prompt library. Exported so the
 *  editor can invalidate the exact key the welcome screen reads. */
export function agentPromptsKey(agentName: string | null | undefined) {
  return ["agent-prompts", agentName ?? ""] as const;
}

/** Fetches and parses an agent's per-agent prompt library
 *  (`workspace/agents/{agent}/prompts.md`) for the chat welcome screen. Same
 *  fail-soft contract as {@link usePrompts}: missing file / any error → empty
 *  list. Disabled (and empty) when no agent name is provided. */
export function useAgentPrompts(agentName: string | null | undefined): { prompts: PromptEntry[]; isLoading: boolean } {
  const { data, isLoading } = useQuery({
    queryKey: agentPromptsKey(agentName),
    enabled: !!agentName,
    queryFn: async (): Promise<PromptEntry[]> => {
      if (!agentName) return [];
      try {
        const file = await apiGet<WorkspaceFile>(`/api/workspace/${encodeWorkspacePath(agentPromptsPath(agentName))}`);
        if (isBinaryFile(file)) return [];
        return parsePrompts(file.content);
      } catch {
        return [];
      }
    },
    staleTime: 60_000,
  });
  return { prompts: data ?? [], isLoading };
}
