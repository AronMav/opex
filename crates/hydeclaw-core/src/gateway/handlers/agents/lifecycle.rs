use std::sync::Arc;

use crate::agent::handle::AgentHandle;
use crate::channels::access::AccessGuard;
use crate::config::AgentConfig;
use crate::gateway::clusters::{AgentCore, AuthServices, ChannelBus, ConfigServices, InfraServices, StatusMonitor};

// ── Agent lifecycle ─────────────────────────────────────

/// Start an agent from config: create engine, channel adapter, scheduler jobs.
/// Returns the `AgentHandle` and optional `AccessGuard`.
pub async fn start_agent_from_config(
    agent_cfg: &AgentConfig,
    agents: &AgentCore,
    infra: &InfraServices,
    auth: &AuthServices,
    bus: &ChannelBus,
    cfg: &ConfigServices,
    status: &StatusMonitor,
) -> anyhow::Result<(AgentHandle, Option<Arc<AccessGuard>>)> {
    use crate::agent::{engine::AgentEngine, providers};
    use crate::channels;

    let deps = agents.deps.read().await;
    let name = &agent_cfg.agent.name;

    // Apply [agent.defaults] fallback: use global temperature/max_tokens when agent doesn't override.
    let global_defaults = &cfg.config.agent.defaults;
    let effective_temperature = global_defaults.temperature.unwrap_or(agent_cfg.agent.temperature);
    let effective_max_tokens = agent_cfg.agent.max_tokens.or(global_defaults.max_tokens);

    // Use routing provider if routing rules are configured, otherwise resolve provider
    // (named connection → legacy provider_type fallback).
    let provider = if agent_cfg.agent.routing.is_empty() {
        providers::resolve_provider_for_agent(
            &infra.db,
            &agent_cfg.agent,
            effective_temperature,
            effective_max_tokens,
            auth.secrets.clone(),
            deps.sandbox.clone(),
            name,
            &deps.workspace_dir,
            agent_cfg.agent.base,
        ).await
    } else {
        tracing::info!(
            agent = %name,
            routes = agent_cfg.agent.routing.len(),
            "using multi-provider routing"
        );
        providers::create_routing_provider(
            &infra.db,
            &agent_cfg.agent.routing,
            effective_temperature,
            effective_max_tokens,
            agent_cfg.agent.max_failover_attempts,
            auth.secrets.clone(),
        ).await
    };

    let channel_router = crate::agent::channel_actions::ChannelActionRouter::new();

    let default_timezone = crate::agent::workspace::parse_user_timezone(&deps.workspace_dir).await;

    // Load dedicated compaction provider from provider_active (optional — falls back to primary).
    let compaction_provider: Option<Arc<dyn crate::agent::providers::LlmProvider>> = {
        match crate::db::providers::get_provider_active(&infra.db, crate::db::providers::CAPABILITY_COMPACTION).await {
            Ok(Some(provider_name)) => {
                match crate::db::providers::get_provider_by_name(&infra.db, &provider_name).await {
                    Ok(Some(provider_row)) => {
                        use crate::agent::providers::{build_provider, build_cli_provider, CliContext, timeouts::ProviderOptions};
                        let opts: ProviderOptions =
                            serde_json::from_value(provider_row.options.clone()).unwrap_or_default();
                        let timeouts_cfg = opts.timeouts;
                        let cancel = tokio_util::sync::CancellationToken::new();
                        let built: Option<Box<dyn crate::agent::providers::LlmProvider>> =
                            match provider_row.provider_type.as_str() {
                                "claude-cli" | "gemini-cli" | "codex-cli" => {
                                    let ctx = CliContext {
                                        sandbox: deps.sandbox.clone(),
                                        agent_name: name,
                                        workspace_dir: &deps.workspace_dir,
                                        base: agent_cfg.agent.base,
                                        secrets: auth.secrets.clone(),
                                    };
                                    build_cli_provider(&provider_row, None, ctx).await.ok()
                                }
                                _ => build_provider(
                                    &provider_row,
                                    auth.secrets.clone(),
                                    &timeouts_cfg,
                                    cancel,
                                    crate::agent::providers::ProviderOverrides::default(),
                                ).ok(),
                            };
                        match built {
                            Some(p) => {
                                tracing::info!(agent = %name, provider = %provider_name, "using dedicated compaction provider");
                                Some(Arc::from(p))
                            }
                            None => None,
                        }
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    };

    // Build the hooks registry (goes into DefaultToolExecutor, Phase 39-02)
    let hooks_registry = {
        let mut registry = crate::agent::hooks::HookRegistry::new();
        if let Some(ref hc) = agent_cfg.agent.hooks {
            if hc.log_all_tool_calls {
                registry.register("log_tool_calls".into(), crate::agent::hooks::logging_hook());
            }
            if !hc.block_tools.is_empty() {
                registry.register("block_tools".into(), crate::agent::hooks::block_tools_hook(hc.block_tools.clone()));
            }
        }
        Arc::new(registry)
    };

    // Shared approval waiters map — used by both ApprovalManager and DefaultToolExecutor.
    // Phase 66 REF-02: `DashMap` (sharded sync locks) replaces `RwLock<HashMap>` so
    // callers never hold a write guard across `.await`.
    let approval_waiters: crate::agent::approval_manager::ApprovalWaitersMap =
        Arc::new(dashmap::DashMap::new());

    let approval_manager = Arc::new(crate::agent::approval_manager::ApprovalManager::new(
        infra.db.clone(),
        approval_waiters.clone(),
    ));

    let agent_state = Arc::new(crate::agent::agent_state::AgentState::new(
        Some(status.processing_tracker.clone()),
        Some(channel_router.clone()),
        Some(bus.ui_event_tx.clone()),
        bus.bg_tasks.clone(),
    ));

    // Build the immutable AgentConfig snapshot (Step A of thin-wrapper conversion).
    let agent_config = Arc::new(crate::agent::agent_config::AgentConfig {
        agent: agent_cfg.agent.clone(),
        workspace_dir: deps.workspace_dir.clone(),
        default_timezone: default_timezone.clone(),
        app_config: std::sync::Arc::new(cfg.config.clone()),
        provider: provider.clone(),
        compaction_provider: compaction_provider.clone(),
        db: infra.db.clone(),
        memory_store: infra.memory_store.clone() as Arc<dyn crate::agent::memory_service::MemoryService>,
        embedder: infra.embedder.clone(),
        tools: agents.tools.clone(),
        approval_manager: approval_manager.clone(),
        scheduler: Some(agents.scheduler.clone()),
        agent_map: Some(agents.map.clone()),
        session_pools: Some(agents.session_pools.clone()),
        audit_queue: deps.audit_queue.clone(),
        metrics: infra.metrics.clone(),
    });

    let engine = Arc::new(AgentEngine {
        context_builder: std::sync::OnceLock::new(),
        tool_executor: std::sync::OnceLock::new(),
        state: agent_state,
        cfg: Some(agent_config),
    });
    engine.set_context_builder(&engine);
    engine.state().set_self_ref(&engine);

    // Build DefaultToolExecutor with its own fields (Phase 39-02: TOOL-04).
    // These 20 fields are owned by the executor; engine accesses them via proxy methods (engine.tex()).
    {
        use crate::agent::tool_executor::{DefaultToolExecutor, DefaultToolExecutorFields, ToolExecutorDeps};

        let deps_strong = engine.clone() as Arc<dyn ToolExecutorDeps>;
        let deps_weak = Arc::downgrade(&deps_strong);
        let executor = Arc::new(DefaultToolExecutor::new(
            deps_weak,
            DefaultToolExecutorFields {
                // Privileged agents run code directly on host (no Docker sandbox)
                sandbox: if agent_cfg.agent.base { None } else { deps.sandbox.clone() },
                bg_processes: Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
                yaml_tools_cache: tokio::sync::RwLock::new((std::time::Instant::now(), std::sync::Arc::new(std::collections::HashMap::new()))),
                skills_cache: tokio::sync::RwLock::new((std::time::Instant::now(), vec![])),
                search_cache: tokio::sync::RwLock::new(std::collections::HashMap::new()),
                tool_embed_cache: deps.tool_embed_cache.clone(),
                penalty_cache: deps.penalty_cache.clone(),
                pinned_chunk_ids: tokio::sync::Mutex::new(vec![]),
                memory_md_lock: tokio::sync::Mutex::new(()),
                canvas_state: tokio::sync::RwLock::new(None),
                ssrf_http_client: crate::net::ssrf::ssrf_http_client(
                    std::time::Duration::from_secs(30),
                ),
                oauth: Some(auth.oauth.clone()),
                subagent_registry: crate::agent::subagent_state::SubagentRegistry::new(),
                // Shared fields (Phase 39-02 wave 2)
                secrets: auth.secrets.clone(),
                mcp: deps.mcp.clone(),
                http_client: reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(120))
                    .build()
                    .unwrap_or_default(),
                hooks: hooks_registry,
                approval_waiters: approval_waiters.clone(),
                sse_event_tx: Arc::new(tokio::sync::Mutex::new(None)),
            },
        ));
        engine.set_tool_executor(executor);
    }
    let workspace_dir = deps.workspace_dir.clone();
    drop(deps); // Release read lock before async operations

    // Ensure workspace directory + scaffold files exist
    if let Err(e) = crate::agent::workspace::ensure_workspace_scaffold(
        &workspace_dir,
        name,
        agent_cfg.agent.base,
    ).await {
        tracing::warn!(agent = %name, error = %e, "failed to scaffold workspace");
    }

    // Schedule heartbeat
    let mut scheduler_job_ids = Vec::new();
    if let Ok(Some(uuid)) = agents.scheduler.add_heartbeat(agent_cfg, engine.clone()).await {
        scheduler_job_ids.push(uuid);
    }

    // Set up access guard if access config is present.
    // Channel adapter connects externally via /ws/channel/{agent}.
    let mut access_guard = None;

    if let Some(ref ac) = agent_cfg.agent.access {
        let restricted = ac.mode == "restricted";
        let guard = Arc::new(channels::access::AccessGuard::new(
            name.clone(),
            ac.owner_id.clone(),
            restricted,
            infra.db.clone(),
        ));
        access_guard = Some(guard.clone());
        tracing::info!(agent = %name, mode = %ac.mode, "access guard configured (adapter via /ws/channel)");
    }

    let agent_handle = AgentHandle {
        engine,
        scheduler_job_ids,
        channel_router: Some(channel_router),
    };

    Ok((agent_handle, access_guard))
}
