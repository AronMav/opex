-- Phase X uploads-to-db: polymorphic table for binary assets that must
-- survive deploy cycles. Replaces workspace/uploads/ filesystem storage.
-- See docs/superpowers/specs/2026-05-15-uploads-to-db-design.md.

CREATE TABLE uploads (
    id UUID PRIMARY KEY,
    owner_type TEXT NOT NULL,
    owner_id TEXT,
    mime TEXT NOT NULL,
    data BYTEA NOT NULL,
    sha256 BYTEA NOT NULL,
    size_bytes BIGINT NOT NULL CHECK (size_bytes >= 0 AND size_bytes <= 20971520),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    expires_at TIMESTAMPTZ
);

CREATE INDEX uploads_owner_idx ON uploads(owner_type, owner_id);
CREATE INDEX uploads_expires_idx ON uploads(expires_at) WHERE expires_at IS NOT NULL;
CREATE UNIQUE INDEX uploads_agent_icon_unique ON uploads(owner_id) WHERE owner_type = 'agent_icon';

COMMENT ON TABLE uploads IS 'Binary assets (agent icons, tool outputs, client uploads). One row per file. agent_icon rows: owner_id = agent name, expires_at NULL. tool_output / client_upload rows: owner_id = message UUID as string, expires_at = NOW() + retention.';
COMMENT ON COLUMN uploads.owner_type IS 'Discriminator: agent_icon | tool_output | client_upload';
COMMENT ON COLUMN uploads.owner_id IS 'For agent_icon: agent name. For tool_output / client_upload: message UUID as Uuid::to_string()';
COMMENT ON COLUMN uploads.sha256 IS '32-byte SHA-256 of data. For future dedup; ETag value (hex) for HTTP cache headers.';
COMMENT ON INDEX uploads_agent_icon_unique IS 'One icon per agent. INSERT must use ON CONFLICT (owner_id) WHERE owner_type = ''agent_icon''.';
