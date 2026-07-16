import { apiGet } from "@/lib/api";
import type { SearchResponse } from "@/types/api";

/**
 * Cross-session full-text search (Ctrl+K palette). Backed by
 * `GET /api/sessions/search` — see
 * crates/opex-core/src/gateway/handlers/sessions.rs::api_search_sessions.
 *
 * Either `all: true` (search every agent's sessions) or `agent` (scope to one
 * agent) must be supplied — mirrors the backend contract, which 400s if
 * neither is present.
 */
export function searchAll(
  q: string,
  opts: { agent?: string; all?: boolean; limit?: number } = {},
): Promise<SearchResponse> {
  const { agent, all = false, limit = 30 } = opts;
  const scope = all ? "all=true" : `agent=${encodeURIComponent(agent ?? "")}`;
  return apiGet<SearchResponse>(
    `/api/sessions/search?q=${encodeURIComponent(q)}&${scope}&limit=${limit}`,
  );
}
