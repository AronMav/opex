-- 089: Deprecate the pending_messages table (A5 durable-delivery redesign,
-- 2026-07-18).
--
-- pending_messages was meant to persist a turn's final done/error frame when the
-- channel WS/adapter dropped before it could be delivered, then re-emit it on
-- reconnect. It never worked end-to-end: after the channel-WS refactor no code
-- path ever INSERTed a row (the sole save_pending calls lived inside the replay
-- loop's re-save branch), so the table was permanently empty. Worse, done/error
-- are request-response frames — they resolve an in-memory promise in the adapter,
-- which is gone after an adapter restart, so a replayed frame could never be
-- routed to the originating chat.
--
-- Durable delivery is now handled by the outbound_queue: the engine enqueues a
-- push-model `send_message` action (routed by context.chat_id) when the live
-- final frame can't be sent, and replay_outbound_queue re-delivers it (fresh
-- action_id) on the next reconnect, surviving restarts. The dead consumer
-- (replay_pending_messages), producer-less db::pending helpers, PendingMessage
-- type, and daily cleanup job have all been removed.
--
-- The table is deliberately retained for audit/rollback safety; this migration
-- is purely documentary so the sequence stays monotonic. Operators may drop it
-- manually once retention is no longer needed. The DO block is a no-op on fresh
-- databases where the table was never created.
DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM pg_catalog.pg_class c
        JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
        WHERE c.relname = 'pending_messages' AND n.nspname = 'public'
    ) THEN
        COMMENT ON TABLE pending_messages IS
            'DEPRECATED (m089, 2026-07-18): never wired (no producer); superseded by durable send_message actions on outbound_queue. No longer read/written.';
    END IF;
END $$;
