-- Widen agent_plans.day_plan_status CHECK to include 'paused' — budget-pause of
-- an auto-approved day plan (2026-07-14). Additive; existing values unchanged.
ALTER TABLE agent_plans DROP CONSTRAINT IF EXISTS agent_plans_day_plan_status_check;
ALTER TABLE agent_plans ADD CONSTRAINT agent_plans_day_plan_status_check
    CHECK (day_plan_status IS NULL OR day_plan_status IN ('pending','approved','done','dismissed','paused'));
