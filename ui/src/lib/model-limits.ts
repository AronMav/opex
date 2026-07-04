// ── model-limits.ts ──────────────────────────────────────────────────────────
// Static context window size table for known model families.
// Partial prefix matching: "claude-sonnet-4.7" matches the "claude-sonnet-4" key.
// Longest matching prefix wins to avoid false matches.
// Unknown model → null (ContextBar hides itself).

// NOTE: This table is a fallback only — the authoritative value comes from the
// backend via the SSE `data-session-id` event (`contextLimit` field) and is
// stored in AgentState.modelContextLimit. Update this table when real values
// are confirmed, but prefer the live backend value whenever available.
export const MODEL_CONTEXT_LIMITS: Record<string, number> = {
  // Anthropic Claude
  "claude-opus-4": 200_000,
  "claude-opus-4.7": 1_000_000,
  "claude-sonnet-4": 200_000,
  "claude-sonnet-4.7": 200_000,
  "claude-haiku-4": 200_000,
  // OpenAI
  "gpt-4o": 128_000,
  "gpt-4o-mini": 128_000,
  "gpt-4.1": 1_047_576,
  "o1": 200_000,
  // Google Gemini
  "gemini-2.0-flash": 1_048_576,
  "gemini-2.5-flash": 1_048_576,
  "gemini-2.5-pro": 1_048_576,
  // Zhipu GLM — real values from Ollama /api/show
  "glm-5.2": 1_000_000,
  "glm-5.1": 202_752,
  "glm-5": 202_752,
  "glm-4": 128_000,
  // Moonshot Kimi — real values from Ollama /api/show (matches kimi-k2.6)
  "kimi-k2": 262_144,
  "kimi-k1.5": 131_072,
  // DeepSeek — real values from Ollama /api/show (cloud tier)
  "deepseek-v4-pro": 1_048_576,
  "deepseek-v4": 1_048_576,
  "deepseek-v3": 128_000,
  "deepseek-chat": 128_000,
  "deepseek-r1": 128_000,
  // MiniMax — real values from Ollama /api/show (m3 matches minimax-m3:cloud)
  "minimax-m3": 524_288,
  "minimax-m2": 196_608,
  // Ollama local (common)
  "qwen2.5": 32_768,
  "qwen3": 32_768,
  "llama3": 8_192,
  "mistral": 32_768,
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
