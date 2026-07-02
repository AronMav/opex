-- Penalty is transient quality data (rolling 20-call window) that self-heals,
-- so we start fresh rather than backfill an ambiguous agent for existing rows.
DELETE FROM tool_quality;
ALTER TABLE tool_quality ADD COLUMN agent_name TEXT NOT NULL DEFAULT '';
ALTER TABLE tool_quality DROP CONSTRAINT tool_quality_pkey;
ALTER TABLE tool_quality ADD PRIMARY KEY (agent_name, tool_name);
