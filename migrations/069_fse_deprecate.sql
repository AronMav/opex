-- 069: Deprecate the legacy File Scenario Engine tables (File Handlers tab + FSE
-- retirement, 2026-07-01).
--
-- The legacy post-send "file-scenario chips" SSE affordance, the Telegram `fse:`
-- callback, the `file_scenario` agent tool, the `/api/file-scenarios/*` routes,
-- the in-core enrich sync-dispatch, and the startup seeder have all been removed.
-- The File Handler Hub (self-describing Python handlers in toolgate +
-- handler_jobs queue) supersedes them. Neither `file_scenarios` (m060) nor
-- `file_scenario_outcomes` (m061) is read or written by any surviving code path.
--
-- The tables and their rows are deliberately retained for audit/rollback safety;
-- this migration is purely documentary so the sequence stays monotonic.
-- Operators may remove the tables manually once retention is no longer needed.
--
-- The DO block is a no-op on fresh databases where these tables were never
-- created (migrations 060/061 create them; sqlx runs them before 069).
DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM pg_catalog.pg_class c
        JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
        WHERE c.relname = 'file_scenarios' AND n.nspname = 'public'
    ) THEN
        COMMENT ON TABLE file_scenarios IS
            'DEPRECATED (m069, 2026-07-01): superseded by the File Handler Hub (handler_jobs + toolgate handlers). No longer read/written.';
    END IF;

    IF EXISTS (
        SELECT 1 FROM pg_catalog.pg_class c
        JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
        WHERE c.relname = 'file_scenario_outcomes' AND n.nspname = 'public'
    ) THEN
        COMMENT ON TABLE file_scenario_outcomes IS
            'DEPRECATED (m069, 2026-07-01): superseded by the File Handler Hub. No longer read/written.';
    END IF;
END $$;
