-- 068: Deprecate the legacy video_jobs queue (File Handler Hub, Phase 6).
--
-- The in-core video pipeline (video_worker.rs / video_summary.rs /
-- opex_db::video_jobs) has been replaced by the universal handler_jobs queue
-- (migration 067) + the Python summarize_video async handler. The video_jobs
-- table is NO LONGER read or written by any code path.
--
-- The table and its rows are deliberately retained for audit/rollback safety;
-- this migration is purely documentary so the sequence stays monotonic.
-- Operators may remove the table manually once retention is no longer needed.
--
-- The DO block is a no-op on fresh databases where video_jobs was never
-- created (migrations 064/065 create it; sqlx runs them before 068).
DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM pg_catalog.pg_class c
        JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
        WHERE c.relname = 'video_jobs' AND n.nspname = 'public'
    ) THEN
        COMMENT ON TABLE video_jobs IS
            'DEPRECATED (m068, 2026-06-30): superseded by handler_jobs. No longer read/written.';
    END IF;
END $$;
