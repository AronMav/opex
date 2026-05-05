ALTER TABLE messages ADD COLUMN compressed BOOLEAN NOT NULL DEFAULT FALSE;
CREATE INDEX idx_messages_session_compressed
    ON messages(session_id, compressed)
    WHERE compressed = TRUE;
