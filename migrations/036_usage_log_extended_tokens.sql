-- Extended token tracking: cache (Anthropic, OpenAI), reasoning (o1/o3, R1, Gemini thinking).
-- All nullable: provider/model may not return them. NEVER sum to base input/output (subset).
ALTER TABLE usage_log
    ADD COLUMN IF NOT EXISTS cache_read_tokens INTEGER,
    ADD COLUMN IF NOT EXISTS cache_creation_tokens INTEGER,
    ADD COLUMN IF NOT EXISTS reasoning_tokens INTEGER;

COMMENT ON COLUMN usage_log.cache_read_tokens IS
    'Subset of input_tokens read from prompt cache (Anthropic cache_read_input_tokens, OpenAI cached_tokens, Gemini cachedContentTokenCount). NULL = unsupported by provider.';
COMMENT ON COLUMN usage_log.cache_creation_tokens IS
    'Subset of input_tokens written to prompt cache (Anthropic cache_creation_input_tokens). Cost x1.25 of base input. NULL = unsupported.';
COMMENT ON COLUMN usage_log.reasoning_tokens IS
    'Subset of output_tokens used for hidden reasoning (o1/o3, DeepSeek-R1, Gemini thinking). NULL = unsupported.';
