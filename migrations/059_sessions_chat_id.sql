-- migrations/059_sessions_chat_id.sql
-- Persist the originating channel chat_id on the session so an interactive
-- `/goal` interrupted by a restart can be re-notified via channel-push (e.g.
-- Telegram), not only the UI bell. Previously chat_id lived only in the
-- ephemeral per-message context. NULL for web/UI sessions. Additive.
ALTER TABLE sessions ADD COLUMN IF NOT EXISTS chat_id BIGINT;

COMMENT ON COLUMN sessions.chat_id IS
  'Originating channel chat_id (e.g. Telegram). Stamped from the incoming message context on each channel turn; NULL for web sessions. Used for channel-push notifications (interrupted interactive /goal).';
