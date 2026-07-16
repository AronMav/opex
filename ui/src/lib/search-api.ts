import { apiGet, apiFetchRaw } from "@/lib/api";
import type { BookmarkedResponse, SearchResponse } from "@/types/api";

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

/**
 * List bookmarked messages (search palette "Favourites" section, T7) — backed
 * by `GET /api/messages/bookmarked` — see
 * crates/opex-core/src/gateway/handlers/sessions.rs::api_list_bookmarked.
 * Same all/agent contract as {@link searchAll}: pass `all: true` to list across
 * every agent, otherwise `agent` is required server-side.
 */
export function listBookmarked(
  opts: { agent?: string; all?: boolean; limit?: number } = {},
): Promise<BookmarkedResponse> {
  const { agent, all = false, limit = 50 } = opts;
  const scope = all ? "all=true" : `agent=${encodeURIComponent(agent ?? "")}`;
  return apiGet<BookmarkedResponse>(`/api/messages/bookmarked?${scope}&limit=${limit}`);
}

/**
 * Set/clear a bookmark on a message — backed by `PATCH /api/messages/{id}/bookmark`,
 * which answers 204 No Content on success. Uses {@link apiFetchRaw} (not
 * `apiPatch`) because `apiPatch` always calls `resp.json()`, which throws on an
 * empty 204 body.
 */
export async function toggleBookmark(messageId: string, agent: string, bookmarked: boolean): Promise<void> {
  const resp = await apiFetchRaw(
    `/api/messages/${encodeURIComponent(messageId)}/bookmark?agent=${encodeURIComponent(agent)}`,
    {
      method: "PATCH",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ bookmarked }),
    },
  );
  if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
}
