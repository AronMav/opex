-- Universal durable async queue for File Handler Hub jobs.
-- Generalizes video_jobs (064/065): handler-agnostic, params/result are JSONB.
-- Carries BOTH upload-based (upload_id) and url-based (source_ref) sources so a
-- YouTube link and an attached video file both flow through the same queue.
CREATE TABLE handler_jobs (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    upload_id   UUID,
    source_ref  TEXT,
    handler_id  TEXT NOT NULL,
    agent_name  TEXT NOT NULL,
    session_id  UUID NOT NULL,
    params      JSONB NOT NULL DEFAULT '{}',
    status      TEXT NOT NULL DEFAULT 'queued'
                CHECK (status IN ('queued','processing','done','failed')),
    phase       TEXT,
    pct         INT,
    result      JSONB,
    attempts    INT  NOT NULL DEFAULT 0,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX handler_jobs_claim_idx ON handler_jobs (status, created_at);
