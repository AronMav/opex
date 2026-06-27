/** Build absolute-within-workspace from/to paths for a rename within the current folder. */
export function buildRenameTarget(currentPath: string, oldName: string, newName: string) {
  const prefix = currentPath ? `${currentPath}/` : "";
  return { from: `${prefix}${oldName}`, to: `${prefix}${newName}` };
}
