ALTER TABLE messages ADD COLUMN IF NOT EXISTS compressed BOOLEAN NOT NULL DEFAULT FALSE;
CREATE INDEX IF NOT EXISTS idx_messages_session_compressed
    ON messages(session_id, compressed)
    WHERE compressed = TRUE;
