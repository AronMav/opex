-- Migration 063: переименование функционального тега @hydeclaw:ephemeral → @opex:ephemeral.
-- Идемпотентно переустанавливает COMMENT ON TABLE для всех ephemeral-таблиц
-- (см. m030 + m050). Старые миграции не редактируются (checksum-safety).
DO $$
DECLARE
    t text;
    tables text[] := ARRAY[
        'sessions','messages','session_events','session_timeline','usage_log',
        'audit_log','audit_events','notifications','pending_approvals',
        'pending_messages','outbound_queue','memory_tasks','pairing_codes',
        'cron_runs','tool_execution_cache','stream_jobs',
        'graph_extraction_queue','tasks','task_steps'
    ];
    cur text;
BEGIN
    FOREACH t IN ARRAY tables LOOP
        IF to_regclass('public.' || t) IS NOT NULL THEN
            cur := obj_description(('public.' || t)::regclass, 'pg_class');
            IF cur IS NOT NULL AND cur LIKE '@hydeclaw:ephemeral%' THEN
                EXECUTE format(
                    'COMMENT ON TABLE public.%I IS %L',
                    t,
                    '@opex:ephemeral' || substring(cur from length('@hydeclaw:ephemeral') + 1)
                );
            END IF;
        END IF;
    END LOOP;
END $$;
