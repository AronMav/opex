-- 076_memory_soul_columns.sql
-- Agent Soul foundation: memory stream events/reflections live in memory_chunks.
-- kind: 'fact' (everything pre-existing), 'event' (biography), 'reflection' (insight).
-- importance: LLM score 1-10 (soul retrieval scoring); 5.0 neutral for old rows.
-- lineage: reflection provenance — ids of chunks it was synthesized from (quarantine).
ALTER TABLE memory_chunks
  ADD COLUMN kind TEXT NOT NULL DEFAULT 'fact',
  ADD COLUMN importance REAL NOT NULL DEFAULT 5.0,
  ADD COLUMN lineage UUID[];

CREATE INDEX idx_memory_soul
  ON memory_chunks (agent_id, kind, created_at DESC)
  WHERE kind IN ('event', 'reflection');
