ALTER TABLE sessions
  ADD COLUMN IF NOT EXISTS compaction_state JSONB;

COMMENT ON COLUMN sessions.compaction_state IS
  'Compressor per-session state: {previous_summary, ineffective_count, compression_count}. NULL = no compaction yet.';
