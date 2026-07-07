-- Per-agent operator-configurable handler settings ("valves", OpenWebUI-style).
-- The configurable FIELDS are declared in each handler's <config> descriptor
-- block (toolgate); the VALUES an operator sets are stored here keyed by
-- (handler_id, agent_name) and injected as ctx.config when the handler runs.
CREATE TABLE IF NOT EXISTS handler_config (
    handler_id    TEXT        NOT NULL,
    agent_name    TEXT        NOT NULL,
    config_values JSONB       NOT NULL DEFAULT '{}'::jsonb,
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (handler_id, agent_name)
);
