-- Migration 051: drop phantom tasks/task_steps tables.
--
-- These tables were created by m001 alongside a planned task-execution
-- subsystem that never received an executor. No code ever creates rows
-- in task_steps; no code ever transitions tasks from 'pending' to
-- 'running'. The REST API (GET/POST /api/tasks, /api/tasks/{id}/steps)
-- and the MCP callback handler (/api/mcp/callback) operate on rows that
-- never exist. Removed end-to-end (handlers, types, module, callback).
--
-- Idempotent via IF EXISTS so reruns are safe. CASCADE drops the FK
-- from task_steps -> tasks automatically.

DROP TABLE IF EXISTS task_steps;
DROP TABLE IF EXISTS tasks;
