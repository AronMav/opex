-- Message bookmarks (wave 2): NULL = not bookmarked.
ALTER TABLE messages ADD COLUMN bookmarked_at TIMESTAMPTZ;
CREATE INDEX idx_messages_bookmarked ON messages (bookmarked_at DESC) WHERE bookmarked_at IS NOT NULL;
