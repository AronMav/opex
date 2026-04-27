-- 033_drop_parent_id_chunk_index.sql
-- Drop parent_id and chunk_index columns from memory_chunks. Defined in
-- migration 001 to support multi-chunk documents linked by parent_id, but
-- chunking threshold (1500 chars) was never crossed in production: 0/14
-- rows on Pi have non-null parent_id or chunk_index > 0. Removing the
-- columns lets us delete the entire chunking pipeline + hydeclaw-text crate.

DROP INDEX IF EXISTS idx_memory_parent;
ALTER TABLE memory_chunks DROP COLUMN IF EXISTS parent_id;
ALTER TABLE memory_chunks DROP COLUMN IF EXISTS chunk_index;
