-- Curator run history: tracks each invocation of the skill curator pipeline.
CREATE TABLE curator_runs (
    id           UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    trigger      TEXT        NOT NULL DEFAULT 'cron',   -- 'cron' | 'manual'
    status       TEXT        NOT NULL DEFAULT 'running', -- 'running' | 'skipped' | 'done' | 'error'
    skip_reason  TEXT,
    phase1       INTEGER,
    phase2       INTEGER,
    phase3       INTEGER,
    report_md    TEXT,
    error        TEXT,
    started_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    finished_at  TIMESTAMPTZ
);

CREATE INDEX curator_runs_started_at_idx ON curator_runs (started_at DESC);
