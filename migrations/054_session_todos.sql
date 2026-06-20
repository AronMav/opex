CREATE TABLE session_todos (
    session_id  UUID    NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    item_id     TEXT    NOT NULL,
    content     TEXT    NOT NULL,
    status      TEXT    NOT NULL DEFAULT 'pending'
                CHECK (status IN ('pending', 'in_progress', 'done', 'cancelled')),
    position    INT     NOT NULL DEFAULT 0,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (session_id, item_id)
);

CREATE INDEX idx_session_todos_session ON session_todos(session_id, position);
