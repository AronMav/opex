-- migrations/078_initiative_status_and_origin.sql
-- Phase 2A: allow cancelling a standing initiative goal + update origin comment
-- to reflect that initiative goals are now crash re-driven. Additive only.

-- session_goals.status CHECK (m056) did not allow 'cancelled'; phase 2A adds
-- owner-initiated cancellation of an active standing goal. The CHECK added in
-- 056 is an unnamed inline column constraint auto-named session_goals_status_check.
ALTER TABLE session_goals DROP CONSTRAINT IF EXISTS session_goals_status_check;
ALTER TABLE session_goals ADD CONSTRAINT session_goals_status_check
    CHECK (status IN ('active','paused','done','cleared','cancelled'));

-- m077 said initiative goals were NOT re-driven; phase 2A adds durable re-drive.
COMMENT ON COLUMN session_goals.origin IS
  'goal = interactive /goal (never re-driven); cron = autonomous cron (crash re-driven); initiative = owner-approved self-initiated (crash re-driven since phase 2A).';
