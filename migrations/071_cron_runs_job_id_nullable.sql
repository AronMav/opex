-- Decouple cron_runs lifetime from scheduled_jobs.
--
-- Batch F / F1 regression: run_once dispatch now commits the one-shot
-- DELETE of `scheduled_jobs` BEFORE the side effect runs (hermes-parity
-- double-fire fix, commit 23ccbdf0). But `cron_runs.job_id` was
-- `NOT NULL REFERENCES scheduled_jobs(id) ON DELETE CASCADE`, so that
-- DELETE cascaded onto the `cron_runs` row inserted moments earlier for
-- THIS SAME run, and the post-dispatch `UPDATE cron_runs SET status = ...`
-- silently matched zero rows — the run history for every one-shot job was
-- destroyed on every execution.
--
-- Fix: make `job_id` nullable and switch the FK action to `ON DELETE SET
-- NULL`. Deleting the parent job now orphans (not destroys) its run
-- history; `cron.rs` history queries already LEFT JOIN `scheduled_jobs`
-- and tolerate a missing job row (`COALESCE(j.name, 'unknown')`).
ALTER TABLE cron_runs ALTER COLUMN job_id DROP NOT NULL;

ALTER TABLE cron_runs DROP CONSTRAINT cron_runs_job_id_fkey;

ALTER TABLE cron_runs
    ADD CONSTRAINT cron_runs_job_id_fkey
    FOREIGN KEY (job_id) REFERENCES scheduled_jobs(id) ON DELETE SET NULL;
