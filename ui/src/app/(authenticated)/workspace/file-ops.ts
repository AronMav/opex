/** Build absolute-within-workspace from/to paths for a rename within the current folder. */
export function buildRenameTarget(currentPath: string, oldName: string, newName: string) {
  const prefix = currentPath ? `${currentPath}/` : "";
  return { from: `${prefix}${oldName}`, to: `${prefix}${newName}` };
}

/**
 * Percent-encode each path segment so that filenames with spaces, `#`, `?`, etc.
 * are safe to use in `/api/workspace/<path>` URLs.
 *
 * @example encodeWorkspacePath("agents/My Agent/SOUL.md") === "agents/My%20Agent/SOUL.md"
 */
export function encodeWorkspacePath(p: string): string {
  if (!p) return p;
  return p.split("/").map(encodeURIComponent).join("/");
}
