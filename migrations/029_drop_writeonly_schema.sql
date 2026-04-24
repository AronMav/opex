-- Drop write-only schema: skill_metrics table + tool_execution_cache.query_text.
--
-- Audit of the working DB (2026-04-24) found two write-only paths with
-- zero reader anywhere in the Rust code, UI, or HTTP API surface:
--
-- 1. `skill_metrics` — the entire table. `record_outcome()` writes
--    times_applied / times_success / times_fail / effectiveness_score
--    on every scheduled-task skill use via UPSERT, but nothing ever
--    SELECTs from the table. No API endpoint, no UI page, no cron
--    job reads it. Four columns of accumulating analytics garbage.
--
-- 2. `tool_execution_cache.query_text` — lookups match via embedding
--    similarity (`query_embedding <=> $2::vector`). The `query_text`
--    column is only written, never consulted during lookup.
--
-- The companion code removal:
--   * delete crates/hydeclaw-core/src/db/skill_metrics.rs
--   * drop `pub mod skill_metrics;` from db/mod.rs
--   * remove record_outcome call in skills/evolution.rs
--   * remove query_text from semantic_cache::store INSERT

DROP TABLE IF EXISTS skill_metrics;

ALTER TABLE tool_execution_cache DROP COLUMN IF EXISTS query_text;
