-- m047 — parallel_batch_id for tool calls executed in a parallel batch
-- See docs/superpowers/specs/2026-05-07-s2-identity-first-stream-objects-design.md (T3)
--
-- NULL semantics:
--   - non-tool messages → NULL
--   - single-tool turns → NULL (not a "batch")
--   - parallel-tool turns → all tool messages in the batch share one UUID

ALTER TABLE messages ADD COLUMN parallel_batch_id UUID;
CREATE INDEX messages_parallel_batch_idx ON messages(parallel_batch_id)
  WHERE parallel_batch_id IS NOT NULL;
