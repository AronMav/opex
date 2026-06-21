-- migrations/058_session_goals_cron_job_link.sql
-- Link a cron-owned goal to its scheduled job so a re-firing cron job can
-- supersede its OWN prior in-flight goal — preventing two live drivers (and
-- double execution) for one logical cron job. NULL for /goal runs. Additive.
ALTER TABLE session_goals ADD COLUMN IF NOT EXISTS cron_job_id UUID;

COMMENT ON COLUMN session_goals.cron_job_id IS
  'For origin=cron goals: the scheduled_jobs.id that spawned this run. Lets a re-firing job supersede its prior active goal. NULL for /goal runs.';
