use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use async_trait::async_trait;
use serde_json::Value;
use sqlx::PgPool;
use tokio::sync::broadcast;

use crate::agent::agent_config::AgentConfig;
use crate::agent::agent_state::AgentState;
use crate::agent::memory_service::MemoryService;
use crate::agent::session_agent_pool::SessionPoolsMap;
use crate::agent::tool_executor::DefaultToolExecutor;
use crate::containers::sandbox::CodeSandbox;
use crate::gateway::state::AgentMap;
use crate::oauth::OAuthManager;
use crate::secrets::SecretsManager;

/// All services a system tool handler may need.
/// Built once before dispatch from `&AgentEngine`.
pub struct ToolDeps<'a> {
    // Convenience flat fields (most handlers use these directly)
    pub workspace_dir:       &'a str,
    pub agent_name:          &'a str,
    pub agent_base:          bool,
    pub db:                  &'a PgPool,
    pub http_client:         &'a reqwest::Client,
    pub ssrf_client:         &'a reqwest::Client,
    pub secrets:             &'a Arc<SecretsManager>,  // &Arc — needed by tool_test, secret_set; call .as_ref() where &SecretsManager needed
    pub sandbox:             &'a Option<Arc<CodeSandbox>>,
    pub session_pools:       Option<&'a SessionPoolsMap>,
    pub memory_store:        &'a Arc<dyn MemoryService>,
    pub agent_map:           Option<&'a AgentMap>,
    pub ui_event_tx:         Option<&'a broadcast::Sender<String>>,
    // Expanded config values
    pub toolgate_url:        String,    // cfg().app_config.toolgate_url (Option<String>, unwrapped)
    pub gateway_listen:      &'a str,
    pub signed_url_ttl_secs: u64,
    // Pre-computed agent tool timeouts (AgentToolTimeouts is Copy)
    pub agent_tool_timeouts: crate::agent::pipeline::agent_tool::AgentToolTimeouts,
    // Auth
    pub oauth:               &'a Option<Arc<OAuthManager>>,
    // Service bags needed by complex handlers (message/cron use CommandContext)
    pub cfg:                 &'a AgentConfig,
    pub state:               &'a AgentState,
    pub tex:                 &'a DefaultToolExecutor,
    // Pre-computed (avoids async inside handlers)
    pub available_tools:     &'a HashSet<String>,
}

impl<'a> ToolDeps<'a> {
    pub fn from_engine(
        engine: &'a crate::agent::engine::AgentEngine,
        available: &'a HashSet<String>,
    ) -> Self {
        let cfg = engine.cfg();
        Self {
            workspace_dir:       &cfg.workspace_dir,
            agent_name:          &cfg.agent.name,
            agent_base:          cfg.agent.base,
            db:                  &cfg.db,
            http_client:         engine.http_client(),
            ssrf_client:         engine.ssrf_http_client(),
            secrets:             engine.secrets(),
            sandbox:             engine.sandbox(),
            session_pools:       cfg.session_pools.as_ref(),
            memory_store:        &cfg.memory_store,
            agent_map:           cfg.agent_map.as_ref(),
            ui_event_tx:         engine.state().ui_event_tx.as_ref(),
            toolgate_url:        cfg.app_config.toolgate_url.clone()
                                    .unwrap_or_else(|| "http://localhost:9011".to_string()),
            gateway_listen:      &cfg.app_config.gateway.listen,
            signed_url_ttl_secs: cfg.app_config.uploads.signed_url_ttl_secs,
            agent_tool_timeouts: crate::agent::pipeline::agent_tool::AgentToolTimeouts::from(
                &cfg.app_config.agent_tool,
            ),
            oauth:               engine.oauth(),
            cfg,
            state:               engine.state(),
            tex:                 engine.tex(),
            available_tools:     available,
        }
    }
}

/// A system tool handler. One struct per tool name.
#[async_trait]
pub trait SystemToolHandler: Send + Sync {
    async fn handle(&self, deps: ToolDeps<'_>, args: &Value) -> String;
}

pub struct SystemToolRegistry {
    handlers: HashMap<&'static str, Arc<dyn SystemToolHandler + Send + Sync>>,
}

impl SystemToolRegistry {
    pub fn new() -> Self {
        Self { handlers: HashMap::new() }
    }

    pub fn register(&mut self, name: &'static str, h: impl SystemToolHandler + 'static) {
        self.handlers.insert(name, Arc::new(h));
    }

    /// Returns `Some(result)` if `name` is registered, `None` to fall through.
    pub async fn dispatch(
        &self,
        name: &str,
        deps: &ToolDeps<'_>,
        args: &Value,
    ) -> Option<String> {
        let handler = self.handlers.get(name)?;
        Some(handler.handle(ToolDeps {
            workspace_dir:       deps.workspace_dir,
            agent_name:          deps.agent_name,
            agent_base:          deps.agent_base,
            db:                  deps.db,
            http_client:         deps.http_client,
            ssrf_client:         deps.ssrf_client,
            secrets:             deps.secrets,
            sandbox:             deps.sandbox,
            session_pools:       deps.session_pools,
            memory_store:        deps.memory_store,
            agent_map:           deps.agent_map,
            ui_event_tx:         deps.ui_event_tx,
            toolgate_url:        deps.toolgate_url.clone(),
            gateway_listen:      deps.gateway_listen,
            signed_url_ttl_secs: deps.signed_url_ttl_secs,
            agent_tool_timeouts: deps.agent_tool_timeouts,
            oauth:               deps.oauth,
            cfg:                 deps.cfg,
            state:               deps.state,
            tex:                 deps.tex,
            available_tools:     deps.available_tools,
        }, args).await)
    }
}
