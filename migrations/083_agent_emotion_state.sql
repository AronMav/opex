-- Per-agent transient affective mood (emotion layer v1). Decay-on-read,
-- intensity-weighted blend-on-write. No CHECK constraints (values are bounded
-- in Rust; a text label column would otherwise need widening later).
CREATE TABLE IF NOT EXISTS agent_emotion_state (
    agent_id   TEXT PRIMARY KEY,
    valence    REAL NOT NULL DEFAULT 0,
    label      TEXT,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
