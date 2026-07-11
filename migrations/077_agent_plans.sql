-- migrations/077_agent_plans.sql
-- Stage C «Initiative»: per-agent persistent plan object + widen session_goals.origin.
-- Additive only.

CREATE TABLE IF NOT EXISTS agent_plans (
    agent_id         TEXT PRIMARY KEY,
    current_focus    TEXT,
    proposals        JSONB NOT NULL DEFAULT '[]',
    last_proposal_at TIMESTAMPTZ,
    proposals_today  INT  NOT NULL DEFAULT 0,
    proposal_day     DATE,
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);

COMMENT ON TABLE agent_plans IS
  'Stage C initiative: per-agent persistent plan (current focus + owner-gated goal proposals).';

-- Widen origin to allow owner-approved self-initiated goals. The CHECK added in
-- 057 is an unnamed inline column constraint auto-named session_goals_origin_check.
ALTER TABLE session_goals DROP CONSTRAINT IF EXISTS session_goals_origin_check;
ALTER TABLE session_goals ADD CONSTRAINT session_goals_origin_check
    CHECK (origin IN ('goal','cron','initiative'));

COMMENT ON COLUMN session_goals.origin IS
  'goal = interactive /goal (never auto-re-driven); cron = autonomous cron run (crash re-driven); initiative = owner-approved self-initiated goal (NOT re-driven in v1).';
