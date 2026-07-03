// Query params for GET /api/audit. Search is server-side (ILIKE over agent,
// event type, actor and details JSON) — see crates/opex-core/src/db/audit.rs.
export function buildAuditParams(opts: {
  pageSize: number;
  offset: number;
  agent: string;
  eventType: string;
  search: string;
}): Record<string, string> {
  const params: Record<string, string> = {
    limit: String(opts.pageSize),
    offset: String(opts.offset),
  };
  if (opts.agent !== "_all") params.agent = opts.agent;
  if (opts.eventType !== "_all") params.event_type = opts.eventType;
  const search = opts.search.trim();
  if (search) params.search = search;
  return params;
}
