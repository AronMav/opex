CREATE TABLE session_goals (
    session_id   UUID PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
    goal_text    TEXT NOT NULL,
    status       TEXT NOT NULL DEFAULT 'active'
                 CHECK (status IN ('active', 'paused', 'done', 'cleared')),
    turn_count   INT  NOT NULL DEFAULT 0,
    max_turns    INT  NOT NULL DEFAULT 20,
    subgoals     JSONB NOT NULL DEFAULT '[]',
    last_verdict TEXT,
    consecutive_judge_failures INT NOT NULL DEFAULT 0,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
