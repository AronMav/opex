-- migrations/041_sessions_compression_chains.sql
ALTER TABLE sessions
  ADD COLUMN IF NOT EXISTS parent_session_id UUID REFERENCES sessions(id) NULL,
  ADD COLUMN IF NOT EXISTS end_reason        TEXT NULL;

COMMENT ON COLUMN sessions.parent_session_id IS
  'For compression chains: UUID of the session this was split from. NULL = root session.';
COMMENT ON COLUMN sessions.end_reason IS
  'Why this session ended: ''compression'' = split into child session. NULL = active or normal end.';

CREATE INDEX IF NOT EXISTS idx_sessions_parent_id
  ON sessions(parent_session_id)
  WHERE parent_session_id IS NOT NULL;
