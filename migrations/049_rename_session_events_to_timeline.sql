-- Migration 049: rename session_events to session_timeline.
--
-- Honest naming: this table is a chronological log of session lifecycle
-- events used for LoopDetector warm-up after restart, diagnostics, and
-- audit. It is NOT a Write-Ahead Log — there is no replay-based recovery.
-- The "WAL" framing it inherited from m013 overpromised; this rename
-- removes the misleading vocabulary.
--
-- Column names and event_type values are unchanged. Old migrations
-- (m013, m030) are append-only history and stay as-is.
--
-- ALTER TABLE RENAME is metadata-only in PostgreSQL — atomic, no data
-- copy. Idempotent via IF EXISTS so reruns are safe.

ALTER TABLE  IF EXISTS session_events                RENAME TO session_timeline;
ALTER INDEX  IF EXISTS idx_session_events_session    RENAME TO idx_session_timeline_session;
ALTER INDEX  IF EXISTS idx_session_events_type       RENAME TO idx_session_timeline_type;
