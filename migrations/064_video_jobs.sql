-- Durable async queue for FSE video summarization jobs.
-- Mirrors the memory_tasks claim/recover pattern (see opex-memory-worker).
CREATE TABLE video_jobs (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    session_id   UUID NOT NULL,
    agent_name   TEXT NOT NULL,
    channel_id   UUID,                       -- always NULL in v1 (web-only); reserved for Telegram
    source_type  TEXT NOT NULL CHECK (source_type IN ('file','url')),
    source_ref   TEXT NOT NULL,              -- signed upload URL or video link
    status       TEXT NOT NULL DEFAULT 'pending'
                 CHECK (status IN ('pending','processing','done','failed')),
    summary      TEXT,
    error        TEXT,
    attempts     INT  NOT NULL DEFAULT 0,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX video_jobs_claim_idx ON video_jobs (status, created_at);
