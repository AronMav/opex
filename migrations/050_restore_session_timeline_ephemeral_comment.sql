-- Migration 050: restore @hydeclaw:ephemeral comment on session_timeline.
--
-- m030 attached COMMENT ON TABLE session_events IS '@hydeclaw:ephemeral ...'
-- to mark the table as excluded from pg_dump backups. m049's ALTER TABLE
-- ... RENAME TO transferred the table catalog entry but dropped the
-- comment (PostgreSQL stores comments by object identity that includes
-- the name). This migration re-attaches the marker so backup discovery
-- continues to skip session_timeline rows — they are transient
-- diagnostics, not user data.

COMMENT ON TABLE session_timeline IS
    '@hydeclaw:ephemeral session timeline (chronological lifecycle log)';
