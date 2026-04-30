// ── model-limits.ts ──────────────────────────────────────────────────────────
// Static context window size table for known model families.
// Partial prefix matching: "claude-sonnet-4.7" matches the "claude-sonnet-4" key.
// Longest matching prefix wins to avoid false matches.
// Unknown model → null (ContextBar hides itself).

export const MODEL_CONTEXT_LIMITS: Record<string, number> = {
  "claude-opus-4": 200_000,
  "claude-opus-4.7": 1_000_000,
  "claude-sonnet-4": 200_000,
  "claude-sonnet-4.7": 200_000,
  "claude-haiku-4": 200_000,
  "gpt-4o": 128_000,
  "gpt-4o-mini": 128_000,
  "gpt-4.1": 1_047_576,
  "o1": 200_000,
  "gemini-2.0-flash": 1_048_576,
  "gemini-2.5-flash": 1_048_576,
  "gemini-2.5-pro": 1_048_576,
};

/**
 * Return the context window size for the given model name, or null if unknown.
 * Matching is case-insensitive. Exact match is tried first; then the longest
 * matching prefix key wins (handles version suffixes like "-20250101").
 */
export function getContextLimit(model: string | null | undefined): number | null {
  if (!model) return null;
  const lower = model.toLowerCase();

  // Exact match first.
  if (MODEL_CONTEXT_LIMITS[lower] != null) return MODEL_CONTEXT_LIMITS[lower];

  // Prefix match — longest key wins.
  const keys = Object.keys(MODEL_CONTEXT_LIMITS).sort((a, b) => b.length - a.length);
  for (const k of keys) {
    if (lower.startsWith(k.toLowerCase())) return MODEL_CONTEXT_LIMITS[k];
  }

  return null;
}
