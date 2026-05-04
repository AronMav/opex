# System Tool Registry Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the 622-line hardcoded match in `engine_dispatch.rs` with a `SystemToolRegistry` — a HashMap of trait objects — so adding a new system tool requires only a new file and one registration line.

**Architecture:** New `tool_registry.rs` defines `ToolDeps<'a>` (service-locator struct), `SystemToolHandler` trait, and `SystemToolRegistry`. New `tool_handlers/` directory contains one file per domain (workspace, memory, skills, comms, etc.), each with `struct FooHandler` implementing the trait. `engine_dispatch.rs` shrinks to ~30 lines: build deps, call registry, fall through to MCP/YAML/external.

**Tech Stack:** Rust/async-trait (already in Cargo.toml), sqlx PgPool, tokio, existing `ph::*` functions in `pipeline/handlers.rs`.

---

## File Map

| Action | Path | Responsibility |
| --- | --- | --- |
| Create | `crates/hydeclaw-core/src/agent/tool_registry.rs` | `ToolDeps`, `SystemToolHandler` trait, `SystemToolRegistry` |
| Create | `crates/hydeclaw-core/src/agent/tool_handlers/mod.rs` | Re-exports + `SystemToolRegistry::build()` |
| Create | `crates/hydeclaw-core/src/agent/tool_handlers/workspace.rs` | 6 workspace handlers |
| Create | `crates/hydeclaw-core/src/agent/tool_handlers/memory.rs` | `MemoryToolHandler` (was `dispatch_memory_tool`) |
| Create | `crates/hydeclaw-core/src/agent/tool_handlers/skills.rs` | `SkillHandler` + `SkillUseHandler` |
| Create | `crates/hydeclaw-core/src/agent/tool_handlers/agent_tool.rs` | `AgentToolHandler` + `AgentsListHandler` |
| Create | `crates/hydeclaw-core/src/agent/tool_handlers/tools_mgmt.rs` | 6 tool management handlers |
| Create | `crates/hydeclaw-core/src/agent/tool_handlers/web.rs` | `WebFetchHandler` |
| Create | `crates/hydeclaw-core/src/agent/tool_handlers/code.rs` | `CodeExecHandler` |
| Create | `crates/hydeclaw-core/src/agent/tool_handlers/comms.rs` | `MessageHandler`, `CronHandler`, `GitToolHandler`, `CanvasHandler`, `RichCardHandler`, `BrowserActionHandler`, `ProcessHandler` |
| Create | `crates/hydeclaw-core/src/agent/tool_handlers/secrets.rs` | `SecretSetHandler` |
| Create | `crates/hydeclaw-core/src/agent/tool_handlers/session.rs` | `SessionHandler` (was `dispatch_session_tool`) |
| Modify | `crates/hydeclaw-core/src/agent/mod.rs` | Add `pub mod tool_registry; pub mod tool_handlers;` |
| Modify | `crates/hydeclaw-core/src/agent/engine_dispatch.rs` | Replace match with registry dispatch; delete 5 `dispatch_*` methods |
| Modify | `crates/hydeclaw-core/src/agent/engine/mod.rs` | Add `tool_registry` field to `AgentEngine`; build in `new()` |

---

## Task 1 — `tool_registry.rs`: ToolDeps, trait, registry skeleton

**Files:**
- Create: `crates/hydeclaw-core/src/agent/tool_registry.rs`
- Modify: `crates/hydeclaw-core/src/agent/mod.rs`

### Context

`engine_dispatch.rs` uses these engine accessors across its 28 arms:
- `self.cfg().*`: `workspace_dir`, `agent.name/base`, `db`, `memory_store`, `agent_map`, `session_pools`, `app_config.*`
- Engine methods: `http_client()`, `ssrf_http_client()`, `secrets()` → `&Arc<SecretsManager>` (use `.as_ref()` for `&SecretsManager`)
- `self.sandbox()`, `self.oauth()`
- `self.state()` — needed by `message`, `cron`, `canvas`, `session`
- `self.tex()` — needed by `memory` (pinned_chunk_ids, memory_md_lock), `canvas` (canvas_state), `process` (bg_processes)
- `self.cfg()` — needed by `message`, `cron` (to build `CommandContext`)
- `self.available_tool_names().await` — needed by `skill_use`, `skill` (pre-computed before ToolDeps construction)

Note: `message` and `cron` build `CommandContext { cfg, state, tex, subagent_depth: 0 }` internally.
Therefore `ToolDeps` must include `cfg: &'a AgentConfig`, `state: &'a AgentState`, `tex: &'a DefaultToolExecutor`.

- [ ] **Step 1: Add module declarations to mod.rs**

In `crates/hydeclaw-core/src/agent/mod.rs`, add:

```rust
pub mod tool_registry;
pub mod tool_handlers;
```

- [ ] **Step 2: Create `tool_registry.rs`**

```rust
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
    pub secrets:             &'a Arc<SecretsManager>,  // &Arc — needed by tool_test, secret_set
    pub sandbox:             &'a Option<Arc<CodeSandbox>>,
    pub session_pools:       Option<&'a SessionPoolsMap>,
    pub memory_store:        &'a Arc<dyn MemoryService>,
    pub agent_map:           Option<&'a AgentMap>,
    pub ui_event_tx:         Option<&'a broadcast::Sender<String>>,
    // Expanded config values (avoid traversing cfg.app_config in every handler)
    pub toolgate_url:        String,
    pub gateway_listen:      &'a str,
    pub signed_url_ttl_secs: u64,
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
            oauth:               deps.oauth,
            cfg:                 deps.cfg,
            state:               deps.state,
            tex:                 deps.tex,
            available_tools:     deps.available_tools,
        }, args).await)
    }
}
```

- [ ] **Step 3: Compile check**

```powershell
cd d:\GIT\bogdan\hydeclaw
cargo check --package hydeclaw-core 2>&1 | Select-String "^error" | Select-Object -First 10
```

Expected: error about missing `tool_handlers` module only. `tool_registry.rs` itself compiles.

- [ ] **Step 4: Commit**

```bash
git add crates/hydeclaw-core/src/agent/tool_registry.rs \
        crates/hydeclaw-core/src/agent/mod.rs
git commit -m "feat(dispatch): add ToolDeps, SystemToolHandler trait, SystemToolRegistry skeleton"
```

---

## Task 2 — Workspace handlers

**Files:**
- Create: `crates/hydeclaw-core/src/agent/tool_handlers/workspace.rs`

### Context

Six arms in engine_dispatch.rs lines 157–176:

| Tool | `ph::*` call |
| --- | --- |
| `workspace_write` | `ph::handle_workspace_write(workspace_dir, agent_name, agent_base, secrets, ttl_secs, args)` |
| `workspace_read` | `ph::handle_workspace_read(workspace_dir, agent_name, args)` |
| `workspace_list` | `ph::handle_workspace_list(workspace_dir, agent_name, args)` |
| `workspace_edit` | `ph::handle_workspace_edit(workspace_dir, agent_name, agent_base, secrets, ttl_secs, args)` |
| `workspace_delete` | `ph::handle_workspace_delete(workspace_dir, agent_name, args)` |
| `workspace_rename` | `ph::handle_workspace_rename(workspace_dir, agent_name, args)` |

- [ ] **Step 1: Write compile-time trait check test**

This goes at the end of `workspace.rs` after handler impls:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_workspace_handlers_implement_trait() {
        fn assert_impl<T: SystemToolHandler>(_: T) {}
        assert_impl(WorkspaceWriteHandler);
        assert_impl(WorkspaceReadHandler);
        assert_impl(WorkspaceListHandler);
        assert_impl(WorkspaceEditHandler);
        assert_impl(WorkspaceDeleteHandler);
        assert_impl(WorkspaceRenameHandler);
    }
}
```

- [ ] **Step 2: Run to verify it fails (handlers not yet defined)**

```powershell
cargo test -p hydeclaw-core tool_handlers::workspace -- --nocapture 2>&1 | Select-String "error\[" | Select-Object -First 5
```

Expected: compile error — handler structs not defined.

- [ ] **Step 3: Implement all 6 workspace handlers**

Create `crates/hydeclaw-core/src/agent/tool_handlers/workspace.rs`:

```rust
use async_trait::async_trait;
use serde_json::Value;
use crate::agent::pipeline::handlers as ph;
use crate::agent::tool_registry::{SystemToolHandler, ToolDeps};

pub struct WorkspaceWriteHandler;
pub struct WorkspaceReadHandler;
pub struct WorkspaceListHandler;
pub struct WorkspaceEditHandler;
pub struct WorkspaceDeleteHandler;
pub struct WorkspaceRenameHandler;

#[async_trait]
impl SystemToolHandler for WorkspaceWriteHandler {
    async fn handle(&self, deps: ToolDeps<'_>, args: &Value) -> String {
        ph::handle_workspace_write(
            deps.workspace_dir, deps.agent_name, deps.agent_base,
            deps.secrets.as_ref(), deps.signed_url_ttl_secs, args,
        ).await
    }
}

#[async_trait]
impl SystemToolHandler for WorkspaceReadHandler {
    async fn handle(&self, deps: ToolDeps<'_>, args: &Value) -> String {
        ph::handle_workspace_read(deps.workspace_dir, deps.agent_name, args).await
    }
}

#[async_trait]
impl SystemToolHandler for WorkspaceListHandler {
    async fn handle(&self, deps: ToolDeps<'_>, args: &Value) -> String {
        ph::handle_workspace_list(deps.workspace_dir, deps.agent_name, args).await
    }
}

#[async_trait]
impl SystemToolHandler for WorkspaceEditHandler {
    async fn handle(&self, deps: ToolDeps<'_>, args: &Value) -> String {
        ph::handle_workspace_edit(
            deps.workspace_dir, deps.agent_name, deps.agent_base,
            deps.secrets.as_ref(), deps.signed_url_ttl_secs, args,
        ).await
    }
}

#[async_trait]
impl SystemToolHandler for WorkspaceDeleteHandler {
    async fn handle(&self, deps: ToolDeps<'_>, args: &Value) -> String {
        ph::handle_workspace_delete(deps.workspace_dir, deps.agent_name, args).await
    }
}

#[async_trait]
impl SystemToolHandler for WorkspaceRenameHandler {
    async fn handle(&self, deps: ToolDeps<'_>, args: &Value) -> String {
        ph::handle_workspace_rename(deps.workspace_dir, deps.agent_name, args).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_workspace_handlers_implement_trait() {
        fn assert_impl<T: SystemToolHandler>(_: T) {}
        assert_impl(WorkspaceWriteHandler);
        assert_impl(WorkspaceReadHandler);
        assert_impl(WorkspaceListHandler);
        assert_impl(WorkspaceEditHandler);
        assert_impl(WorkspaceDeleteHandler);
        assert_impl(WorkspaceRenameHandler);
    }
}
```

- [ ] **Step 4: Run test**

```powershell
cargo test -p hydeclaw-core all_workspace_handlers -- --nocapture
```

Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/hydeclaw-core/src/agent/tool_handlers/workspace.rs
git commit -m "feat(dispatch): add workspace tool handlers (write, read, list, edit, delete, rename)"
```

---

## Task 3 — Simple remaining handlers: web, code, secrets, tools_mgmt, agent_tool

**Files:**
- Create: `crates/hydeclaw-core/src/agent/tool_handlers/web.rs`
- Create: `crates/hydeclaw-core/src/agent/tool_handlers/code.rs`
- Create: `crates/hydeclaw-core/src/agent/tool_handlers/secrets.rs`
- Create: `crates/hydeclaw-core/src/agent/tool_handlers/tools_mgmt.rs`
- Create: `crates/hydeclaw-core/src/agent/tool_handlers/agent_tool.rs`

### Context (from engine_dispatch.rs)

**web_fetch** (lines 190–199):
```rust
let toolgate_url = self.cfg().app_config.toolgate_url.clone()
    .unwrap_or_else(|| "http://localhost:9011".to_string());
psub::handle_web_fetch(self.http_client(), &toolgate_url, &self.cfg().app_config.gateway.listen, arguments).await
```
→ `psub::handle_web_fetch(deps.http_client, &deps.toolgate_url, deps.gateway_listen, args)`

**code_exec** (lines 259–267):
```rust
ps::handle_code_exec(arguments, &self.cfg().agent.name, self.cfg().agent.base,
    self.sandbox(), &self.cfg().workspace_dir, self.secrets().as_ref(),
    self.cfg().app_config.uploads.signed_url_ttl_secs).await
```
→ same params from `deps`

**secret_set** (line 250):
```rust
ph::handle_secret_set(self.secrets().as_ref(), &self.cfg().agent.name, self.cfg().agent.base, arguments).await
```

**tool_create / tool_list / tool_verify / tool_disable** (lines 200–204): each is `ph::handle_tool_*(deps.workspace_dir, args)`

**tool_test** (line 202):
```rust
ph::handle_tool_test(workspace_dir, http_client, ssrf_client, secrets, agent_name, oauth.as_ref(), args)
```
Check if `secrets` param is `&SecretsManager` or `&Arc<SecretsManager>` by reading `ph::handle_tool_test` signature in `handlers.rs`. Pass the correct one.

**tool_discover** (line 249): `ph::handle_tool_discover(workspace_dir, ssrf_client, args)`

**agent** (lines 180–189):
```rust
crate::agent::pipeline::agent_tool::handle_agent_tool(
    self.cfg().session_pools.as_ref(), self.cfg().agent_map.as_ref(),
    &self.cfg().db, &self.cfg().agent.name, arguments,
    AgentToolTimeouts::from(&self.cfg().app_config.agent_tool),
).await
```
`AgentToolTimeouts` must be pre-computed. Add to ToolDeps:
```rust
pub agent_tool_timeouts: crate::agent::pipeline::agent_tool::AgentToolTimeouts,
```
In `from_engine`:
```rust
agent_tool_timeouts: crate::agent::pipeline::agent_tool::AgentToolTimeouts::from(
    &cfg.app_config.agent_tool,
),
```
In `dispatch()` re-borrow, copy it (`AgentToolTimeouts` derives `Copy`):
```rust
agent_tool_timeouts: deps.agent_tool_timeouts,
```

**agents_list** (lines 252–257): `sessions::handle_agents_list(agent_map, session_pools, agent_name, args)`

- [ ] **Step 1: Add `agent_tool_timeouts` to ToolDeps**

In `tool_registry.rs`, add to the struct:
```rust
pub agent_tool_timeouts: crate::agent::pipeline::agent_tool::AgentToolTimeouts,
```
In `from_engine`:
```rust
agent_tool_timeouts: crate::agent::pipeline::agent_tool::AgentToolTimeouts::from(
    &cfg.app_config.agent_tool,
),
```
In `dispatch()`, add to the re-borrow block:
```rust
agent_tool_timeouts: deps.agent_tool_timeouts,
```

- [ ] **Step 2: Create `web.rs`**

```rust
use async_trait::async_trait;
use serde_json::Value;
use crate::agent::pipeline::subagent as psub;
use crate::agent::tool_registry::{SystemToolHandler, ToolDeps};

pub struct WebFetchHandler;

#[async_trait]
impl SystemToolHandler for WebFetchHandler {
    async fn handle(&self, deps: ToolDeps<'_>, args: &Value) -> String {
        psub::handle_web_fetch(
            deps.http_client, &deps.toolgate_url, deps.gateway_listen, args,
        ).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn implements_trait() {
        fn assert_impl<T: SystemToolHandler>(_: T) {}
        assert_impl(WebFetchHandler);
    }
}
```

- [ ] **Step 3: Create `code.rs`**

```rust
use async_trait::async_trait;
use serde_json::Value;
use crate::agent::pipeline::sandbox as ps;
use crate::agent::tool_registry::{SystemToolHandler, ToolDeps};

pub struct CodeExecHandler;

#[async_trait]
impl SystemToolHandler for CodeExecHandler {
    async fn handle(&self, deps: ToolDeps<'_>, args: &Value) -> String {
        ps::handle_code_exec(
            args, deps.agent_name, deps.agent_base,
            deps.sandbox, deps.workspace_dir,
            deps.secrets.as_ref(), deps.signed_url_ttl_secs,
        ).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn implements_trait() {
        fn assert_impl<T: SystemToolHandler>(_: T) {}
        assert_impl(CodeExecHandler);
    }
}
```

- [ ] **Step 4: Create `secrets.rs`**

```rust
use async_trait::async_trait;
use serde_json::Value;
use crate::agent::pipeline::handlers as ph;
use crate::agent::tool_registry::{SystemToolHandler, ToolDeps};

pub struct SecretSetHandler;

#[async_trait]
impl SystemToolHandler for SecretSetHandler {
    async fn handle(&self, deps: ToolDeps<'_>, args: &Value) -> String {
        // handle_secret_set takes &Arc<SecretsManager> — pass deps.secrets directly
        ph::handle_secret_set(deps.secrets, deps.agent_name, deps.agent_base, args).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn implements_trait() {
        fn assert_impl<T: SystemToolHandler>(_: T) {}
        assert_impl(SecretSetHandler);
    }
}
```

- [ ] **Step 5: Create `tools_mgmt.rs`**

```rust
use async_trait::async_trait;
use serde_json::Value;
use crate::agent::pipeline::handlers as ph;
use crate::agent::tool_registry::{SystemToolHandler, ToolDeps};

pub struct ToolCreateHandler;
pub struct ToolListHandler;
pub struct ToolTestHandler;
pub struct ToolVerifyHandler;
pub struct ToolDisableHandler;
pub struct ToolDiscoverHandler;

#[async_trait]
impl SystemToolHandler for ToolCreateHandler {
    async fn handle(&self, deps: ToolDeps<'_>, args: &Value) -> String {
        ph::handle_tool_create(deps.workspace_dir, args).await
    }
}

#[async_trait]
impl SystemToolHandler for ToolListHandler {
    async fn handle(&self, deps: ToolDeps<'_>, args: &Value) -> String {
        ph::handle_tool_list(deps.workspace_dir, args).await
    }
}

#[async_trait]
impl SystemToolHandler for ToolTestHandler {
    async fn handle(&self, deps: ToolDeps<'_>, args: &Value) -> String {
        // handle_tool_test takes &Arc<SecretsManager> — pass deps.secrets directly
        ph::handle_tool_test(
            deps.workspace_dir, deps.http_client, deps.ssrf_client,
            deps.secrets, deps.agent_name,
            deps.oauth.as_ref().and_then(|o| o.as_deref()),
            args,
        ).await
    }
}

#[async_trait]
impl SystemToolHandler for ToolVerifyHandler {
    async fn handle(&self, deps: ToolDeps<'_>, args: &Value) -> String {
        ph::handle_tool_verify(deps.workspace_dir, args).await
    }
}

#[async_trait]
impl SystemToolHandler for ToolDisableHandler {
    async fn handle(&self, deps: ToolDeps<'_>, args: &Value) -> String {
        ph::handle_tool_disable(deps.workspace_dir, args).await
    }
}

#[async_trait]
impl SystemToolHandler for ToolDiscoverHandler {
    async fn handle(&self, deps: ToolDeps<'_>, args: &Value) -> String {
        ph::handle_tool_discover(deps.workspace_dir, deps.ssrf_client, args).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_tools_mgmt_handlers_implement_trait() {
        fn assert_impl<T: SystemToolHandler>(_: T) {}
        assert_impl(ToolCreateHandler);
        assert_impl(ToolListHandler);
        assert_impl(ToolTestHandler);
        assert_impl(ToolVerifyHandler);
        assert_impl(ToolDisableHandler);
        assert_impl(ToolDiscoverHandler);
    }
}
```

- [ ] **Step 6: Create `agent_tool.rs`**

```rust
use async_trait::async_trait;
use serde_json::Value;
use crate::agent::tool_registry::{SystemToolHandler, ToolDeps};

pub struct AgentToolHandler;
pub struct AgentsListHandler;

#[async_trait]
impl SystemToolHandler for AgentToolHandler {
    async fn handle(&self, deps: ToolDeps<'_>, args: &Value) -> String {
        crate::agent::pipeline::agent_tool::handle_agent_tool(
            deps.session_pools,
            deps.agent_map,
            deps.db,
            deps.agent_name,
            args,
            deps.agent_tool_timeouts.clone(),
        ).await
    }
}

#[async_trait]
impl SystemToolHandler for AgentsListHandler {
    async fn handle(&self, deps: ToolDeps<'_>, args: &Value) -> String {
        crate::agent::pipeline::sessions::handle_agents_list(
            deps.agent_map, deps.session_pools, deps.agent_name, args,
        ).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn implements_trait() {
        fn assert_impl<T: SystemToolHandler>(_: T) {}
        assert_impl(AgentToolHandler);
        assert_impl(AgentsListHandler);
    }
}
```

- [ ] **Step 7: Compile check — fix any type mismatches**

```powershell
cargo check --package hydeclaw-core 2>&1 | Select-String "^error" | Select-Object -First 15
```

Common issue: verify `ph::handle_tool_test` param order matches the implementation above. `AgentToolTimeouts` is `Copy` — no clone needed.

- [ ] **Step 8: Commit**

```bash
git add crates/hydeclaw-core/src/agent/tool_handlers/web.rs \
        crates/hydeclaw-core/src/agent/tool_handlers/code.rs \
        crates/hydeclaw-core/src/agent/tool_handlers/secrets.rs \
        crates/hydeclaw-core/src/agent/tool_handlers/tools_mgmt.rs \
        crates/hydeclaw-core/src/agent/tool_handlers/agent_tool.rs \
        crates/hydeclaw-core/src/agent/tool_registry.rs
git commit -m "feat(dispatch): add web, code, secrets, tools_mgmt, agent_tool handlers; add agent_tool_timeouts to ToolDeps"
```

---

## Task 4 — Memory handler (migrate `dispatch_memory_tool`)

**Files:**
- Create: `crates/hydeclaw-core/src/agent/tool_handlers/memory.rs`

### Context

`dispatch_memory_tool` (engine_dispatch.rs lines 423–454). Six sub-actions:
- `search` → `pipeline_memory::handle_memory_search(memory_store.as_ref(), agent_name, &pinned_ids, args)` where `pinned_ids = deps.tex.pinned_chunk_ids.lock().await.clone()`
- `index` → `handle_memory_index(memory_store.as_ref(), agent_name, args)`
- `reindex` → `handle_memory_reindex(memory_store.as_ref(), agent_name, workspace_dir, args)`
- `get` → `handle_memory_get(memory_store.as_ref(), args)`
- `delete` → `handle_memory_delete(memory_store.as_ref(), args)`
- `update` → remaps `sub_action`→`action`, calls `handle_memory_update(&tex.memory_md_lock, workspace_dir, agent_name, &args)`

- [ ] **Step 1: Write unit test for action routing**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn implements_trait() {
        fn assert_impl<T: SystemToolHandler>(_: T) {}
        assert_impl(MemoryToolHandler);
    }
    #[test]
    fn unknown_action_returns_descriptive_error() {
        let expected = "Error: unknown memory action 'bogus'. Use: search, index, reindex, get, delete, update.";
        let msg = format!(
            "Error: unknown memory action '{}'. Use: search, index, reindex, get, delete, update.",
            "bogus"
        );
        assert_eq!(msg, expected);
    }
}
```

- [ ] **Step 2: Run test to verify failure**

```powershell
cargo test -p hydeclaw-core tool_handlers::memory -- --nocapture 2>&1 | Select-String "error\[" | Select-Object -First 5
```

Expected: compile error.

- [ ] **Step 3: Implement `MemoryToolHandler`**

```rust
use async_trait::async_trait;
use serde_json::Value;
use crate::agent::tool_registry::{SystemToolHandler, ToolDeps};

pub struct MemoryToolHandler;

#[async_trait]
impl SystemToolHandler for MemoryToolHandler {
    async fn handle(&self, deps: ToolDeps<'_>, arguments: &Value) -> String {
        use crate::agent::pipeline::memory as pm;
        let action = arguments.get("action").and_then(|v| v.as_str()).unwrap_or("");
        match action {
            "search" => {
                let pinned_ids = deps.tex.pinned_chunk_ids.lock().await.clone();
                pm::handle_memory_search(
                    deps.memory_store.as_ref(), deps.agent_name, &pinned_ids, arguments,
                ).await
            }
            "index" => pm::handle_memory_index(
                deps.memory_store.as_ref(), deps.agent_name, arguments,
            ).await,
            "reindex" => pm::handle_memory_reindex(
                deps.memory_store.as_ref(), deps.agent_name, deps.workspace_dir, arguments,
            ).await,
            "get" => pm::handle_memory_get(deps.memory_store.as_ref(), arguments).await,
            "delete" => pm::handle_memory_delete(deps.memory_store.as_ref(), arguments).await,
            "update" => {
                let mut args = arguments.clone();
                if let Some(sa) = arguments.get("sub_action").cloned()
                    && let Some(obj) = args.as_object_mut()
                {
                    obj.insert("action".to_string(), sa);
                }
                pm::handle_memory_update(
                    &deps.tex.memory_md_lock,
                    deps.workspace_dir,
                    deps.agent_name,
                    &args,
                ).await
            }
            _ => format!(
                "Error: unknown memory action '{}'. Use: search, index, reindex, get, delete, update.",
                action
            ),
        }
    }
}
```

- [ ] **Step 4: Run tests**

```powershell
cargo test -p hydeclaw-core tool_handlers::memory -- --nocapture
```

Expected: both tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/hydeclaw-core/src/agent/tool_handlers/memory.rs
git commit -m "feat(dispatch): add MemoryToolHandler (migrated from dispatch_memory_tool)"
```

---

## Task 5 — Comms handlers: message, cron, git, canvas, rich_card, browser_action, process

**Files:**
- Create: `crates/hydeclaw-core/src/agent/tool_handlers/comms.rs`

### Context

- **message** (line 178): `self.handle_message_action(args)` builds `CommandContext { cfg, state, tex, subagent_depth: 0 }` then calls `channel_actions::handle_message_action`
- **cron** (line 179): same pattern → `cron::handle_cron`
- **git** (line 268): `self.dispatch_git_tool(args)` — copy that method body verbatim, replacing `self.cfg().workspace_dir` → `deps.workspace_dir`
- **canvas** (line 269): `canvas::handle_canvas(&tex.canvas_state, agent_name, ui_event_tx, http_client, args)`
- **rich_card** (line 270): `ph::handle_rich_card(args)` — synchronous
- **browser_action** (line 258): `ph::handle_browser_action(http_client, &canvas::browser_renderer_url(), args)`
- **process** (line 271): `self.dispatch_process_tool(args)` — copy that body verbatim

For `GitToolHandler`: read `dispatch_git_tool` body (lines 494–621 of engine_dispatch.rs), copy verbatim, replace `self.cfg().workspace_dir` → `deps.workspace_dir`.

For `ProcessHandler`: read `dispatch_process_tool` body (lines 483–492), copy verbatim, replace `self.tex().bg_processes` → `deps.tex.bg_processes` and `self.cfg().agent.name` → `deps.agent_name`.

- [ ] **Step 1: Write trait tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_comms_handlers_implement_trait() {
        fn assert_impl<T: SystemToolHandler>(_: T) {}
        assert_impl(MessageHandler);
        assert_impl(CronHandler);
        assert_impl(GitToolHandler);
        assert_impl(CanvasHandler);
        assert_impl(RichCardHandler);
        assert_impl(BrowserActionHandler);
        assert_impl(ProcessHandler);
    }

    #[test]
    fn process_unknown_action_error_format() {
        let msg = format!(
            "Error: unknown process action '{}'. Use: start, status, logs, kill.", "bad"
        );
        assert!(msg.contains("bad"));
    }
}
```

- [ ] **Step 2: Implement `MessageHandler` and `CronHandler`**

```rust
use async_trait::async_trait;
use serde_json::Value;
use crate::agent::pipeline::handlers as ph;
use crate::agent::tool_registry::{SystemToolHandler, ToolDeps};

pub struct MessageHandler;

#[async_trait]
impl SystemToolHandler for MessageHandler {
    async fn handle(&self, deps: ToolDeps<'_>, args: &Value) -> String {
        let ctx = crate::agent::pipeline::CommandContext {
            cfg: deps.cfg,
            state: deps.state,
            tex: deps.tex,
            subagent_depth: 0,
        };
        crate::agent::pipeline::channel_actions::handle_message_action(&ctx, args).await
    }
}

pub struct CronHandler;

#[async_trait]
impl SystemToolHandler for CronHandler {
    async fn handle(&self, deps: ToolDeps<'_>, args: &Value) -> String {
        let ctx = crate::agent::pipeline::CommandContext {
            cfg: deps.cfg,
            state: deps.state,
            tex: deps.tex,
            subagent_depth: 0,
        };
        crate::agent::pipeline::cron::handle_cron(&ctx, args).await
    }
}
```

- [ ] **Step 3: Implement `GitToolHandler`**

Copy `dispatch_git_tool` method body from `engine_dispatch.rs` lines 494–621 verbatim into `GitToolHandler::handle()`. Replace all `self.cfg().workspace_dir` with `deps.workspace_dir`. There are no other `self.*` references in that method.

- [ ] **Step 4: Implement `CanvasHandler`, `RichCardHandler`, `BrowserActionHandler`**

```rust
pub struct CanvasHandler;

#[async_trait]
impl SystemToolHandler for CanvasHandler {
    async fn handle(&self, deps: ToolDeps<'_>, args: &Value) -> String {
        crate::agent::pipeline::canvas::handle_canvas(
            &deps.tex.canvas_state,
            deps.agent_name,
            deps.ui_event_tx,
            deps.http_client,
            args,
        ).await
    }
}

pub struct RichCardHandler;

#[async_trait]
impl SystemToolHandler for RichCardHandler {
    async fn handle(&self, _deps: ToolDeps<'_>, args: &Value) -> String {
        ph::handle_rich_card(args)
    }
}

pub struct BrowserActionHandler;

#[async_trait]
impl SystemToolHandler for BrowserActionHandler {
    async fn handle(&self, deps: ToolDeps<'_>, args: &Value) -> String {
        ph::handle_browser_action(
            deps.http_client,
            &crate::agent::pipeline::canvas::browser_renderer_url(),
            args,
        ).await
    }
}
```

- [ ] **Step 5: Implement `ProcessHandler`**

Copy `dispatch_process_tool` body (lines 483–492) verbatim into `ProcessHandler::handle()`. Replace `self.tex().bg_processes` → `deps.tex.bg_processes` and `self.cfg().agent.name` → `deps.agent_name`.

- [ ] **Step 6: Compile check**

```powershell
cargo check --package hydeclaw-core 2>&1 | Select-String "^error" | Select-Object -First 10
```

Verify `CommandContext` field names match those in `crate::agent::pipeline` (check `pipeline/mod.rs`).

- [ ] **Step 7: Commit**

```bash
git add crates/hydeclaw-core/src/agent/tool_handlers/comms.rs
git commit -m "feat(dispatch): add comms handlers (message, cron, git, canvas, rich_card, browser, process)"
```

---

## Task 6 — Skills handlers

**Files:**
- Create: `crates/hydeclaw-core/src/agent/tool_handlers/skills.rs`

### Context

**skill** arm (line 205): `self.dispatch_skill_tool(args)` → routes on action:
- `create`/`update` → `ph::handle_skill_create(workspace_dir, args)`
- `list` → `ph::handle_skill_list(workspace_dir, agent_base, &available_tools, args)`
- unknown → error string

**skill_use** arm (lines 206–248): routes on action:
- `capture` → `ph::handle_skill_capture(workspace_dir, agent_name, db, ui_event_tx, args)`
- `load` + archived skill → spawn `reactivate_skill` + call `handle_skill_use` + append note
- default → `ph::handle_skill_use(workspace_dir, agent_base, &available_tools, args)`

Full `skill_use` logic is identical to what's in engine_dispatch.rs lines 206–248 — copy verbatim, replacing `self.cfg().*` → `deps.*`.

- [ ] **Step 1: Write routing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn both_handlers_implement_trait() {
        fn assert_impl<T: SystemToolHandler>(_: T) {}
        assert_impl(SkillHandler);
        assert_impl(SkillUseHandler);
    }
    #[test]
    fn skill_unknown_action_error_format() {
        let msg = format!(
            "Error: unknown skill action '{}'. Use: create, update, list.", "bad"
        );
        assert!(msg.contains("bad"));
    }
}
```

- [ ] **Step 2: Implement `SkillHandler`**

```rust
use async_trait::async_trait;
use serde_json::Value;
use crate::agent::pipeline::handlers as ph;
use crate::agent::tool_registry::{SystemToolHandler, ToolDeps};

pub struct SkillHandler;

#[async_trait]
impl SystemToolHandler for SkillHandler {
    async fn handle(&self, deps: ToolDeps<'_>, arguments: &Value) -> String {
        let action = arguments.get("action").and_then(|v| v.as_str()).unwrap_or("");
        match action {
            "create" | "update" => ph::handle_skill_create(deps.workspace_dir, arguments).await,
            "list" => ph::handle_skill_list(
                deps.workspace_dir, deps.agent_base, deps.available_tools, arguments,
            ).await,
            _ => format!(
                "Error: unknown skill action '{}'. Use: create, update, list.", action
            ),
        }
    }
}
```

- [ ] **Step 3: Implement `SkillUseHandler`**

Copy the `skill_use` match arm body from engine_dispatch.rs lines 206–248 verbatim into `SkillUseHandler::handle()`. Replace `self.cfg().workspace_dir` → `deps.workspace_dir`, `self.cfg().agent.name` → `deps.agent_name`, `self.cfg().agent.base` → `deps.agent_base`, `self.cfg().db` → `deps.db`, `self.state().ui_event_tx.as_ref()` → `deps.ui_event_tx`, and the `let available = self.available_tool_names().await;` lines → use `deps.available_tools` directly (already pre-computed).

```rust
pub struct SkillUseHandler;

#[async_trait]
impl SystemToolHandler for SkillUseHandler {
    async fn handle(&self, deps: ToolDeps<'_>, arguments: &Value) -> String {
        let action = arguments.get("action").and_then(|v| v.as_str()).unwrap_or("list");
        if action == "capture" {
            return ph::handle_skill_capture(
                deps.workspace_dir, deps.agent_name, deps.db,
                deps.ui_event_tx, arguments,
            ).await;
        }
        if action == "load" {
            if let Some(name) = arguments.get("name").and_then(|v| v.as_str()) {
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
                            deps.workspace_dir, deps.agent_base, deps.available_tools, arguments,
                        ).await;
                        return format!(
                            "{}\n\n*(This skill was archived and has been reactivated.)*",
                            result
                        );
                    }
                }
            }
        }
        ph::handle_skill_use(
            deps.workspace_dir, deps.agent_base, deps.available_tools, arguments,
        ).await
    }
}
```

- [ ] **Step 4: Run tests**

```powershell
cargo test -p hydeclaw-core tool_handlers::skills -- --nocapture
```

Expected: both tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/hydeclaw-core/src/agent/tool_handlers/skills.rs
git commit -m "feat(dispatch): add SkillHandler and SkillUseHandler"
```

---

## Task 7 — Session handler

**Files:**
- Create: `crates/hydeclaw-core/src/agent/tool_handlers/session.rs`

### Context

`dispatch_session_tool` (lines 469–481). Six sub-actions:
- `list` → `sessions::handle_sessions_list(db, agent_name, args)`
- `history` → `sessions::handle_sessions_history(db, agent_name, args)`
- `search` → `sessions::handle_session_search(db, agent_name, args)`
- `context` → `sessions::handle_session_context(db, args)`
- `send` → `sessions::handle_session_send(state.channel_router.as_ref(), args)`
- `export` → `sessions::handle_session_export(db, args)`

- [ ] **Step 1: Write routing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn implements_trait() {
        fn assert_impl<T: SystemToolHandler>(_: T) {}
        assert_impl(SessionHandler);
    }
    #[test]
    fn unknown_action_error_format() {
        let msg = format!(
            "Error: unknown session action '{}'. Use: list, history, search, context, send, export.",
            "bad"
        );
        assert!(msg.contains("bad"));
    }
}
```

- [ ] **Step 2: Implement `SessionHandler`**

```rust
use async_trait::async_trait;
use serde_json::Value;
use crate::agent::tool_registry::{SystemToolHandler, ToolDeps};

pub struct SessionHandler;

#[async_trait]
impl SystemToolHandler for SessionHandler {
    async fn handle(&self, deps: ToolDeps<'_>, arguments: &Value) -> String {
        use crate::agent::pipeline::sessions;
        let action = arguments.get("action").and_then(|v| v.as_str()).unwrap_or("");
        match action {
            "list" => sessions::handle_sessions_list(deps.db, deps.agent_name, arguments).await,
            "history" => sessions::handle_sessions_history(deps.db, deps.agent_name, arguments).await,
            "search" => sessions::handle_session_search(deps.db, deps.agent_name, arguments).await,
            "context" => sessions::handle_session_context(deps.db, arguments).await,
            "send" => sessions::handle_session_send(
                deps.state.channel_router.as_ref(), arguments,
            ).await,
            "export" => sessions::handle_session_export(deps.db, arguments).await,
            _ => format!(
                "Error: unknown session action '{}'. Use: list, history, search, context, send, export.",
                action
            ),
        }
    }
}
```

- [ ] **Step 3: Compile + test**

```powershell
cargo check --package hydeclaw-core 2>&1 | Select-String "^error" | Select-Object -First 10
cargo test -p hydeclaw-core tool_handlers::session -- --nocapture
```

- [ ] **Step 4: Commit**

```bash
git add crates/hydeclaw-core/src/agent/tool_handlers/session.rs
git commit -m "feat(dispatch): add SessionHandler (migrated from dispatch_session_tool)"
```

---

## Task 8 — Wire up registry + replace dispatch match

**Files:**
- Create: `crates/hydeclaw-core/src/agent/tool_handlers/mod.rs`
- Modify: `crates/hydeclaw-core/src/agent/engine/mod.rs`
- Modify: `crates/hydeclaw-core/src/agent/engine_dispatch.rs`

- [ ] **Step 1: Create `tool_handlers/mod.rs`**

```rust
mod workspace;
mod memory;
mod skills;
mod agent_tool;
mod tools_mgmt;
mod web;
mod code;
mod comms;
mod secrets;
mod session;

use workspace::*;
use memory::*;
use skills::*;
use agent_tool::*;
use tools_mgmt::*;
use web::*;
use code::*;
use comms::*;
use secrets::*;
use session::*;

use crate::agent::tool_registry::SystemToolRegistry;

impl SystemToolRegistry {
    pub fn build() -> Self {
        let mut r = Self::new();
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
}
```

- [ ] **Step 2: Add `tool_registry` field to `AgentEngine`**

In `engine/mod.rs`, locate `pub struct AgentEngine` and add:

```rust
pub(crate) tool_registry: std::sync::Arc<crate::agent::tool_registry::SystemToolRegistry>,
```

In `AgentEngine::new()` (wherever the struct is initialized), add:

```rust
tool_registry: std::sync::Arc::new(crate::agent::tool_registry::SystemToolRegistry::build()),
```

- [ ] **Step 3: Replace the match in `execute_tool_call_inner`**

In `engine_dispatch.rs`, find `// 1. Internal tools — match dispatch table` (line 155). Replace from there through the end of the fallback chain with:

```rust
            // 1. System tools (registry)
            let available = self.available_tool_names().await;
            let deps = crate::agent::tool_registry::ToolDeps::from_engine(self, &available);
            if let Some(result) = self.tool_registry.dispatch(name, &deps, arguments).await {
                return result;
            }

            // 2. YAML-defined tools — only VERIFIED may be called directly.
            if let Some(yaml_tool) = crate::tools::yaml_tools::find_yaml_tool(
                &self.cfg().workspace_dir,
                name,
            ).await {
                if yaml_tool.status == crate::tools::yaml_tools::ToolStatus::Draft {
                    return format!(
                        "Tool '{}' is in DRAFT status and cannot be called directly. \
                        Use tool_test(tool_name=\"{}\", test_params={{...}}) to test it, \
                        then tool_verify(tool_name=\"{}\") to promote it to verified.",
                        name, name, name
                    );
                }
                if yaml_tool.required_base && !self.cfg().agent.base {
                    return format!("Tool '{}' requires base agent.", name);
                }
                if name.starts_with("github_") {
                    let owner = arguments.get("owner").and_then(|v| v.as_str()).unwrap_or("");
                    let repo_name = arguments.get("repo").and_then(|v| v.as_str()).unwrap_or("");
                    if owner.is_empty() || repo_name.is_empty() {
                        return "GitHub tools require 'owner' and 'repo' parameters.".to_string();
                    }
                    match crate::db::github::check_repo_access(
                        &self.cfg().db, &self.cfg().agent.name, owner, repo_name,
                    ).await {
                        Ok(true) => {}
                        Ok(false) => return format!(
                            "Repository {}/{} is not in the allowed list for agent '{}'. \
                            Add it via POST /api/agents/{}/github/repos",
                            owner, repo_name, self.cfg().agent.name, self.cfg().agent.name
                        ),
                        Err(e) => return format!("Error checking repo access: {}", e),
                    }
                }
                if let Some(ref ca) = yaml_tool.channel_action.clone() {
                    return self.execute_yaml_channel_action(&yaml_tool, arguments, ca).await;
                }
                if CACHEABLE_SEARCH_TOOLS.contains(&name)
                    && let Some(q) = arguments.get("query").and_then(|v| v.as_str())
                    && let Some(cached) = self.check_search_cache(q).await
                {
                    return cached;
                }
                let resolver = self.make_resolver();
                let oauth_ctx = self.make_oauth_context();
                let client = if crate::tools::ssrf::is_internal_endpoint(&yaml_tool.endpoint) {
                    self.http_client()
                } else {
                    self.ssrf_http_client()
                };
                return match yaml_tool.execute_oauth(
                    arguments, client, Some(&resolver), oauth_ctx.as_ref(),
                ).await {
                    Ok(result) => {
                        if CACHEABLE_SEARCH_TOOLS.contains(&name)
                            && let Some(q) = arguments.get("query").and_then(|v| v.as_str())
                        {
                            self.store_search_cache(q, &result).await;
                        }
                        result
                    }
                    Err(e) => Self::format_tool_error(name, &e.to_string()),
                };
            }

            // 3. MCP tools
            if let Some(mcp) = self.mcp()
                && let Some(mcp_name) = mcp.find_mcp_for_tool(name).await
            {
                return match mcp.call_tool(&mcp_name, name, arguments).await {
                    Ok(result) => result,
                    Err(e) => Self::format_tool_error(name, &e.to_string()),
                };
            }

            // 4. External tool registry
            match self.cfg().tools.call(name, arguments).await {
                Ok(result) => serde_json::to_string(&result).unwrap_or_default(),
                Err(e) => {
                    let msg = e.to_string();
                    if msg.contains("tool not found") {
                        tracing::warn!(tool = %name, "LLM called non-existent tool");
                        format!(
                            "Error: tool '{}' does not exist. Use tool_list to see available tools.",
                            name
                        )
                    } else {
                        Self::format_tool_error(name, &msg)
                    }
                }
            }
```

- [ ] **Step 4: Delete the 5 dead dispatch methods**

From `engine_dispatch.rs`, delete these complete method bodies:
- `async fn dispatch_memory_tool` (lines 423–454)
- `async fn dispatch_skill_tool` (lines 456–467)
- `async fn dispatch_session_tool` (lines 469–481)
- `async fn dispatch_process_tool` (lines 483–492)
- `async fn dispatch_git_tool` (lines 494–621)

Also remove the comment `// ── Dispatch helpers for tools with sub-action routing ──────────────` (line 421).

- [ ] **Step 5: Full compile**

```powershell
cargo check --package hydeclaw-core 2>&1 | Select-String "^error" | Select-Object -First 20
```

Expected: no errors.

- [ ] **Step 6: Commit**

```bash
git add crates/hydeclaw-core/src/agent/tool_handlers/mod.rs \
        crates/hydeclaw-core/src/agent/engine/mod.rs \
        crates/hydeclaw-core/src/agent/engine_dispatch.rs
git commit -m "refactor(dispatch): replace 28-arm match with SystemToolRegistry; delete dispatch_* methods"
```

---

## Task 9 — Final verification

- [ ] **Step 1: Full cargo check all targets**

```powershell
cd d:\GIT\bogdan\hydeclaw
cargo check --all-targets 2>&1 | Select-String "^error" | Select-Object -First 10
```

Expected: no errors.

- [ ] **Step 2: Run test suite**

```powershell
cargo test -p hydeclaw-core 2>&1 | Select-String "FAILED|test result" | Select-Object -First 10
```

Expected: same pass/fail count as before the refactor. Pre-existing DB-integration failures (7 tests, `DATABASE_URL` missing) are expected.

- [ ] **Step 3: Verify 28 registrations**

```powershell
(Select-String -Path "crates/hydeclaw-core/src/agent/tool_handlers/mod.rs" -Pattern "r\.register").Count
```

Expected: `28`

- [ ] **Step 4: Verify engine_dispatch.rs shrunk**

```powershell
(Get-Content crates/hydeclaw-core/src/agent/engine_dispatch.rs).Count
```

Expected: under 250 (was 622).

- [ ] **Step 5: Verify no dead method references**

```powershell
Select-String -Path "crates/hydeclaw-core/src/agent/engine_dispatch.rs" `
    -Pattern "dispatch_memory_tool|dispatch_git_tool|dispatch_skill_tool|dispatch_session_tool|dispatch_process_tool"
```

Expected: no matches.

- [ ] **Step 6: Final commit**

```bash
git add .
git commit -m "test(dispatch): verify SystemToolRegistry covers all 28 tools"
```
