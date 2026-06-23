-- 022_data_layer_indexes: Phase 63 DATA-01 + DATA-05
-- Composite index to close the sessions hot-path (agent + user + channel + recent activity
-- list), and partial index to close the unread-notifications hot-path.
--
-- Plain CREATE INDEX (not CONCURRENTLY) is intentional on Pi:
--   - Tables are small in single-user deploy; index-build time is <1s on a full dump
--   - SHARE lock blocks concurrent writes but NOT concurrent reads — UI stays responsive
--   - CONCURRENTLY requires a non-transactional migration and complicates the sqlx pipeline
--
-- IF NOT EXISTS guards make re-running the migration idempotent (required for
-- TestHarness re-use and for safe re-run after partial failures).

-- ── DATA-01: composite btree for sessions list hot-path ────────────────────
-- Covers: WHERE agent_id = ? AND user_id = ? AND channel = ? ORDER BY last_message_at DESC
-- Also subsumes (in leading-column order) the existing idx_sessions_agent +
-- idx_sessions_user (two of the three single-column indexes from 001_init).
-- The existing single-column indexes are retained — PostgreSQL ignores them when
-- the composite is more selective, and dropping them is a separate cleanup migration.
CREATE INDEX IF NOT EXISTS idx_sessions_agent_user_channel_last_msg
    ON sessions (agent_id, user_id, channel, last_message_at DESC);

-- ── DATA-05: partial btree for unread-notifications hot-path ───────────────
-- The notifications table has NO user_id column (Opex is self-hosted
-- single-user; notifications are global). Partial predicate filters to
-- WHERE read = FALSE — queries MUST use the literal boolean to exercise
-- this index. Parameterised `WHERE read = $1` defeats the partial predicate
-- (see integration_data_layer_indexes.rs::partial_index_NOT_used_by_parameterised*).
CREATE INDEX IF NOT EXISTS idx_notifications_unread
    ON notifications (created_at DESC) WHERE read = FALSE;
