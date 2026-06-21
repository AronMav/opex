-- migrations/057_session_goals_durable_redrive.sql
-- Durable re-drive for autonomous (cron) goal runs. Additive only.

-- Distinguish an autonomous cron-owned goal run from a /goal attached to a live
-- interactive chat session. ONLY origin='cron' rows are eligible for automatic
-- crash re-drive: an interactive /goal shares the user's live DM/web session and
-- must never be auto-continued into a human's conversation.
ALTER TABLE session_goals
  ADD COLUMN IF NOT EXISTS origin TEXT NOT NULL DEFAULT 'goal'
  CHECK (origin IN ('goal', 'cron'));

-- Backoff gate for the startup resumer: a row is skipped until now() reaches
-- next_redrive_at. NULL or past = eligible now.
ALTER TABLE session_goals
  ADD COLUMN IF NOT EXISTS next_redrive_at TIMESTAMPTZ;

-- Opt-in: a cron job whose autonomous_goal is non-null becomes a durable goal
-- session (origin='cron') that is re-driven to completion after a crash. NULL =
-- unchanged fire-and-forget cron behaviour.
ALTER TABLE scheduled_jobs
  ADD COLUMN IF NOT EXISTS autonomous_goal TEXT;

COMMENT ON COLUMN session_goals.origin IS
  'goal = /goal on a live interactive session (never auto-re-driven); cron = autonomous cron-owned run (eligible for crash re-drive).';
COMMENT ON COLUMN session_goals.next_redrive_at IS
  'Backoff gate for the startup resumer; NULL or past timestamp = eligible now.';
COMMENT ON COLUMN scheduled_jobs.autonomous_goal IS
  'Opt-in durable cron goal text. NULL = ordinary fire-and-forget cron job.';
