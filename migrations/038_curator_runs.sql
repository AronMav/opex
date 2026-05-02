-- migrations/038_curator_runs.sql

CREATE TABLE IF NOT EXISTS curator_runs (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    started_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    finished_at         TIMESTAMPTZ,
    duration_ms         INT,
    triggered_by        TEXT NOT NULL DEFAULT 'cron',
    phase1_transitions  INT NOT NULL DEFAULT 0,
    phase2_repairs      INT NOT NULL DEFAULT 0,
    phase3_commands     INT NOT NULL DEFAULT 0,
    skipped_reason      TEXT,
    report_md           TEXT,
    error               TEXT
);

CREATE INDEX IF NOT EXISTS curator_runs_started_at_idx ON curator_runs (started_at DESC);
