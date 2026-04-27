-- 031_drop_archived_column.sql
-- Drop the `archived` column and its partial index on memory_chunks.
-- The column was added in migration 009 (memory_palace) but no code path
-- ever sets archived=true; the partial index `WHERE archived=true` has been
-- empty since creation. Safe to drop with IF EXISTS guards for replay safety.

DROP INDEX IF EXISTS idx_memory_chunks_archived;
ALTER TABLE memory_chunks DROP COLUMN IF EXISTS archived;
