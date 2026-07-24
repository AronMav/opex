-- F-03: cap memory-task retries. Previously `claim_next` reclaimed the oldest
-- `failed` task forever (no counter, no dead-letter), so a permanently-poison
-- task looped `failed → processing → failed` and starved newer failed tasks.
--
-- `attempts` counts (re)tries; `claim_next` only retries `failed` rows below
-- MEMORY_TASK_MAX_ATTEMPTS, moving exhausted ones to terminal status 'dead'.
ALTER TABLE memory_tasks ADD COLUMN IF NOT EXISTS attempts integer NOT NULL DEFAULT 0;

-- Backfill is unnecessary: existing failed/pending rows default to attempts=0,
-- giving every in-flight task a full retry budget from this migration on.
