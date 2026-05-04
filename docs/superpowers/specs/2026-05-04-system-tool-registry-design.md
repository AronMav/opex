# System Tool Registry — Design Spec

**Date:** 2026-05-04
**Status:** Approved (v2 — issues fixed after spec review)

## Problem

`crates/hydeclaw-core/src/agent/engine_dispatch.rs` contains a 622-line hardcoded
`match` with 28 named arms. Adding a new system tool requires modifying this file.
The `skill_use` arm (42 lines), `dispatch_memory_tool` (~200 lines),
`dispatch_git_tool` (130 lines), `dispatch_skill_tool`, `dispatch_session_tool`,
and `dispatch_process_tool` embed complex logic directly in the dispatch layer,
violating single responsibility.

## Goal

Replace the hardcoded match with a `SystemToolRegistry` — a `HashMap` of trait
objects. Adding a new system tool = new file + one registration line. The dispatch
file shrinks to ~30 lines of pure routing.

---

## Architecture

Three new components, one new directory:

```text
crates/hydeclaw-core/src/agent/
  tool_registry.rs          — ToolDeps, SystemToolHandler, SystemToolRegistry
  tool_handlers/
    mod.rs                  — re-exports + SystemToolRegistry::build()
    workspace.rs            — write, read, list, edit, delete, rename
    memory.rs               — all memory sub-actions
    skills.rs               — skill, skill_use
    agent_tool.rs           — agent, agents_list
    tools_mgmt.rs           — tool_create/list/test/verify/disable/discover
    web.rs                  — web_fetch
    code.rs                 — code_exec
    comms.rs                — message, cron, git, canvas, rich_card, process,
                              browser_action
    secrets.rs              — secret_set
    session.rs              — session
```

`engine_dispatch.rs` is reduced to building `ToolDeps`, calling
`registry.dispatch()`, and falling through to MCP/YAML/external tools.

---

## `ToolDeps<'a>`

Service locator built once before dispatch from `&AgentEngine`. Holds lifetime-bound
references — no cloning, no Arc overhead beyond what already exists.

```rust
pub struct ToolDeps<'a> {
    // Core
    pub workspace_dir:      &'a str,
    pub agent_name:         &'a str,
    pub agent_base:         bool,
    pub db:                 &'a PgPool,
    // HTTP
    pub http_client:        &'a reqwest::Client,
    pub ssrf_client:        &'a reqwest::Client,
    // Services
    pub secrets:            &'a SecretsManager,          // engine.secrets().as_ref()
    pub sandbox:            &'a Option<Arc<CodeSandbox>>,
    pub session_pools:      Option<&'a SessionPoolsMap>, // cfg().session_pools.as_ref()
    pub memory_store:       &'a Arc<dyn MemoryService>,  // cfg().memory_store
    pub agent_map:          Option<&'a AgentMap>,        // cfg().agent_map.as_ref()
    // Comms
    pub ui_event_tx:        Option<&'a broadcast::Sender<String>>,
    // Config values needed by specific handlers
    pub toolgate_url:       &'a str,   // cfg().app_config.toolgate_url
    pub gateway_listen:     &'a str,   // cfg().app_config.gateway.listen
    pub signed_url_ttl_secs: u64,      // cfg().app_config.uploads.signed_url_ttl_secs
    // Pre-computed (avoids async call inside handlers)
    pub available_tools:    &'a HashSet<String>,
}

impl<'a> ToolDeps<'a> {
    pub fn from_engine(engine: &'a AgentEngine, available: &'a HashSet<String>) -> Self {
        let cfg = engine.cfg();
        Self {
            workspace_dir:       &cfg.workspace_dir,
            agent_name:          &cfg.agent.name,
            agent_base:          cfg.agent.base,
            db:                  &cfg.db,
            http_client:         engine.http_client(),
            ssrf_client:         engine.ssrf_http_client(),
            secrets:             engine.secrets().as_ref(),
            sandbox:             engine.sandbox(),
            session_pools:       cfg.session_pools.as_ref(),
            memory_store:        &cfg.memory_store,
            agent_map:           cfg.agent_map.as_ref(),
            ui_event_tx:         engine.state().ui_event_tx.as_ref(),
            toolgate_url:        &cfg.app_config.toolgate_url,
            gateway_listen:      &cfg.app_config.gateway.listen,
            signed_url_ttl_secs: cfg.app_config.uploads.signed_url_ttl_secs,
            available_tools:     available,
        }
    }
}
```

---

## `SystemToolHandler` Trait

```rust
#[async_trait]
pub trait SystemToolHandler: Send + Sync {
    async fn handle(&self, deps: ToolDeps<'_>, args: &serde_json::Value) -> String;
}
```

Returns `String` — the tool result. The handler always knows the answer for its
tool; `Option` lives only at the registry level (tool not found = `None`).

---

## `SystemToolRegistry`

```rust
pub struct SystemToolRegistry {
    handlers: HashMap<&'static str, Arc<dyn SystemToolHandler + Send + Sync>>,
}

impl SystemToolRegistry {
    pub fn build() -> Self;

    pub async fn dispatch(
        &self,
        name: &str,
        deps: &ToolDeps<'_>,
        args: &serde_json::Value,
    ) -> Option<String>;   // None = not a system tool
}
```

`build()` registers all 28 handlers. Built once in `AgentEngine::new()`, stored as
`Arc<SystemToolRegistry>` on the engine struct.

---

## `engine_dispatch.rs` After

`available_tool_names().await` is called once before `ToolDeps` construction to
avoid async inside handlers. The fallback chain mirrors the current code exactly —
MCP tools come after system tools and before YAML/external tools.

```rust
pub(super) async fn execute_tool_call_inner(
    &self,
    name: &str,
    args: &serde_json::Value,
) -> String {
    // Pre-compute available tools once (async, needed by skill_use handler)
    let available = self.available_tool_names().await;
    let deps = ToolDeps::from_engine(self, &available);

    // 1. System tools (registry)
    if let Some(result) = self.tool_registry.dispatch(name, &deps, args).await {
        return result;
    }

    // 2. MCP tools (existing path, unchanged)
    if let Some(result) = self.dispatch_mcp_tool(name, args).await {
        return result;
    }

    // 3. YAML tools (existing path, unchanged)
    if let Some(entry) = crate::tools::find_yaml_tool(&deps.workspace_dir, name) {
        return self.execute_yaml_tool(&entry, args).await;
    }

    // 4. External tool registry (existing: self.cfg().tools.call)
    match self.cfg().tools.call(name, args).await {
        Ok(v) => v.to_string(),
        Err(_) => format!("Unknown tool: {name}"),
    }
}
```

---

## Handler Design

### Simple handlers (1–6 lines)

Each wraps an existing `ph::*` function with zero logic change:

```rust
// workspace.rs
pub struct WorkspaceReadHandler;

#[async_trait]
impl SystemToolHandler for WorkspaceReadHandler {
    async fn handle(&self, deps: ToolDeps<'_>, args: &Value) -> String {
        ph::handle_workspace_read(deps.workspace_dir, deps.agent_name, args).await
    }
}
```

### `SkillUseHandler` (complex)

The 42-line inline `skill_use` arm moves verbatim into `skills.rs`. The three
action branches (`capture`, `load`, default) become explicit `match` arms inside
`handle()`. The `tokio::spawn` for reactivation captures clones from `deps` fields —
no `self` references needed.

```rust
pub struct SkillUseHandler;

#[async_trait]
impl SystemToolHandler for SkillUseHandler {
    async fn handle(&self, deps: ToolDeps<'_>, args: &Value) -> String {
        let action = args["action"].as_str().unwrap_or("list");
        match action {
            "capture" => ph::handle_skill_capture(
                deps.workspace_dir, deps.agent_name, deps.db,
                deps.ui_event_tx, args,
            ).await,
            "load" => {
                if let Some(name) = args["name"].as_str() {
                    let skills = crate::skills::load_skills(deps.workspace_dir).await;
                    if let Some(skill) = skills.iter().find(|s| s.meta.name == name) {
                        if matches!(skill.meta.state, crate::skills::SkillState::Archived) {
                            let (ws, n, db, an, ts) = (
                                deps.workspace_dir.to_string(),
                                name.to_string(),
                                deps.db.clone(),
                                deps.agent_name.to_string(),
                                chrono::Utc::now().to_rfc3339(),
                            );
                            tokio::spawn(async move {
                                crate::skills::reactivate_skill(&ws, &n, &db, &an, &ts).await;
                            });
                            let result = ph::handle_skill_use(
                                deps.workspace_dir, deps.agent_base,
                                deps.available_tools, args,
                            ).await;
                            return format!(
                                "{result}\n\n*(This skill was archived and has been reactivated.)*"
                            );
                        }
                    }
                }
                ph::handle_skill_use(
                    deps.workspace_dir, deps.agent_base, deps.available_tools, args,
                ).await
            }
            _ => ph::handle_skill_use(
                deps.workspace_dir, deps.agent_base, deps.available_tools, args,
            ).await,
        }
    }
}
```

### Methods migrating off `AgentEngine` into handlers

Five dispatch methods currently live on `AgentEngine` and must move:

| Method | Moves to |
| --- | --- |
| `dispatch_memory_tool` | `MemoryToolHandler` in `memory.rs` |
| `dispatch_git_tool` | `GitToolHandler` in `comms.rs` |
| `dispatch_skill_tool` | `SkillHandler` in `skills.rs` |
| `dispatch_session_tool` | `SessionHandler` in `session.rs` |
| `dispatch_process_tool` | `ProcessHandler` in `comms.rs` |

All move verbatim — no logic changes. `self.cfg().*` references replaced with
`deps.*` equivalents. After the migration, these methods are deleted from
`AgentEngine`.

---

## Registration

```rust
// tool_handlers/mod.rs
impl SystemToolRegistry {
    pub fn build() -> Self {
        let mut r = Self { handlers: HashMap::new() };
        r.register("workspace_write",  WorkspaceWriteHandler);
        r.register("workspace_read",   WorkspaceReadHandler);
        r.register("workspace_list",   WorkspaceListHandler);
        r.register("workspace_edit",   WorkspaceEditHandler);
        r.register("workspace_delete", WorkspaceDeleteHandler);
        r.register("workspace_rename", WorkspaceRenameHandler);
        r.register("memory",           MemoryToolHandler);
        r.register("message",          MessageHandler);
        r.register("cron",             CronHandler);
        r.register("agent",            AgentToolHandler);
        r.register("web_fetch",        WebFetchHandler);
        r.register("tool_create",      ToolCreateHandler);
        r.register("tool_list",        ToolListHandler);
        r.register("tool_test",        ToolTestHandler);
        r.register("tool_verify",      ToolVerifyHandler);
        r.register("tool_disable",     ToolDisableHandler);
        r.register("skill",            SkillHandler);
        r.register("skill_use",        SkillUseHandler);
        r.register("tool_discover",    ToolDiscoverHandler);
        r.register("secret_set",       SecretSetHandler);
        r.register("session",          SessionHandler);
        r.register("agents_list",      AgentsListHandler);
        r.register("browser_action",   BrowserActionHandler);
        r.register("code_exec",        CodeExecHandler);
        r.register("git",              GitToolHandler);
        r.register("canvas",           CanvasHandler);
        r.register("rich_card",        RichCardHandler);
        r.register("process",          ProcessHandler);
        r
    }

    fn register(&mut self, name: &'static str, h: impl SystemToolHandler + 'static) {
        self.handlers.insert(name, Arc::new(h));
    }
}
```

---

## Migration Strategy

Single PR — no partial state. The old `execute_tool_call_inner` match is replaced
atomically. Handlers are extracted file-by-file but the switch happens in one commit.

**Order of work:**

1. Create `tool_registry.rs` — `ToolDeps`, `SystemToolHandler` trait, empty
   `SystemToolRegistry` struct. Compiles standalone.
2. Create `tool_handlers/` directory with all 28 handler structs, each wrapping
   the existing `ph::*` call. No logic changes yet.
3. Move `dispatch_memory_tool` body into `MemoryToolHandler::handle()`.
4. Move `dispatch_git_tool` body into `GitToolHandler::handle()`.
5. Move `dispatch_skill_tool` body into `SkillHandler::handle()`.
6. Move `dispatch_session_tool` body into `SessionHandler::handle()`.
7. Move `dispatch_process_tool` body into `ProcessHandler::handle()`.
8. Move inline `skill_use` match logic into `SkillUseHandler::handle()`.
9. Populate `SystemToolRegistry::build()` with all 28 registrations.
10. Replace `execute_tool_call_inner` match with registry + MCP + YAML + external
    fallback chain.
11. Delete `dispatch_memory_tool`, `dispatch_git_tool`, `dispatch_skill_tool`,
    `dispatch_session_tool`, `dispatch_process_tool` from `AgentEngine`.
12. `cargo check` + `cargo test`.

---

## Testing

- Each handler is testable in isolation: construct `ToolDeps` with mocked services,
  call `handler.handle(deps, args).await`, assert the returned `String`.
- No integration test changes — existing `cargo test` exercises the dispatch path
  end-to-end.
- `skill_use` and `memory` handlers warrant dedicated unit tests for their
  sub-action routing (the most complex handlers).

---

## What Does NOT Change

- All `ph::*` function signatures — handlers wrap them, not replace them
- MCP tool dispatch path
- YAML tool dispatch
- External tool registry (`self.cfg().tools`)
- Tool policy / deny-list enforcement (applied before dispatch, unchanged)
- Any agent TOML config

---

## Out of Scope

- Converting `ph::*` functions themselves to handlers (they stay as free functions)
- Dynamic runtime registration of new handlers (build-time only)
- Plugin system / WASM handlers
