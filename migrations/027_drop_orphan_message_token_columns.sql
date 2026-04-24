-- Drop orphan tokens_input / tokens_output columns from `messages`.
--
-- These columns have been declared since 001_init.sql but nothing ever
-- wrote to them. Per-LLM-call token accounting lives in `usage_log`
-- (row-per-call, keyed by agent_id + provider + model + session_id),
-- which is the single source of truth for billing, quotas and analytics.
--
-- The UI does not read `messages.tokens_input / tokens_output` either.
-- Dropping removes dead schema; a future per-message token feature
-- would re-introduce them together with proper write-side plumbing
-- and a UI consumer.

ALTER TABLE messages DROP COLUMN IF EXISTS tokens_input;
ALTER TABLE messages DROP COLUMN IF EXISTS tokens_output;
