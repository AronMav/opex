-- Session timeline (historical name: session_events, renamed by m049).
-- Chronological log of session lifecycle events. Used for LoopDetector
-- warm-up after restart, diagnostics, and audit. NOT a Write-Ahead Log:
-- no replay-based recovery. The "WAL" framing this migration originally
-- carried was misleading and was removed by m049 + accompanying docs.

CREATE TABLE session_events (
    id         BIGSERIAL PRIMARY KEY,
    session_id UUID NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    event_type TEXT NOT NULL,  -- 'running', 'tool_start', 'tool_end', 'done', 'failed', 'interrupted'
    payload    JSONB,          -- tool_call_id, tool_name for tool events; reason for failed
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_session_events_session ON session_events(session_id);
CREATE INDEX idx_session_events_type ON session_events(event_type) WHERE event_type IN ('running', 'tool_start');
