-- Drop dead / duplicate indexes identified by the 2026-07-17 dead-code audit.
-- Index drops are safe and reversible (recreate to roll back); no code forces
-- these by name (the only REINDEX/hint in tests targets the m022 composite
-- idx_sessions_agent_user_channel_last_msg, which is NOT dropped here).
--
-- NOTE: messages.edited_at column drop is intentionally NOT included — it is
-- still read by the message-row struct + history SELECTs; dropping it needs a
-- coordinated core change first (deferred).

-- Superseded by the m022 composite idx_sessions_agent_user_channel_last_msg
-- (leading-column order covers agent_id / user_id prefix lookups).
DROP INDEX IF EXISTS idx_sessions_agent;
DROP INDEX IF EXISTS idx_sessions_user;

-- No query filters on (session_id, role) or (session_id, tool_call_id) — message
-- loads use idx_messages_session; role/tool_call_id are never WHERE-predicates.
DROP INDEX IF EXISTS idx_messages_role;
DROP INDEX IF EXISTS idx_messages_tool_call;

-- No query filters stream_jobs by status='running' (get_active_job filters by
-- session_id; the live terminal status is 'finished', not 'running').
DROP INDEX IF EXISTS idx_stream_running;

-- Duplicates the UNIQUE(token) index created by `token TEXT NOT NULL UNIQUE` (m074).
DROP INDEX IF EXISTS idx_session_shares_token;

-- Duplicates the (agent_id, code) PRIMARY KEY prefix (m006) — agent_id-only
-- lookups use the PK's leading column.
DROP INDEX IF EXISTS idx_pairing_codes_agent;
