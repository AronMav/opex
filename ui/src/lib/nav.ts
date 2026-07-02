/** Pages that render their own full-height header (no shared mobile header). */
const PAGES_WITH_OWN_HEADER = new Set(["/chat", "/workspace"]);

export function pageHasOwnHeader(pathname: string): boolean {
  const normalized = pathname.replace(/\/+$/, "") || "/";
  return PAGES_WITH_OWN_HEADER.has(normalized);
}
