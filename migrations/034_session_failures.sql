-- Migration 034: structured session failure log.
--
-- When a chat session ends with `failed` status, we capture diagnostic data
-- here so an operator can investigate later (LLM/provider error patterns,
-- repeated tool failures, sub-agent timeouts, iteration limits, etc.).
--
-- The table is append-only — one row per terminal `failed` transition.
-- `last_tool_output` is truncated by the writer (≈2048 chars) to keep rows
-- compact even if a tool emitted a large payload before crashing.

CREATE TABLE IF NOT EXISTS session_failures (
    id                UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    session_id        UUID NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    agent_id          TEXT NOT NULL,
    failed_at         TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    failure_kind      TEXT NOT NULL,
    error_message     TEXT NOT NULL,
    last_tool_name    TEXT,
    last_tool_output  TEXT,
    llm_provider      TEXT,
    llm_model         TEXT,
    iteration_count   INTEGER,
    duration_secs     INTEGER,
    context_json      JSONB
);

CREATE INDEX IF NOT EXISTS idx_session_failures_agent_failed_at
    ON session_failures(agent_id, failed_at DESC);

CREATE INDEX IF NOT EXISTS idx_session_failures_session
    ON session_failures(session_id);
