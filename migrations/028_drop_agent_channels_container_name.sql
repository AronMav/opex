-- Drop orphan `container_name` column from `agent_channels`.
--
-- The column was introduced when channel adapters ran as Docker containers.
-- The current architecture (CLAUDE.md "NOT Docker containers") spawns the
-- TypeScript/Bun channels process as a supervised child of core, not as a
-- container, so there is nothing meaningful to store here. No code path
-- writes to the column, no API DTO exposes it, no UI surface reads it.

ALTER TABLE agent_channels DROP COLUMN IF EXISTS container_name;
