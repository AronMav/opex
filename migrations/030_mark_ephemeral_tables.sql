-- 030_mark_ephemeral_tables.sql
-- Tag tables whose contents are ephemeral and should be excluded from backups.
-- Used by gateway/handlers/backup.rs::ephemeral_tables() at backup time.
--
-- The tag MUST be at the start of the comment ("@hydeclaw:ephemeral...").
-- The discovery query uses LIKE '@hydeclaw:ephemeral%' (anchored).
--
-- Forward-compat: each COMMENT is wrapped in DO $$ IF EXISTS ... $$ so this
-- migration keeps replaying cleanly even if a later migration drops or renames
-- one of these tables. New migrations that drop a tagged table need no special
-- handling — the comment goes away with the table.
--
-- COMMENT ON is idempotent (overwrites silently); replay is safe.

DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM pg_tables WHERE schemaname='public' AND tablename='sessions') THEN
        COMMENT ON TABLE sessions IS '@hydeclaw:ephemeral chat session metadata, not backed up';
    END IF;
    IF EXISTS (SELECT 1 FROM pg_tables WHERE schemaname='public' AND tablename='messages') THEN
        COMMENT ON TABLE messages IS '@hydeclaw:ephemeral chat messages, not backed up';
    END IF;
    IF EXISTS (SELECT 1 FROM pg_tables WHERE schemaname='public' AND tablename='session_events') THEN
        COMMENT ON TABLE session_events IS '@hydeclaw:ephemeral session WAL journal';
    END IF;
    IF EXISTS (SELECT 1 FROM pg_tables WHERE schemaname='public' AND tablename='usage_log') THEN
        COMMENT ON TABLE usage_log IS '@hydeclaw:ephemeral token billing analytics';
    END IF;
    IF EXISTS (SELECT 1 FROM pg_tables WHERE schemaname='public' AND tablename='audit_log') THEN
        COMMENT ON TABLE audit_log IS '@hydeclaw:ephemeral security event log';
    END IF;
    IF EXISTS (SELECT 1 FROM pg_tables WHERE schemaname='public' AND tablename='audit_events') THEN
        COMMENT ON TABLE audit_events IS '@hydeclaw:ephemeral security event log (legacy)';
    END IF;
    IF EXISTS (SELECT 1 FROM pg_tables WHERE schemaname='public' AND tablename='notifications') THEN
        COMMENT ON TABLE notifications IS '@hydeclaw:ephemeral UI notifications';
    END IF;
    IF EXISTS (SELECT 1 FROM pg_tables WHERE schemaname='public' AND tablename='pending_approvals') THEN
        COMMENT ON TABLE pending_approvals IS '@hydeclaw:ephemeral in-flight tool approvals';
    END IF;
    IF EXISTS (SELECT 1 FROM pg_tables WHERE schemaname='public' AND tablename='pending_messages') THEN
        COMMENT ON TABLE pending_messages IS '@hydeclaw:ephemeral channel inbound queue';
    END IF;
    IF EXISTS (SELECT 1 FROM pg_tables WHERE schemaname='public' AND tablename='outbound_queue') THEN
        COMMENT ON TABLE outbound_queue IS '@hydeclaw:ephemeral channel outbound queue';
    END IF;
    IF EXISTS (SELECT 1 FROM pg_tables WHERE schemaname='public' AND tablename='memory_tasks') THEN
        COMMENT ON TABLE memory_tasks IS '@hydeclaw:ephemeral memory worker task queue';
    END IF;
    IF EXISTS (SELECT 1 FROM pg_tables WHERE schemaname='public' AND tablename='pairing_codes') THEN
        COMMENT ON TABLE pairing_codes IS '@hydeclaw:ephemeral OTP codes';
    END IF;
    IF EXISTS (SELECT 1 FROM pg_tables WHERE schemaname='public' AND tablename='cron_runs') THEN
        COMMENT ON TABLE cron_runs IS '@hydeclaw:ephemeral cron execution history';
    END IF;
    IF EXISTS (SELECT 1 FROM pg_tables WHERE schemaname='public' AND tablename='tool_execution_cache') THEN
        COMMENT ON TABLE tool_execution_cache IS '@hydeclaw:ephemeral semantic cache, regenerated on demand';
    END IF;
    IF EXISTS (SELECT 1 FROM pg_tables WHERE schemaname='public' AND tablename='stream_jobs') THEN
        COMMENT ON TABLE stream_jobs IS '@hydeclaw:ephemeral streaming job state';
    END IF;
    IF EXISTS (SELECT 1 FROM pg_tables WHERE schemaname='public' AND tablename='graph_extraction_queue') THEN
        COMMENT ON TABLE graph_extraction_queue IS '@hydeclaw:ephemeral knowledge graph processing queue';
    END IF;
    IF EXISTS (SELECT 1 FROM pg_tables WHERE schemaname='public' AND tablename='tasks') THEN
        COMMENT ON TABLE tasks IS '@hydeclaw:ephemeral internal task queue';
    END IF;
    IF EXISTS (SELECT 1 FROM pg_tables WHERE schemaname='public' AND tablename='task_steps') THEN
        COMMENT ON TABLE task_steps IS '@hydeclaw:ephemeral internal task queue (steps)';
    END IF;
END $$;
