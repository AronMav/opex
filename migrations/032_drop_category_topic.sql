-- 032_drop_category_topic.sql
-- Drop category/topic columns from memory_chunks. Added in migration 009
-- (memory_palace) but never adopted: 0 non-null rows on production Pi after
-- 6+ months. Keeping them costs maintenance on every memory change.

DROP INDEX IF EXISTS idx_memory_category_topic;
ALTER TABLE memory_chunks DROP COLUMN IF EXISTS category;
ALTER TABLE memory_chunks DROP COLUMN IF EXISTS topic;
