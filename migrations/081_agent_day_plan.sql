-- migrations/081_agent_day_plan.sql
-- B-wide: per-agent persistent daily plan (morning-generated, heartbeat-advanced).
-- Additive only.

ALTER TABLE agent_plans
    ADD COLUMN IF NOT EXISTS day_plan JSONB NOT NULL DEFAULT '[]',
    ADD COLUMN IF NOT EXISTS day_plan_current INT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS day_plan_date DATE,
    ADD COLUMN IF NOT EXISTS day_plan_status TEXT;

ALTER TABLE agent_plans DROP CONSTRAINT IF EXISTS agent_plans_day_plan_status_check;
ALTER TABLE agent_plans ADD CONSTRAINT agent_plans_day_plan_status_check
    CHECK (day_plan_status IS NULL OR day_plan_status IN ('pending','approved','done','dismissed'));

COMMENT ON COLUMN agent_plans.day_plan IS
  'B-wide daily plan: ordered [{session_id,intent,status}]; session_id null until approve.';

-- Persist the decompose-fallback flag so a stateless heartbeat advance (advance_one_chunk
-- called once per tick, no long-lived driver) does not retry an empty decompose forever.
ALTER TABLE session_goals
    ADD COLUMN IF NOT EXISTS decompose_failed BOOLEAN NOT NULL DEFAULT false;

-- Day-plan goals are advanced by day_plan_tick (heartbeat), NOT by the generic crash
-- redrive sweep. Mark them so list_redrivable can EXCLUDE them — otherwise a mid-day
-- deploy/restart spawns a continuous driver that races the per-tick advance (review H1).
ALTER TABLE session_goals
    ADD COLUMN IF NOT EXISTS day_plan_managed BOOLEAN NOT NULL DEFAULT false;
