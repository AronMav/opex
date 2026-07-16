# Global `always_core` Tool Promotion — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let an operator promote named extension tools (initially `sequentialthinking`) into the native `tools[]` array of every dispatcher-mode agent — main agents AND dispatcher-mode subagents — via one global `opex.toml` knob, while excluding those tools from the dispatcher catalogue/hint/suppressor.

**Architecture:** New global `[tool_dispatcher] always_core` config on `AppConfig`. Reachable everywhere via the `Arc<AppConfig>` already held by the runtime `AgentConfig`. Two promotion paths (main-agent `context_builder` retain, subagent `subagent_runner` injection) keep the tool native; one shared exclusion (`build_extension_tool_list` gains an `always_core` filter param) removes it from the dispatcher catalogue, tool hint, and hallucination suppressor.

**Tech Stack:** Rust 2024, tokio, serde/toml, sqlx (unrelated here). No new dependencies.

**Spec:** [docs/superpowers/specs/2026-07-16-global-always-core-tool-promotion-design.md](../specs/2026-07-16-global-always-core-tool-promotion-design.md)

## Global Constraints

- **rustls-tls only — never add OpenSSL.** (Project-wide.)
- **Tests live in the BIN target.** Run `cargo test -p opex-core --bin opex-core <name>` — `--lib` reports 0 tests. The Windows dev box cannot run opex-core Rust tests (process crashes); **authoritative test runs happen on the server** (`aronmav@188.246.224.118`, `~/opex-src`). Local `cargo check` is fine for compile verification.
- **`cargo check` does NOT catch clippy `-D warnings`.** Run `make lint` (`cargo clippy --all-targets -- -D warnings`) before declaring a task done.
- **Rust unit tests are inline** in the same file under `#[cfg(test)] mod tests` (the codebase convention — see `hallucinated_tool.rs`, `config/mod.rs`).
- **Commits:** work directly in `master`; no feature branch. **Never add `Co-Authored-By`** or any Claude attribution to commit messages. Do not `git push` (user pushes manually).
- **Backward compatibility:** an `opex.toml` with no `[tool_dispatcher]` section must behave byte-for-byte as before (empty `always_core` ⇒ every added branch is a no-op).

---

## File Structure

| File | Responsibility | Change |
| --- | --- | --- |
| `crates/opex-core/src/config/mod.rs` | `GlobalToolDispatcherConfig` struct + `AppConfig.tool_dispatcher` field | Task 1 |
| `crates/opex-core/src/agent/dispatcher/lookup.rs` | `always_core` exclusion param + filter in `build_extension_tool_list`/`find_extension_tool` | Task 2 |
| `crates/opex-core/src/agent/context_builder.rs` | trait method decl + native-partition predicate + retain + 1 exclusion call-site | Tasks 2, 3 |
| `crates/opex-core/src/agent/engine/context_builder.rs` | `dispatcher_always_core()` trait impl | Task 2 |
| `crates/opex-core/src/agent/pipeline/execute.rs` | pass `always_core` to suppressor-list call-site | Task 2 |
| `crates/opex-core/src/agent/tool_handlers/tool_use.rs` | pass `always_core` to 2 call-sites | Task 2 |
| `crates/opex-core/src/agent/pipeline/subagent_runner.rs` | native injection of `always_core` subset in dispatcher-mode subagents | Task 4 |
| `config/opex.toml` | set `always_core = ["sequentialthinking"]` | Task 6 |

Task 5 (F3 startup warn) adds a pure helper + one call-site in `context_builder.rs`.

---

## Task 1: Global config struct + `AppConfig` field

**Files:**
- Modify: `crates/opex-core/src/config/mod.rs` (add struct after `ToolDispatcherConfig` ~line 1756; add field to `AppConfig` after `pub video: VideoConfig` ~line 2068)
- Test: inline `#[cfg(test)] mod tests` in `crates/opex-core/src/config/mod.rs`

**Interfaces:**
- Produces: `pub struct GlobalToolDispatcherConfig { pub always_core: Vec<String> }` (derives `Default`); `AppConfig.tool_dispatcher: GlobalToolDispatcherConfig`.

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` block in `config/mod.rs`:

```rust
#[test]
fn global_tool_dispatcher_defaults_empty_and_parses() {
    // Absent section ⇒ empty list (backward compat).
    let cfg: AppConfig = toml::from_str(
        r#"
        [gateway]
        listen = "0.0.0.0:18789"
        [database]
        url = "postgres://localhost/test"
        "#,
    )
    .expect("minimal config parses");
    assert!(cfg.tool_dispatcher.always_core.is_empty());

    // Present section ⇒ list parses.
    let cfg2: AppConfig = toml::from_str(
        r#"
        [gateway]
        listen = "0.0.0.0:18789"
        [database]
        url = "postgres://localhost/test"
        [tool_dispatcher]
        always_core = ["sequentialthinking"]
        "#,
    )
    .expect("config with [tool_dispatcher] parses");
    assert_eq!(cfg2.tool_dispatcher.always_core, vec!["sequentialthinking".to_string()]);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run (on server): `cargo test -p opex-core --bin opex-core global_tool_dispatcher_defaults_empty_and_parses`
Expected: FAIL to compile — `no field tool_dispatcher on AppConfig`.

- [ ] **Step 3: Add the struct**

After the `impl Default for ToolDispatcherConfig { ... }` block (~line 1756) in `config/mod.rs`:

```rust
/// Global (process-wide) tool-dispatcher settings, `[tool_dispatcher]` in
/// `opex.toml`. Distinct from the per-agent `[agent.tool_dispatcher]`
/// (`ToolDispatcherConfig`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct GlobalToolDispatcherConfig {
    /// Extension tool names promoted into the native `tools[]` array for EVERY
    /// dispatcher-mode agent (main agents and dispatcher-mode subagents), and
    /// excluded from the dispatcher catalogue/hint/suppressor. Subject to
    /// per-agent deny-list and `required_base` at apply time. Empty by default
    /// (no behaviour change).
    #[serde(default)]
    pub always_core: Vec<String>,
}
```

- [ ] **Step 4: Add the `AppConfig` field**

In `pub struct AppConfig`, immediately after `pub video: VideoConfig,` (the current last field, ~line 2068):

```rust
    /// Global tool-dispatcher settings (`[tool_dispatcher]`). Promotes named
    /// extension tools into the native tools[] of every dispatcher-mode agent.
    #[serde(default)]
    pub tool_dispatcher: GlobalToolDispatcherConfig,
```

- [ ] **Step 5: Run test to verify it passes**

Run (on server): `cargo test -p opex-core --bin opex-core global_tool_dispatcher_defaults_empty_and_parses`
Expected: PASS.

- [ ] **Step 6: Lint + commit**

```bash
make lint
git add crates/opex-core/src/config/mod.rs
git commit -m "feat(dispatcher): add global [tool_dispatcher] always_core config"
```

---

## Task 2: `always_core` exclusion param + trait accessor + all call-sites

Adds the shared exclusion. Because the `build_extension_tool_list` signature changes, ALL four call-sites plus the `find_extension_tool` wrapper are updated in this same task so the crate compiles.

**Files:**
- Modify: `crates/opex-core/src/agent/dispatcher/lookup.rs` (signature + filter in `build_extension_tool_list` ~28-92 and `find_extension_tool` ~98-111; inline test)
- Modify: `crates/opex-core/src/agent/context_builder.rs` (trait decl ~222; call-site ~480)
- Modify: `crates/opex-core/src/agent/engine/context_builder.rs` (trait impl ~670)
- Modify: `crates/opex-core/src/agent/pipeline/execute.rs` (call-site ~185)
- Modify: `crates/opex-core/src/agent/tool_handlers/tool_use.rs` (call-sites ~70, ~145)
- Test: inline in `crates/opex-core/src/agent/dispatcher/lookup.rs`

**Interfaces:**
- Consumes: `AppConfig.tool_dispatcher.always_core` (Task 1).
- Produces:
  - `build_extension_tool_list(is_base_agent, deny, promoted, always_core: &[String], workspace_dir, slots, mcp)` — new 4th param.
  - `find_extension_tool(name, is_base_agent, deny, promoted, always_core: &[String], workspace_dir, slots, mcp)` — new 5th param.
  - trait method `fn dispatcher_always_core(&self) -> &[String]` on `ContextBuilderDeps`.

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` block in `lookup.rs`:

```rust
#[tokio::test]
async fn always_core_name_excluded_from_extension_list() {
    // `process` is a system tool in all_system_tool_names() but NOT in
    // static_core_tool_names(), so it normally appears in the extension list.
    // With it in always_core, it must NOT.
    let slots = crate::db::profiles::Slots::default();
    let without = build_extension_tool_list(
        true, &[], &std::collections::HashSet::new(), &[],
        ".", &slots, None,
    ).await;
    assert!(without.iter().any(|t| t.name == "process"),
        "control: `process` is a system extension tool (not static-core)");

    let with = build_extension_tool_list(
        true, &[], &std::collections::HashSet::new(),
        &["process".to_string()],
        ".", &slots, None,
    ).await;
    assert!(!with.iter().any(|t| t.name == "process"),
        "always_core name must be filtered out of the extension list");
}
```

(`process` is confirmed present in `all_system_tool_names()` and absent from `static_core_tool_names()` in `pipeline/tool_defs.rs`. The system-tool loop in `build_extension_tool_list` adds every `all_system_tool_names()` entry that is not static-core / denied / promoted, regardless of group-gating, so `process` reliably appears for `is_base_agent = true` with an empty deny-list.)

- [ ] **Step 2: Run test to verify it fails**

Run (on server): `cargo test -p opex-core --bin opex-core always_core_name_excluded_from_extension_list`
Expected: FAIL to compile — `build_extension_tool_list` takes 6 args, not 7.

- [ ] **Step 3: Add the param + filter in `lookup.rs`**

Change the `build_extension_tool_list` signature (add `always_core` after `promoted`):

```rust
pub async fn build_extension_tool_list(
    is_base_agent: bool,
    deny: &[String],
    promoted: &std::collections::HashSet<String>,
    always_core: &[String],
    workspace_dir: &str,
    slots: &crate::db::profiles::Slots,
    mcp: Option<&crate::mcp::McpRegistry>,
) -> Vec<ToolDefinition> {
```

Immediately after `let mut out: Vec<ToolDefinition> = Vec::new();`, add a closure and apply it in every push guard. Simplest: filter once at the end. Replace the final two lines:

```rust
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
```

with:

```rust
    // Global always_core tools are promoted to native tools[] elsewhere, so
    // they must NOT appear in the dispatcher catalogue / suppressor list.
    out.retain(|t| !always_core.iter().any(|n| n == &t.name));
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
```

Update `find_extension_tool` to accept and forward the param:

```rust
pub async fn find_extension_tool(
    name: &str,
    is_base_agent: bool,
    deny: &[String],
    promoted: &std::collections::HashSet<String>,
    always_core: &[String],
    workspace_dir: &str,
    slots: &crate::db::profiles::Slots,
    mcp: Option<&crate::mcp::McpRegistry>,
) -> Option<ToolDefinition> {
    build_extension_tool_list(is_base_agent, deny, promoted, always_core, workspace_dir, slots, mcp)
        .await
        .into_iter()
        .find(|t| t.name == name)
}
```

- [ ] **Step 4: Add the trait accessor**

In `context_builder.rs`, in the `ContextBuilderDeps` trait right after `fn agent_core_extra(&self) -> &[String];` (~222):

```rust
    /// Global `[tool_dispatcher] always_core` list — extension tools promoted
    /// to native tools[] for every dispatcher-mode agent (and excluded from the
    /// dispatcher catalogue/hint/suppressor).
    fn dispatcher_always_core(&self) -> &[String];
```

In `engine/context_builder.rs`, right after the `agent_core_extra` impl (~670):

```rust
    fn dispatcher_always_core(&self) -> &[String] {
        &self.cfg().app_config.tool_dispatcher.always_core
    }
```

- [ ] **Step 5: Update the four call-sites**

`context_builder.rs` ~480 (add `deps.dispatcher_always_core()` after the empty `HashSet::new()`):

```rust
            let candidates = crate::agent::dispatcher::build_extension_tool_list(
                deps.agent_base(),
                &deny,
                &std::collections::HashSet::new(),
                deps.dispatcher_always_core(),
                deps.workspace_dir(),
                deps.profile_slots(),
                deps.mcp_registry(),
            ).await;
```

`execute.rs` ~185 (add `&engine.cfg().app_config.tool_dispatcher.always_core`):

```rust
        let names = crate::agent::dispatcher::build_extension_tool_list(
            engine.cfg().agent.base,
            &deny,
            &std::collections::HashSet::new(),
            &engine.cfg().app_config.tool_dispatcher.always_core,
            &engine.cfg().workspace_dir,
            &engine.cfg().profile_slots,
            engine.mcp().as_deref(),
        )
```

`tool_use.rs` ~70 (search):

```rust
    let candidates = dispatcher::build_extension_tool_list(
        deps.agent_base,
        &deny,
        &std::collections::HashSet::new(),
        &deps.cfg.app_config.tool_dispatcher.always_core,
        deps.workspace_dir,
        &deps.cfg.profile_slots,
        deps.mcp,
    ).await;
```

`tool_use.rs` ~145 (`find_extension_tool`, describe):

```rust
    let tool = dispatcher::find_extension_tool(
        name,
        deps.agent_base,
        &deny,
        &std::collections::HashSet::new(),
        &deps.cfg.app_config.tool_dispatcher.always_core,
        deps.workspace_dir,
        &deps.cfg.profile_slots,
        deps.mcp,
    ).await;
```

- [ ] **Step 6: Run test to verify it passes + crate compiles**

Run (on server): `cargo check -p opex-core && cargo test -p opex-core --bin opex-core always_core_name_excluded_from_extension_list`
Expected: check passes; test PASS.

- [ ] **Step 7: Lint + commit**

```bash
make lint
git add crates/opex-core/src/agent/dispatcher/lookup.rs \
        crates/opex-core/src/agent/context_builder.rs \
        crates/opex-core/src/agent/engine/context_builder.rs \
        crates/opex-core/src/agent/pipeline/execute.rs \
        crates/opex-core/src/agent/tool_handlers/tool_use.rs
git commit -m "feat(dispatcher): exclude always_core tools from extension catalogue/hint/suppressor"
```

---

## Task 3: Native promotion for main agents (context_builder retain)

**Files:**
- Modify: `crates/opex-core/src/agent/context_builder.rs` (extract predicate ~near module fns; use in retain ~706)
- Test: inline in `crates/opex-core/src/agent/context_builder.rs`

**Interfaces:**
- Consumes: `deps.dispatcher_always_core()` (Task 2).
- Produces: `fn keep_in_native_partition(name: &str, core_names: &std::collections::HashSet<&str>, core_extra: &std::collections::HashSet<String>, always_core: &[String]) -> bool`.

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` block in `context_builder.rs`:

```rust
#[test]
fn native_partition_keeps_always_core() {
    use std::collections::HashSet;
    let core: HashSet<&str> = ["workspace_read"].into_iter().collect();
    let core_extra: HashSet<String> = HashSet::new();
    let always = vec!["sequentialthinking".to_string()];

    // static core kept
    assert!(super::keep_in_native_partition("workspace_read", &core, &core_extra, &always));
    // always_core kept
    assert!(super::keep_in_native_partition("sequentialthinking", &core, &core_extra, &always));
    // unrelated extension dropped
    assert!(!super::keep_in_native_partition("brave_search", &core, &core_extra, &always));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run (on server): `cargo test -p opex-core --bin opex-core native_partition_keeps_always_core`
Expected: FAIL to compile — `keep_in_native_partition` not found.

- [ ] **Step 3: Add the predicate**

Add a module-level fn in `context_builder.rs` (near the other free fns, e.g. beside `shares_significant_token`):

```rust
/// Whether a tool stays in the native per-turn `tools[]` array under the
/// dispatcher partition: static core, per-agent `core_extra`, or global
/// `always_core`. Everything else is reachable via the `tool_use` dispatcher.
fn keep_in_native_partition(
    name: &str,
    core_names: &std::collections::HashSet<&str>,
    core_extra: &std::collections::HashSet<String>,
    always_core: &[String],
) -> bool {
    core_names.contains(name)
        || core_extra.contains(name)
        || always_core.iter().any(|n| n == name)
}
```

- [ ] **Step 4: Use it in the retain**

Replace the retain block (~706-709) — currently:

```rust
                all_tools.retain(|t| {
                    core_names.contains(t.name.as_str())
                        || core_extra.contains(&t.name)
                });
```

with:

```rust
                let always_core = deps.dispatcher_always_core();
                all_tools.retain(|t| {
                    keep_in_native_partition(&t.name, &core_names, &core_extra, always_core)
                });
```

- [ ] **Step 5: Run test to verify it passes**

Run (on server): `cargo test -p opex-core --bin opex-core native_partition_keeps_always_core`
Expected: PASS.

- [ ] **Step 6: Lint + commit**

```bash
make lint
git add crates/opex-core/src/agent/context_builder.rs
git commit -m "feat(dispatcher): promote always_core tools into native tools[] for main agents"
```

---

## Task 4: Native promotion for dispatcher-mode subagents

Dispatcher-mode subagents (`dispatch_for_subagent == true`) keep `available_tools` at static-core only. Inject the `always_core` subset (from YAML + MCP) natively, honouring `denied_for_subagent`.

**Files:**
- Modify: `crates/opex-core/src/agent/pipeline/subagent_runner.rs` (helper + injection after the `if !dispatch_for_subagent` block ~182)
- Test: inline in `crates/opex-core/src/agent/pipeline/subagent_runner.rs`

**Interfaces:**
- Consumes: `executor.cfg().app_config.tool_dispatcher.always_core` (Task 1); the in-scope `yaml_tools: Vec<YamlToolDef>`, `denied_for_subagent: Vec<String>`, `ctx.tex.mcp`.
- Produces: `fn always_core_subagent_defs(always_core: &[String], yaml: &[crate::tools::yaml_tools::YamlToolDef], mcp_defs: &[opex_types::ToolDefinition], denied: &std::collections::HashSet<&str>) -> Vec<opex_types::ToolDefinition>`.

- [ ] **Step 1: Write the failing test**

Add to (or create) a `#[cfg(test)] mod tests` block in `subagent_runner.rs`:

```rust
#[test]
fn always_core_subagent_defs_filters_denied_and_selects_subset() {
    use std::collections::HashSet;
    let mcp_defs = vec![
        opex_types::ToolDefinition { name: "sequentialthinking".into(), description: "x".into(), input_schema: serde_json::json!({}) },
        opex_types::ToolDefinition { name: "other_mcp".into(), description: "y".into(), input_schema: serde_json::json!({}) },
    ];
    let always = vec!["sequentialthinking".to_string(), "denied_tool".to_string()];
    let denied: HashSet<&str> = ["denied_tool"].into_iter().collect();

    let out = super::always_core_subagent_defs(&always, &[], &mcp_defs, &denied);
    let names: Vec<&str> = out.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, vec!["sequentialthinking"],
        "only non-denied always_core names present in any source are injected");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run (on server): `cargo test -p opex-core --bin opex-core always_core_subagent_defs_filters_denied_and_selects_subset`
Expected: FAIL to compile — `always_core_subagent_defs` not found.

- [ ] **Step 3: Add the helper**

Module-level fn in `subagent_runner.rs`:

```rust
/// The `always_core` subset to inject natively for a dispatcher-mode subagent:
/// names that (a) are in `always_core`, (b) are NOT denied for the subagent,
/// (c) exist in the YAML or MCP tool sources. YAML shadows MCP on name clash
/// (mirrors the non-dispatcher path's YAML-then-MCP order).
fn always_core_subagent_defs(
    always_core: &[String],
    yaml: &[crate::tools::yaml_tools::YamlToolDef],
    mcp_defs: &[opex_types::ToolDefinition],
    denied: &std::collections::HashSet<&str>,
) -> Vec<opex_types::ToolDefinition> {
    let mut out: Vec<opex_types::ToolDefinition> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for name in always_core {
        if denied.contains(name.as_str()) || seen.contains(name) {
            continue;
        }
        if let Some(y) = yaml.iter().find(|t| &t.name == name) {
            out.push(y.to_tool_definition());
            seen.insert(name.clone());
        } else if let Some(m) = mcp_defs.iter().find(|t| &t.name == name) {
            out.push(m.clone());
            seen.insert(name.clone());
        }
    }
    out
}
```

- [ ] **Step 4a: Stop moving `yaml_tools` in the existing block (borrow prerequisite)**

The existing `if !dispatch_for_subagent` block moves `yaml_tools` via `.into_iter()`. The borrow checker sees that move on all paths, so the new dispatcher block below cannot borrow `&yaml_tools`. Change the move to a borrow. Replace (in `run_subagent_with_session`, ~170-175):

```rust
        available_tools.extend(
            yaml_tools
                .into_iter()
                .filter(|t| !denied_set.contains(t.name.as_str()))
                .map(|t| t.to_tool_definition()),
        );
```

with:

```rust
        available_tools.extend(
            yaml_tools
                .iter()
                .filter(|t| !denied_set.contains(t.name.as_str()))
                .map(|t| t.to_tool_definition()),
        );
```

(`to_tool_definition(&self)` takes `&self`, so iterating by reference needs no `.cloned()`.)

- [ ] **Step 4b: Wire the injection**

In `run_subagent_with_session`, immediately AFTER the existing `if !dispatch_for_subagent { ... }` block and BEFORE `available_tools = executor.filter_tools_by_policy(available_tools);` (~182), add:

```rust
    if dispatch_for_subagent {
        let always_core = &executor.cfg().app_config.tool_dispatcher.always_core;
        if !always_core.is_empty() {
            let denied_set: std::collections::HashSet<&str> =
                denied_for_subagent.iter().map(String::as_str).collect();
            // MCP discovery only when there is something to promote (one call
            // per dispatcher-mode subagent spawn; the non-dispatcher path pays
            // the same cost).
            let mcp_defs = if let Some(mcp) = &ctx.tex.mcp {
                mcp.all_tool_definitions().await
            } else {
                Vec::new()
            };
            available_tools.extend(always_core_subagent_defs(
                always_core, &yaml_tools, &mcp_defs, &denied_set,
            ));
        }
    }
```

- [ ] **Step 5: Run test + compile**

Run (on server): `cargo check -p opex-core && cargo test -p opex-core --bin opex-core always_core_subagent_defs_filters_denied_and_selects_subset`
Expected: check passes; test PASS.

- [ ] **Step 6: Lint + commit**

```bash
make lint
git add crates/opex-core/src/agent/pipeline/subagent_runner.rs
git commit -m "feat(dispatcher): promote always_core tools natively for dispatcher-mode subagents"
```

---

## Task 5: F3 — warn-once on unmatched `always_core` names

Catch operator typos: an `always_core` name matching no tool in the assembled universe. Piggyback on the already-built `all_tools` in `context_builder` (system + YAML + MCP), warn at most once per process.

**Files:**
- Modify: `crates/opex-core/src/agent/context_builder.rs` (pure helper + `OnceLock`-guarded warn near the retain)
- Test: inline in `crates/opex-core/src/agent/context_builder.rs`

**Interfaces:**
- Produces: `fn unmatched_always_core(configured: &[String], known: &std::collections::HashSet<String>) -> Vec<String>`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn unmatched_always_core_reports_typos() {
    use std::collections::HashSet;
    let known: HashSet<String> = ["sequentialthinking".to_string()].into_iter().collect();
    let configured = vec!["sequentialthinking".to_string(), "sequentialthinkng".to_string()];
    assert_eq!(
        super::unmatched_always_core(&configured, &known),
        vec!["sequentialthinkng".to_string()],
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run (on server): `cargo test -p opex-core --bin opex-core unmatched_always_core_reports_typos`
Expected: FAIL to compile — fn not found.

- [ ] **Step 3: Add the helper**

Module-level fn in `context_builder.rs`:

```rust
/// `always_core` names that match no tool in `known` (the assembled tool
/// universe). Used to warn the operator about typos / absent tools.
fn unmatched_always_core(
    configured: &[String],
    known: &std::collections::HashSet<String>,
) -> Vec<String> {
    configured.iter().filter(|n| !known.contains(*n)).cloned().collect()
}
```

- [ ] **Step 4: Wire the warn-once**

Add a process-wide guard near the top of `context_builder.rs` module scope:

```rust
static ALWAYS_CORE_WARNED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
```

This edit is INSIDE the `if dispatcher_enabled { ... }` block, immediately after Task 3's `let always_core = deps.dispatcher_always_core();` line and BEFORE the `all_tools.retain(...)` call. Reuse Task 3's `always_core` binding — do NOT declare a second one. The resulting sequence is:

```rust
                let always_core = deps.dispatcher_always_core();

                // F3: warn once per process about always_core names that match
                // no tool in the assembled universe (typo / absent tool).
                if !always_core.is_empty() && ALWAYS_CORE_WARNED.get().is_none() {
                    let known: std::collections::HashSet<String> =
                        all_tools.iter().map(|t| t.name.clone()).collect();
                    let missing = unmatched_always_core(always_core, &known);
                    if !missing.is_empty() {
                        tracing::warn!(
                            missing = ?missing,
                            "[tool_dispatcher] always_core names not found in any tool source \
                             (typo or tool absent) — they will never be promoted"
                        );
                    }
                    let _ = ALWAYS_CORE_WARNED.set(());
                }

                all_tools.retain(|t| {
                    keep_in_native_partition(&t.name, &core_names, &core_extra, always_core)
                });
```

(Task 3 introduced the `let always_core` + `retain`; this step inserts the warn block between them. If Task 3 and Task 5 are implemented by separate workers, the Task 5 worker edits Task 3's output rather than adding a parallel binding.)

- [ ] **Step 5: Run test to verify it passes**

Run (on server): `cargo test -p opex-core --bin opex-core unmatched_always_core_reports_typos`
Expected: PASS.

- [ ] **Step 6: Lint + commit**

```bash
make lint
git add crates/opex-core/src/agent/context_builder.rs
git commit -m "feat(dispatcher): warn once on unmatched always_core config names"
```

---

## Task 6: Set the production value in `opex.toml`

**Files:**
- Modify: `config/opex.toml` (add `[tool_dispatcher]` section)

- [ ] **Step 1: Add the section**

Append to `config/opex.toml` (top level, not nested under another table):

```toml
[tool_dispatcher]
# Extension tools promoted into the native tools[] of every dispatcher-mode
# agent (and hidden from the tool_use catalogue). Fixes weak models writing
# these as free-form text instead of calling them.
always_core = ["sequentialthinking"]
```

- [ ] **Step 2: Verify it parses**

Run (on server): `cargo run -p opex-core --bin opex-core -- --check-config` if such a flag exists; otherwise start the binary and confirm no config-parse error in `~/opex/logs/core.log`. As a minimum, run `cargo test -p opex-core --bin opex-core global_tool_dispatcher_defaults_empty_and_parses` (already green) to confirm the schema accepts the section.

- [ ] **Step 3: Commit**

```bash
git add config/opex.toml
git commit -m "chore(config): promote sequentialthinking via [tool_dispatcher] always_core"
```

---

## Final verification (after all tasks)

- [ ] **Full check + lint + test on server**

```bash
make check
make lint
cargo test -p opex-core --bin opex-core   # full bin-target suite
```
Expected: clean build, no clippy warnings, all tests pass (the 4 new + existing `hallucinated_tool.rs` untouched and green).

- [ ] **Deploy + smoke**

```bash
make remote-deploy
make doctor
```
Then, on an agent with `[agent.tool_dispatcher] enabled = true` running the weak model, send a prompt that would use `sequentialthinking`. Confirm in `~/opex/logs/core.log` (`context_size` line) that the tool now appears in the native `tools[]` (tools_tokens rises) and the model emits a real `tool_calls` invocation rather than free-form `sequentialthinking\n{...}` text. Repeat for a spawned subagent.

- [ ] **Verify F5 (hot-reload) claim from spec**

Edit `always_core` in the deployed `opex.toml` (add a second name) WITHOUT rebuilding; check whether the config-watcher reload propagates to running engines (new name promoted on next turn) or whether a `systemctl --user restart opex-core` is required. Document the answer in the spec's Deploy section.
