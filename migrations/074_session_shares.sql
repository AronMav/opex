-- Session sharing: durable, read-only shareable links (Tier-3 #6).
-- One active share per session; the unguessable `token` is the security
-- boundary (the GET /api/shares/{token} endpoint is auth-exempt, like the
-- HMAC-signed /api/uploads/* reads). Revoking deletes the row; ON DELETE
-- CASCADE cleans up if the session itself is removed.
CREATE TABLE IF NOT EXISTS session_shares (
    id         UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    token      TEXT NOT NULL UNIQUE,
    session_id UUID NOT NULL UNIQUE REFERENCES sessions(id) ON DELETE CASCADE,
    created_by TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_session_shares_token ON session_shares(token);
