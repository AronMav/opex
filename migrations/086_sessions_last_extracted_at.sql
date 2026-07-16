-- Per-session watermark for incremental knowledge extraction: the created_at of
-- the newest message already summarized. NULL = never extracted (extract from
-- the start). Additive, history-preserving.
ALTER TABLE sessions ADD COLUMN last_extracted_at TIMESTAMPTZ;
