-- File Scenario Engine — bindings table mapping file type -> action.
-- Shaped on provider_active (multi-active by priority). See
-- docs/superpowers/specs/2026-06-22-file-scenario-engine-design.md §4.1.

CREATE TABLE file_scenarios (
    id          UUID PRIMARY KEY,
    match_type  TEXT NOT NULL,                       -- MIME glob ('image/*') or extension ('.mp4')
    executor    TEXT NOT NULL CHECK (executor IN ('tool', 'skill')),
    action_ref  TEXT NOT NULL,                       -- built-in action name (tool) or skill name (skill)
    label       TEXT NOT NULL,
    is_default  BOOLEAN NOT NULL DEFAULT false,
    priority    INT NOT NULL DEFAULT 100,            -- lowest integer wins, ties by created_at then id
    enabled     BOOLEAN NOT NULL DEFAULT true,
    scope       TEXT NOT NULL DEFAULT 'global',      -- reserved; 'global' only in v1
    created_by  TEXT NOT NULL,                       -- system | ui | agent:<name>
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (match_type, action_ref)
);

-- At most one zero-tap default per match_type.
CREATE UNIQUE INDEX file_scenarios_one_default
    ON file_scenarios (match_type) WHERE is_default;

-- Enabled-binding lookup by sniffed type, ordered by priority.
CREATE INDEX file_scenarios_lookup_idx
    ON file_scenarios (match_type, enabled, priority);

COMMENT ON TABLE file_scenarios IS 'FSE bindings: file type -> action. executor=tool binds a built-in deterministic action (allowlisted); executor=skill binds an LLM-mediated recipe (never a 0-click default).';
COMMENT ON COLUMN file_scenarios.priority IS 'Lowest integer wins; ties broken by created_at then id (cf. provider_active).';
COMMENT ON INDEX file_scenarios_one_default IS 'One is_default=true row per match_type. Write paths must clear the prior default before setting a new one.';
