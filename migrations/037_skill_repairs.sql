CREATE TABLE pending_skill_repairs (
    id              UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    skill_name      TEXT        NOT NULL,
    agent_name      TEXT        NOT NULL,
    kind            TEXT        NOT NULL CHECK (kind IN ('fix','derived','captured')),
    diagnosis       TEXT        NOT NULL,
    status          TEXT        NOT NULL DEFAULT 'pending'
                                CHECK (status IN ('pending','in_progress','done','failed')),
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    resolved_at     TIMESTAMPTZ,
    resolution_note TEXT
);

CREATE INDEX idx_skill_repairs_status
    ON pending_skill_repairs (status, created_at DESC);
