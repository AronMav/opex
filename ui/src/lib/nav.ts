/** Pages that render their own full-height header (no shared mobile header). */
const PAGES_WITH_OWN_HEADER = new Set(["/chat", "/workspace"]);

/** Strip trailing slashes from a Next.js pathname. `next.config.ts` sets
 *  `trailingSlash: true` (static export), so `usePathname()` returns
 *  "/chat/" at runtime — any exact comparison against "/chat" must go
 *  through this first. */
export function normalizePathname(pathname: string): string {
  return pathname.replace(/\/+$/, "") || "/";
}

export function pageHasOwnHeader(pathname: string): boolean {
  return PAGES_WITH_OWN_HEADER.has(normalizePathname(pathname));
}
