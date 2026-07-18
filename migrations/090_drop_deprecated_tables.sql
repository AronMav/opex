-- 090: Drop the four deprecated tables (deletion-completeness design, 2026-07-18).
-- pending_messages (m089-deprecated, never wired), video_jobs (m068, superseded by
-- handler_jobs), file_scenarios + file_scenario_outcomes (m069, superseded by the
-- File Handler Hub). Constants/tests stopped referencing pending_messages in the
-- same release (T1) — deploying this migration on an older binary would break
-- agent RENAME, hence single-release ordering. Operator exported file_scenarios
-- (4 rows) + video_jobs (11 rows) via pg_dump before this migration (runbook).
DROP TABLE IF EXISTS pending_messages;
DROP TABLE IF EXISTS video_jobs;
DROP TABLE IF EXISTS file_scenarios;
DROP TABLE IF EXISTS file_scenario_outcomes;
