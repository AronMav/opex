-- migrations/072_sessions_chat_scope.sql
-- T03 triage Point 5 (P1): the channel-session lookup key was
-- (agent_id, user_id, channel) with `channel` a bare platform label
-- ("telegram", "discord", ...) — NOT the per-chat/group/thread id. One
-- Telegram user writing in group A and group B (or in DM) collapsed into the
-- SAME session, leaking group A's context/history into group B.
--
-- `chat_scope` is an additional, purely additive lookup predicate — NOT a
-- replacement for `channel` (which many call sites still read as the bare
-- platform label for routing replies back to the correct adapter; e.g.
-- send_channel_message, session_goals push-redrive, /status /new /reset
-- slash commands). It carries the adapter-supplied chat/group/thread
-- disambiguator (Telegram chat_id, Discord "guild_id:channel_id", Slack
-- channel id, Matrix room_id, ...). NULL when the platform has no such
-- concept (WhatsApp/email — user_id already IS the peer) or for web/UI/cron
-- sessions.
--
-- Lookups use `chat_scope IS NOT DISTINCT FROM $N` (NULL-safe equality) so a
-- `None` bind matches only NULL rows and a `Some(x)` bind matches only that
-- exact value — pre-migration rows (chat_scope IS NULL) are still found by
-- callers that pass no chat_scope (platforms with no chat concept), but are
-- NOT reused once a real chat_scope becomes available for that session's
-- platform — a one-time "start fresh" for already-active channel DMs on
-- upgrade, not a bug (explicitly acceptable per approval).
ALTER TABLE sessions ADD COLUMN IF NOT EXISTS chat_scope TEXT;

COMMENT ON COLUMN sessions.chat_scope IS
  'Per-chat/group/thread disambiguator from the adapter context (e.g. Telegram chat_id, "guild_id:channel_id" for Discord). NULL = no chat concept on this platform, or web/cron session. Used alongside channel/user_id/agent_id in the session lookup predicate to prevent cross-chat context leakage (T03 triage Point 5).';

-- Composite index for the (agent_id, user_id, channel, chat_scope) lookup
-- pattern used by resolve_active_dm_session / get_or_create_session /
-- find_active_session. Partial on last_message_at window is not indexable
-- generically (NOW()-based), so a plain btree on the lookup columns plus
-- last_message_at covers the ORDER BY ... LIMIT 1 access pattern.
CREATE INDEX IF NOT EXISTS idx_sessions_chat_scope_lookup
  ON sessions (agent_id, user_id, channel, chat_scope, last_message_at DESC);
