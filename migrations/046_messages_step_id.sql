-- Add step_id column to messages so each tool-loop iteration's row is
-- queryable by step number. Used by:
--   • analytics/observability (group messages of one turn by step)
--   • future UI features (per-step badges, iteration counts)
--   • convertHistory persistent step grouping (currently uses runtime
--     mergedIds; step_id offers a stable durable index)
--
-- NULL for legacy rows and rows that aren't part of a tool-loop iteration
-- (final assistant rows, user rows, tool-result rows). The frontend treats
-- NULL identically to "no step info" — backwards compatible.
ALTER TABLE messages ADD COLUMN IF NOT EXISTS step_id INT;

CREATE INDEX IF NOT EXISTS messages_session_step_idx
    ON messages (session_id, step_id)
    WHERE step_id IS NOT NULL;
