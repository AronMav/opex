-- migrations/073_agent_model_overrides.sql
-- T15 triage (P2): the `/model` override (set via `POST
-- /api/agents/{name}/model-override`, `agent::providers::ModelOverride`) only
-- lived in an in-memory `RwLock<Option<String>>` on the agent's provider —
-- it was silently lost on every process restart / redeploy.
--
-- Semantic note (deliberate, see T15 proposed_impl): OPEX's override is
-- per-agent (one `ModelOverride` per agent's provider instance, shared across
-- all sessions of that agent), NOT per-session like hermes'. This migration
-- persists that EXISTING per-agent semantic — it does not change granularity.
-- Moving to per-session would be a larger behavioral change and is left as a
-- follow-up (would need a `sessions.model_override` column + bootstrap-time
-- read instead of agent-start-time read).
--
-- One row per agent; NULL/absent = no override (falls back to the agent's
-- configured default model). Deleted (not just nulled) when the override is
-- cleared, so the table only ever holds active overrides.
CREATE TABLE IF NOT EXISTS agent_model_overrides (
    agent_name  TEXT PRIMARY KEY,
    model       TEXT NOT NULL,
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

COMMENT ON TABLE agent_model_overrides IS
  'Persists the per-agent /model runtime override (agent::providers::ModelOverride) across restarts. One row per agent with an active override; row is deleted when the override is cleared. Read at agent-start (start_agent_from_config) and written on POST /api/agents/{name}/model-override.';
