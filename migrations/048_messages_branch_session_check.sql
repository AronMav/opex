-- Migration 048: enforce that branching pointers stay within the same session.
--
-- Audit 2026-05-08 finding: m012 added `parent_message_id` and
-- `branch_from_message_id` as plain UUID FKs to `messages(id)`. PostgreSQL
-- has no way to enforce a composite (session_id, id) constraint when the
-- referenced column is just the PK, so nothing prevented a forked or
-- mis-saved row from pointing across sessions. `resolveActivePath` in the UI
-- then walks a broken tree and returns dangling links.
--
-- Rather than add a UNIQUE (session_id, id) + composite FK (which would be a
-- much bigger schema change touching every existing index and FK), we add a
-- BEFORE INSERT/UPDATE trigger that rejects cross-session pointers. The
-- existing `ON DELETE SET NULL` semantics are preserved.
--
-- ROLLBACK:
--   DROP TRIGGER IF EXISTS trg_messages_check_branch_session ON messages;
--   DROP FUNCTION IF EXISTS messages_check_branch_session();

CREATE OR REPLACE FUNCTION messages_check_branch_session() RETURNS trigger AS $$
DECLARE
    parent_session UUID;
    fork_session UUID;
BEGIN
    IF NEW.parent_message_id IS NOT NULL THEN
        SELECT session_id INTO parent_session
            FROM messages WHERE id = NEW.parent_message_id;
        -- parent_session IS NULL means the referenced row no longer exists
        -- (the FK has ON DELETE SET NULL, so a deleted parent will already
        -- have nulled out NEW.parent_message_id by the time we get here on
        -- subsequent updates; this branch is a safety net for INSERTs that
        -- raced a parent delete and is intentionally permissive).
        IF parent_session IS NOT NULL AND parent_session <> NEW.session_id THEN
            RAISE EXCEPTION
                'parent_message_id % belongs to session %, not the inserted row''s session %',
                NEW.parent_message_id, parent_session, NEW.session_id
                USING ERRCODE = 'foreign_key_violation';
        END IF;
    END IF;

    IF NEW.branch_from_message_id IS NOT NULL THEN
        SELECT session_id INTO fork_session
            FROM messages WHERE id = NEW.branch_from_message_id;
        IF fork_session IS NOT NULL AND fork_session <> NEW.session_id THEN
            RAISE EXCEPTION
                'branch_from_message_id % belongs to session %, not the inserted row''s session %',
                NEW.branch_from_message_id, fork_session, NEW.session_id
                USING ERRCODE = 'foreign_key_violation';
        END IF;
    END IF;

    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS trg_messages_check_branch_session ON messages;
CREATE TRIGGER trg_messages_check_branch_session
    BEFORE INSERT OR UPDATE OF parent_message_id, branch_from_message_id, session_id
    ON messages
    FOR EACH ROW
    EXECUTE FUNCTION messages_check_branch_session();
