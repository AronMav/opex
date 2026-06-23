-- Opex — consolidated database schema
-- Clean install: creates all tables, indexes, and triggers from scratch.

-- ── Extensions ──────────────────────────────────────────────
CREATE EXTENSION IF NOT EXISTS vector;
CREATE EXTENSION IF NOT EXISTS pg_trgm;

-- ── Sessions & Messages ────────────────────────────────────
CREATE TABLE sessions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    agent_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    channel TEXT NOT NULL,
    title TEXT,
    run_status TEXT,
    activity_at TIMESTAMPTZ,
    started_at TIMESTAMPTZ DEFAULT now(),
    last_message_at TIMESTAMPTZ DEFAULT now(),
    metadata JSONB
);

CREATE TABLE messages (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    session_id UUID REFERENCES sessions(id) ON DELETE CASCADE,
    agent_id TEXT,
    role TEXT NOT NULL,
    content TEXT NOT NULL,
    tool_calls JSONB,
    tool_call_id TEXT,
    tokens_input INT,
    tokens_output INT,
    status TEXT NOT NULL DEFAULT 'complete',
    feedback SMALLINT DEFAULT 0,
    edited_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ DEFAULT now(),
    tsv tsvector GENERATED ALWAYS AS (to_tsvector('simple', COALESCE(content, ''))) STORED
);

CREATE OR REPLACE FUNCTION update_session_last_message()
RETURNS TRIGGER AS $$
BEGIN
    UPDATE sessions SET last_message_at = NEW.created_at WHERE id = NEW.session_id;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER trg_update_session_last_message
    AFTER INSERT ON messages
    FOR EACH ROW EXECUTE FUNCTION update_session_last_message();

-- ── Tasks ───────────────────────────────────────────────────
CREATE TABLE tasks (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    agent_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    source TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    input TEXT NOT NULL,
    plan JSONB,
    result TEXT,
    error TEXT,
    created_at TIMESTAMPTZ DEFAULT now(),
    updated_at TIMESTAMPTZ DEFAULT now()
);

CREATE TABLE task_steps (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    task_id UUID REFERENCES tasks(id) ON DELETE CASCADE,
    step_order INT NOT NULL,
    mcp_name TEXT NOT NULL,
    action TEXT NOT NULL,
    params JSONB,
    status TEXT NOT NULL DEFAULT 'pending',
    tool_calls JSONB,
    output JSONB,
    started_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ
);

-- ── Memory ──────────────────────────────────────────────────
CREATE TABLE memory_chunks (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    agent_id TEXT NOT NULL DEFAULT '',
    user_id TEXT NOT NULL,
    pinned BOOLEAN NOT NULL DEFAULT false,
    content TEXT NOT NULL,
    embedding vector,
    source TEXT,
    relevance_score FLOAT DEFAULT 1.0,
    tsv tsvector,
    parent_id UUID REFERENCES memory_chunks(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED,
    chunk_index INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ DEFAULT now(),
    accessed_at TIMESTAMPTZ DEFAULT now()
);

-- ── Knowledge Graph ─────────────────────────────────────────
CREATE TABLE graph_entities (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name TEXT NOT NULL,
    name_normalized TEXT NOT NULL,
    entity_type TEXT NOT NULL DEFAULT 'Concept',
    summary TEXT,
    group_id TEXT DEFAULT '',
    created_at TIMESTAMPTZ DEFAULT now(),
    updated_at TIMESTAMPTZ DEFAULT now()
);

CREATE TABLE graph_edges (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    source_id UUID NOT NULL REFERENCES graph_entities(id) ON DELETE CASCADE,
    target_id UUID NOT NULL REFERENCES graph_entities(id) ON DELETE CASCADE,
    relation_type TEXT NOT NULL,
    fact TEXT,
    weight FLOAT DEFAULT 1.0,
    valid_at TEXT,
    invalid_at TEXT,
    created_at TIMESTAMPTZ DEFAULT now(),
    updated_at TIMESTAMPTZ DEFAULT now(),
    UNIQUE (source_id, target_id, relation_type)
);

CREATE TABLE graph_episodes (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    session_id UUID,
    chunk_id UUID REFERENCES memory_chunks(id) ON DELETE CASCADE,
    entity_id UUID NOT NULL REFERENCES graph_entities(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ DEFAULT now()
);

CREATE TABLE graph_extraction_queue (
    chunk_id UUID PRIMARY KEY REFERENCES memory_chunks(id) ON DELETE CASCADE,
    status TEXT NOT NULL DEFAULT 'pending',
    attempts INTEGER DEFAULT 0,
    last_error TEXT,
    created_at TIMESTAMPTZ DEFAULT now(),
    processed_at TIMESTAMPTZ
);

-- ── Memory Worker Tasks ─────────────────────────────────────
CREATE TABLE memory_tasks (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    task_type TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    params JSONB DEFAULT '{}',
    result JSONB,
    error TEXT,
    created_at TIMESTAMPTZ DEFAULT now(),
    started_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ
);

-- ── Scheduling ──────────────────────────────────────────────
CREATE TABLE scheduled_jobs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    agent_id TEXT NOT NULL,
    name TEXT NOT NULL UNIQUE,
    cron_expr TEXT NOT NULL,
    timezone TEXT NOT NULL DEFAULT 'Europe/Samara',
    task_message TEXT NOT NULL,
    enabled BOOLEAN NOT NULL DEFAULT true,
    silent BOOLEAN NOT NULL DEFAULT false,
    announce_to JSONB,
    jitter_secs INTEGER NOT NULL DEFAULT 0,
    run_once BOOLEAN NOT NULL DEFAULT false,
    run_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_run_at TIMESTAMPTZ
);

CREATE TABLE cron_runs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    job_id UUID NOT NULL REFERENCES scheduled_jobs(id) ON DELETE CASCADE,
    agent_id TEXT NOT NULL,
    started_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    finished_at TIMESTAMPTZ,
    status TEXT NOT NULL DEFAULT 'running',
    error TEXT,
    response_preview TEXT
);

-- ── Secrets ─────────────────────────────────────────────────
CREATE TABLE secrets (
    name TEXT NOT NULL,
    scope TEXT NOT NULL DEFAULT '',
    encrypted_value BYTEA NOT NULL,
    nonce BYTEA NOT NULL,
    description TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (name, scope)
);

-- ── Channels ────────────────────────────────────────────────
CREATE TABLE agent_channels (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    agent_name TEXT NOT NULL,
    channel_type TEXT NOT NULL,
    display_name TEXT NOT NULL,
    config JSONB NOT NULL DEFAULT '{}',
    container_name TEXT,
    status TEXT NOT NULL DEFAULT 'stopped',
    error_msg TEXT,
    created_at TIMESTAMPTZ DEFAULT now()
);

CREATE TABLE channel_allowed_users (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    agent_id TEXT NOT NULL,
    channel_user_id TEXT NOT NULL,
    display_name TEXT,
    approved_by TEXT,
    approved_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE(agent_id, channel_user_id)
);

CREATE TABLE pending_messages (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    agent_id TEXT NOT NULL,
    request_id TEXT NOT NULL,
    channel TEXT NOT NULL,
    message_type TEXT NOT NULL CHECK (message_type IN ('done', 'error')),
    text TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE outbound_queue (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    agent_id TEXT NOT NULL,
    channel TEXT NOT NULL,
    action_name TEXT NOT NULL,
    payload JSONB NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    attempts INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    sent_at TIMESTAMPTZ,
    acked_at TIMESTAMPTZ
);

-- ── Usage & Approvals ───────────────────────────────────────
CREATE TABLE usage_log (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    agent_id TEXT NOT NULL,
    provider TEXT NOT NULL,
    model TEXT NOT NULL DEFAULT '',
    input_tokens INTEGER NOT NULL DEFAULT 0,
    output_tokens INTEGER NOT NULL DEFAULT 0,
    session_id UUID REFERENCES sessions(id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE pending_approvals (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    agent_id TEXT NOT NULL,
    session_id UUID REFERENCES sessions(id) ON DELETE CASCADE,
    tool_name TEXT NOT NULL,
    tool_args JSONB NOT NULL DEFAULT '{}',
    status TEXT NOT NULL DEFAULT 'pending',
    requested_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    resolved_at TIMESTAMPTZ,
    resolved_by TEXT,
    context JSONB DEFAULT '{}'
);

CREATE TABLE approval_allowlist (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    agent_id TEXT NOT NULL,
    tool_pattern TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_by TEXT
);

-- ── Audit ───────────────────────────────────────────────────
CREATE TABLE audit_events (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    agent_id TEXT NOT NULL,
    event_type TEXT NOT NULL,
    actor TEXT,
    details JSONB NOT NULL DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE audit_log (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    agent_id TEXT NOT NULL,
    session_id UUID,
    tool_name TEXT NOT NULL,
    parameters JSONB,
    status TEXT NOT NULL,
    duration_ms INTEGER,
    error TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ── Streaming ───────────────────────────────────────────────
CREATE TABLE stream_jobs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    session_id UUID NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    agent_id TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'running',
    aggregated_text TEXT NOT NULL DEFAULT '',
    tool_calls JSONB NOT NULL DEFAULT '[]'::jsonb,
    error_text TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    finished_at TIMESTAMPTZ
);

-- ── Providers ───────────────────────────────────────────────
CREATE TABLE providers (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name TEXT NOT NULL UNIQUE,
    type TEXT NOT NULL,
    provider_type TEXT NOT NULL,
    base_url TEXT,
    default_model TEXT,
    enabled BOOLEAN NOT NULL DEFAULT true,
    options JSONB NOT NULL DEFAULT '{}',
    notes TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE provider_active (
    capability TEXT PRIMARY KEY,
    provider_name TEXT REFERENCES providers(name) ON DELETE SET NULL ON UPDATE CASCADE
);

-- ── Webhooks ────────────────────────────────────────────────
CREATE TABLE webhooks (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name TEXT NOT NULL UNIQUE,
    agent_id TEXT NOT NULL,
    secret TEXT,
    prompt_prefix TEXT,
    webhook_type TEXT NOT NULL DEFAULT 'generic',
    event_filter TEXT[],
    enabled BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_triggered_at TIMESTAMPTZ,
    trigger_count INTEGER NOT NULL DEFAULT 0
);

-- ── OAuth ───────────────────────────────────────────────────
CREATE TABLE oauth_accounts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    provider TEXT NOT NULL,
    display_name TEXT NOT NULL DEFAULT '',
    user_email TEXT,
    scope TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'disconnected',
    expires_at TIMESTAMPTZ,
    connected_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE agent_oauth_bindings (
    agent_id TEXT NOT NULL,
    provider TEXT NOT NULL,
    account_id UUID NOT NULL REFERENCES oauth_accounts(id) ON DELETE CASCADE,
    bound_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (agent_id, provider)
);

CREATE TABLE gmail_triggers (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    agent_id TEXT NOT NULL,
    email_address TEXT NOT NULL,
    history_id TEXT,
    watch_expiry TIMESTAMPTZ,
    pubsub_topic TEXT NOT NULL,
    enabled BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (agent_id, email_address)
);

-- ── Documents ───────────────────────────────────────────────
CREATE TABLE session_documents (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    session_id UUID NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    filename TEXT NOT NULL,
    content TEXT NOT NULL,
    chunk_index INTEGER NOT NULL DEFAULT 0,
    embedding halfvec(2560),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ── Watchdog ────────────────────────────────────────────────
CREATE TABLE watchdog_settings (
    key TEXT PRIMARY KEY,
    value JSONB NOT NULL,
    updated_at TIMESTAMPTZ DEFAULT now()
);

INSERT INTO watchdog_settings (key, value) VALUES
    ('alert_channel_ids', '[]'::jsonb),
    ('alert_events', '["down","restart","recovery","resource"]'::jsonb)
ON CONFLICT (key) DO NOTHING;

-- ── GitHub ──────────────────────────────────────────────────
CREATE TABLE agent_github_repos (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    agent_id TEXT NOT NULL,
    owner TEXT NOT NULL,
    repo TEXT NOT NULL,
    added_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (agent_id, owner, repo)
);

-- ── Tool & Skill Quality ───────────────────────────────────
CREATE TABLE tool_quality (
    tool_name TEXT PRIMARY KEY,
    total_calls INTEGER NOT NULL DEFAULT 0,
    success_calls INTEGER NOT NULL DEFAULT 0,
    fail_calls INTEGER NOT NULL DEFAULT 0,
    total_latency_ms BIGINT NOT NULL DEFAULT 0,
    recent_calls JSONB NOT NULL DEFAULT '[]',
    penalty_score REAL NOT NULL DEFAULT 1.0,
    last_error TEXT,
    last_call_at TIMESTAMPTZ,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE skill_metrics (
    skill_name TEXT PRIMARY KEY,
    times_selected INTEGER NOT NULL DEFAULT 0,
    times_applied INTEGER NOT NULL DEFAULT 0,
    times_success INTEGER NOT NULL DEFAULT 0,
    times_fail INTEGER NOT NULL DEFAULT 0,
    avg_token_usage INTEGER NOT NULL DEFAULT 0,
    effectiveness_score REAL NOT NULL DEFAULT 0.5,
    last_selected_at TIMESTAMPTZ,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE skill_versions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    skill_name TEXT NOT NULL,
    generation INTEGER NOT NULL DEFAULT 0,
    parent_id UUID REFERENCES skill_versions(id),
    evolution_type TEXT NOT NULL DEFAULT 'manual',
    content TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    quality_score REAL,
    trigger_reason TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ── Indexes ─────────────────────────────────────────────────

-- Sessions
CREATE INDEX idx_sessions_user ON sessions(user_id, last_message_at DESC);
CREATE INDEX idx_sessions_agent ON sessions(agent_id, last_message_at DESC);
CREATE INDEX idx_sessions_channel ON sessions(channel, last_message_at DESC);
CREATE INDEX idx_sessions_running ON sessions(run_status) WHERE run_status = 'running';

-- Messages
CREATE INDEX idx_messages_session ON messages(session_id, created_at DESC);
CREATE INDEX idx_messages_tsv ON messages USING gin(tsv);
CREATE INDEX idx_messages_role ON messages(session_id, role);
CREATE INDEX idx_messages_streaming ON messages(status) WHERE status = 'streaming';
CREATE INDEX idx_messages_tool_call ON messages(session_id, tool_call_id) WHERE tool_call_id IS NOT NULL;

-- Tasks
CREATE INDEX idx_tasks_status ON tasks(status, created_at);
CREATE INDEX idx_tasks_user ON tasks(user_id, created_at DESC);
CREATE INDEX idx_task_steps_task ON task_steps(task_id, step_order);

-- Memory
CREATE INDEX idx_memory_user ON memory_chunks(user_id, pinned);
CREATE INDEX idx_memory_fts ON memory_chunks USING gin(tsv);
CREATE INDEX idx_memory_source ON memory_chunks(source, created_at DESC);
CREATE INDEX idx_memory_agent ON memory_chunks(agent_id);
CREATE INDEX idx_memory_parent ON memory_chunks(parent_id) WHERE parent_id IS NOT NULL;

-- Graph
CREATE UNIQUE INDEX idx_graph_entities_name_type ON graph_entities(name_normalized, entity_type);
CREATE INDEX idx_graph_entities_trgm ON graph_entities USING gin(name_normalized gin_trgm_ops);
CREATE INDEX idx_graph_edges_source ON graph_edges(source_id);
CREATE INDEX idx_graph_edges_target ON graph_edges(target_id);
CREATE UNIQUE INDEX idx_graph_episodes_chunk ON graph_episodes(chunk_id, entity_id) WHERE chunk_id IS NOT NULL;
CREATE UNIQUE INDEX idx_graph_episodes_session ON graph_episodes(session_id, entity_id) WHERE session_id IS NOT NULL;
CREATE INDEX idx_graph_episodes_entity ON graph_episodes(entity_id);
CREATE INDEX idx_extraction_pending ON graph_extraction_queue(status, created_at)
    WHERE status = 'pending' OR (status = 'failed' AND attempts < 3);

-- Memory tasks
CREATE INDEX idx_memory_tasks_pending ON memory_tasks(status, created_at)
    WHERE status = 'pending' OR status = 'processing';

-- Scheduling
CREATE INDEX idx_scheduled_jobs_agent ON scheduled_jobs(agent_id);
CREATE INDEX idx_cron_runs_job ON cron_runs(job_id, started_at DESC);
CREATE INDEX idx_cron_runs_started ON cron_runs(started_at DESC);

-- Channels
CREATE INDEX idx_channels_agent ON agent_channels(agent_name);
CREATE INDEX idx_channel_users_agent ON channel_allowed_users(agent_id);
CREATE INDEX idx_pending_msg_agent ON pending_messages(agent_id);
CREATE INDEX idx_outbound_status ON outbound_queue(status, created_at)
    WHERE status IN ('pending', 'sent');
CREATE INDEX idx_outbound_dedup ON outbound_queue(agent_id, channel, action_name, created_at DESC);

-- Usage & approvals
CREATE INDEX idx_usage_agent ON usage_log(agent_id, created_at);
CREATE INDEX idx_usage_created ON usage_log(created_at);
CREATE INDEX idx_usage_session ON usage_log(session_id);
CREATE INDEX idx_approvals_pending ON pending_approvals(agent_id, status)
    WHERE status = 'pending';
CREATE INDEX idx_allowlist_agent ON approval_allowlist(agent_id);

-- Audit
CREATE INDEX idx_audit_agent_type ON audit_events(agent_id, event_type, created_at);
CREATE INDEX idx_audit_created ON audit_events(created_at DESC);
CREATE INDEX idx_audit_log_agent ON audit_log(agent_id, created_at DESC);
CREATE INDEX idx_audit_log_tool ON audit_log(tool_name, created_at DESC);

-- Streaming
CREATE INDEX idx_stream_session ON stream_jobs(session_id);
CREATE INDEX idx_stream_running ON stream_jobs(status) WHERE status = 'running';

-- OAuth
CREATE INDEX idx_oauth_provider ON oauth_accounts(provider);
CREATE INDEX idx_oauth_bindings_account ON agent_oauth_bindings(account_id);

-- Documents
CREATE INDEX idx_session_docs ON session_documents(session_id);
CREATE INDEX idx_session_docs_embedding ON session_documents USING hnsw(embedding halfvec_cosine_ops);

-- GitHub
CREATE INDEX idx_github_repos_agent ON agent_github_repos(agent_id);

-- Skill versions
CREATE INDEX idx_skill_versions_name ON skill_versions(skill_name, generation DESC);
CREATE INDEX idx_skill_versions_parent ON skill_versions(parent_id);
