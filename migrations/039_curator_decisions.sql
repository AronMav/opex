CREATE TABLE curator_decisions (
    id          SERIAL      PRIMARY KEY,
    skill_name  TEXT        NOT NULL,
    action      TEXT        NOT NULL,  -- 'archive' | 'reject' | 'fix'
    reason      TEXT,
    decided_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_curator_decisions_skill
    ON curator_decisions(skill_name, decided_at DESC);
