-- FSE per-file processing outcome record. Kept off session_timeline (which
-- LoopDetector warm-up scans). See spec §4.7.

CREATE TABLE file_scenario_outcomes (
    id           UUID PRIMARY KEY,
    session_id   UUID NOT NULL,
    upload_id    UUID NOT NULL,
    match_type   TEXT NOT NULL,
    scenario_id  UUID,                                  -- NULL for 0-binding / save fallback
    status       TEXT NOT NULL,                         -- ok | failed | unsupported | too_large | timeout
    reason       TEXT,                                  -- set for non-ok
    duration_ms  BIGINT NOT NULL,
    bytes        BIGINT NOT NULL,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX file_scenario_outcomes_session_idx ON file_scenario_outcomes (session_id);
CREATE INDEX file_scenario_outcomes_upload_idx  ON file_scenario_outcomes (upload_id);

COMMENT ON TABLE file_scenario_outcomes IS 'Per-file FSE processing outcomes (status/reason/duration/bytes/scenario). Authorization events go to audit_events, not here.';
