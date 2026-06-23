/** Читает новый ключ; при отсутствии — legacy, и переносит его в новый. PR3 удаляет. */
export function readWithLegacy(newKey: string, legacyKey: string): string | null {
  const v = localStorage.getItem(newKey);
  if (v !== null) return v;
  const legacy = localStorage.getItem(legacyKey);
  if (legacy !== null) localStorage.setItem(newKey, legacy);
  return legacy;
}
