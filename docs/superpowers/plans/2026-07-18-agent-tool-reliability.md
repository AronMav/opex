# Agent Tool-Reliability Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Cut agent tool-call failures (the operator's "agents often miss") by removing an ambiguous/duplicate tool surface, fixing config gaps, and pruning flaky MCP servers — validated against prod `tool_quality`.

**Architecture:** A new global `[tool_dispatcher] block` list (a pure name filter over the assembled tool universe, applied at the single tool-assembly chokepoint) plus disabling the redundant `mcp-filesystem` server removes the native-vs-MCP filesystem overlap that glm-5.2 mis-selects. Config-only fixes close two tool env/target gaps. A per-server audit disables/dedups flaky MCP servers in favour of native equivalents. A small `tool_quality` report surfaces regressions.

**Tech Stack:** Rust 2024 (opex-core), TOML config, workspace YAML MCP configs, PostgreSQL (`tool_quality`, `providers`). Deploy: `server-deploy.sh` (Rust rebuild + config sync). Spec: `docs/superpowers/specs/2026-07-18-agent-tool-reliability-design.md`.

## Global Constraints

- Rust + rustls-tls only; never add OpenSSL. (project constraint)
- No API-contract or migration breakage. (project constraint)
- Rust gate before deploy: `cargo check -p opex-core --all-targets` + `cargo clippy -p opex-core --all-targets` clean (clippy is `-D warnings`). Windows can't authoritatively run DB tests — the server is authoritative; `#[sqlx::test]` failures without a local Postgres are expected, not regressions.
- No push/deploy without explicit operator approval. Work in `master`. No `Co-Authored-By`.
- Deploy mechanics: `ssh aronmav@188.246.224.118 'bash ~/opex-src/scripts/server-deploy.sh'` rebuilds Rust + syncs toolgate/channels/migrations. It does NOT sync `workspace/` or `config/` runtime files — those are `scp`'d after diffing the server copy against repo (server copies can diverge). `rg` is absent on the server — use `grep`. `config/opex.toml` and `config/media-drivers.yaml` are read at runtime from `~/opex/config/` (opex.toml is NOT `include_str!`); `scaffold/*` and `media-drivers.yaml` ARE `include_str!` (need rebuild).
- Validation metric: `tool_quality` fail-rate (`fail_calls/total_calls`) and `penalty_score` per tool, before vs after, 2–3 day prod window.

---

## File Structure

| File | Responsibility | Change |
|---|---|---|
| `crates/opex-core/src/config/mod.rs` | `GlobalToolDispatcherConfig` struct | Add `block: Vec<String>` field (after `always_core`, ~L1825) |
| `crates/opex-core/src/agent/pipeline/dispatch.rs` | tool-policy filtering | Add pure `apply_global_block(tools, block)` + unit test |
| `crates/opex-core/src/agent/context_builder.rs` | tool-assembly trait | Add `fn dispatcher_block(&self) -> &[String];` (near `dispatcher_always_core`, ~L232); apply block after L702 |
| `crates/opex-core/src/agent/engine/context_builder.rs` | trait impl for `AgentEngine` | Impl `dispatcher_block()` (near `dispatcher_always_core` impl, ~L675) |
| `config/opex.toml` | global dispatcher config | Add `block = [...]` under `[tool_dispatcher]` |
| `workspace/mcp/filesystem.yaml` | MCP-filesystem server | `enabled: false` |
| `workspace/mcp/{fetch,…}.yaml` | flaky MCP servers | `enabled: false` per audit (Batch 3) |
| `config/agents/*.toml` | per-agent tool policy | `process_start` → `process` normalize (Batch 2) |

---

# BATCH 1 — Filesystem tool de-duplication + global `block`

Primary lever. Rust `block` mechanism (Task 1.1) then config activation + deploy (Task 1.2).

## Task 1.1: Global `block` config + pure filter + wiring

**Files:**
- Modify: `crates/opex-core/src/config/mod.rs` (~L1818-1826, `GlobalToolDispatcherConfig`)
- Modify: `crates/opex-core/src/agent/pipeline/dispatch.rs` (add pure fn + test)
- Modify: `crates/opex-core/src/agent/context_builder.rs` (trait method decl ~L232 + apply after L702)
- Modify: `crates/opex-core/src/agent/engine/context_builder.rs` (trait impl ~L675)

**Interfaces:**
- Produces: `config::GlobalToolDispatcherConfig.block: Vec<String>`; `dispatch::apply_global_block(tools: Vec<ToolDefinition>, block: &[String]) -> Vec<ToolDefinition>`; trait method `dispatcher_block(&self) -> &[String]`.
- Consumes: existing `dispatcher_always_core()` pattern (mirror it exactly).

- [ ] **Step 1: Write the failing test** in `crates/opex-core/src/agent/pipeline/dispatch.rs` (in the existing `#[cfg(test)] mod tests`):

```rust
#[test]
fn apply_global_block_removes_only_listed_names() {
    let mk = |n: &str| ToolDefinition {
        name: n.to_string(),
        description: String::new(),
        input_schema: serde_json::json!({}),
    };
    let tools = vec![mk("read_file"), mk("workspace_read"), mk("write_file"), mk("code_exec")];
    let block = vec!["read_file".to_string(), "write_file".to_string()];
    let out = apply_global_block(tools, &block);
    let names: Vec<&str> = out.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, vec!["workspace_read", "code_exec"]);
}

#[test]
fn apply_global_block_empty_is_noop() {
    let mk = |n: &str| ToolDefinition {
        name: n.to_string(), description: String::new(), input_schema: serde_json::json!({}),
    };
    let tools = vec![mk("read_file"), mk("workspace_read")];
    let out = apply_global_block(tools, &[]);
    assert_eq!(out.len(), 2);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p opex-core --bins apply_global_block 2>&1 | grep -E 'error|test result'`
Expected: compile error `cannot find function 'apply_global_block'`.

- [ ] **Step 3: Implement the pure filter** in `crates/opex-core/src/agent/pipeline/dispatch.rs` (below `filter_tools_by_policy`):

```rust
/// Remove tools whose name is in the global `[tool_dispatcher] block` list.
/// A pure name filter over the fully-assembled tool universe — the single
/// global place to drop a tool from EVERY agent's schema (base and non-base),
/// e.g. redundant MCP duplicates of native tools. Order-independent removal.
pub fn apply_global_block(tools: Vec<ToolDefinition>, block: &[String]) -> Vec<ToolDefinition> {
    if block.is_empty() {
        return tools;
    }
    tools.into_iter().filter(|t| !block.iter().any(|b| b == &t.name)).collect()
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p opex-core --bins apply_global_block 2>&1 | grep 'test result'`
Expected: `test result: ok. 2 passed`.

- [ ] **Step 5: Add the config field** in `crates/opex-core/src/config/mod.rs` — inside `GlobalToolDispatcherConfig`, right after the `always_core` field (~L1825):

```rust
    /// Tool names globally removed from EVERY agent's schema (base and non-base),
    /// applied to the fully-assembled tool universe before dispatcher partition.
    /// The single global place to drop redundant/duplicate tools (e.g. MCP
    /// filesystem duplicates of native `workspace_*`). Empty by default.
    #[serde(default)]
    pub block: Vec<String>,
```

- [ ] **Step 6: Add the trait method decl** in `crates/opex-core/src/agent/context_builder.rs`, right after the `dispatcher_always_core` decl (~L232):

```rust
    /// Global `[tool_dispatcher] block` list — tool names removed from every
    /// agent's assembled schema. Mirrors `dispatcher_always_core`.
    fn dispatcher_block(&self) -> &[String];
```

- [ ] **Step 7: Implement the trait method** in `crates/opex-core/src/agent/engine/context_builder.rs`, right after the `dispatcher_always_core` impl (~L675-677):

```rust
    fn dispatcher_block(&self) -> &[String] {
        &self.cfg().app_config.tool_dispatcher.block
    }
```

- [ ] **Step 8: Apply the block at the assembly chokepoint** in `crates/opex-core/src/agent/context_builder.rs`, immediately after `let mut all_tools = deps.filter_tools_by_policy(tool_list);` (L702):

```rust
            let mut all_tools = deps.filter_tools_by_policy(tool_list);
            // Global dedup: drop tools listed in [tool_dispatcher] block from
            // EVERY agent (before dispatcher partition / top-K). See A-tool-rel design.
            all_tools = crate::agent::pipeline::dispatch::apply_global_block(
                all_tools, deps.dispatcher_block(),
            );
```

- [ ] **Step 9: Verify any other `ContextDeps` impls/mocks compile** (the trait gained a method). Search for impls:

Run: `grep -rn 'dispatcher_always_core' crates/opex-core/src | grep -v 'context_builder.rs'`
Expected: no other impl sites (only the one in `engine/context_builder.rs`). If a test mock impls the trait, add `fn dispatcher_block(&self) -> &[String] { &[] }` to it.

- [ ] **Step 10: Full gate**

Run: `cargo check -p opex-core --all-targets 2>&1 | tail -3` → `Finished`.
Run: `cargo clippy -p opex-core --all-targets 2>&1 | grep -iE 'warning|error' | head` → empty.

- [ ] **Step 11: Commit**

```bash
git add crates/opex-core/src/config/mod.rs crates/opex-core/src/agent/pipeline/dispatch.rs crates/opex-core/src/agent/context_builder.rs crates/opex-core/src/agent/engine/context_builder.rs
git commit -m "feat(tools): global [tool_dispatcher] block list to dedup agent tool surface"
```

## Task 1.2: Activate — disable mcp-filesystem, set block, deploy, measure

**Files:**
- Modify: `workspace/mcp/filesystem.yaml` (`enabled: true` → `false`)
- Modify: `config/opex.toml` (`[tool_dispatcher]` add `block`)

- [ ] **Step 1: Disable mcp-filesystem** in `workspace/mcp/filesystem.yaml`:

```yaml
enabled: false
```

- [ ] **Step 2: Add the block list** in `config/opex.toml` under `[tool_dispatcher]` (next to `always_core`):

```toml
block = ["read_file", "write_file", "edit_file", "list_directory", "search_files"]
```

- [ ] **Step 3: Verify opex.toml parses** (it's read at runtime, not embedded):

Run: `cargo test -p opex-core --bins tool_dispatcher 2>&1 | grep 'test result'`
Expected: existing `[tool_dispatcher]` parse tests pass (they exercise the struct; `block` defaults to `[]` when absent and parses when present).

- [ ] **Step 4: Commit**

```bash
git add workspace/mcp/filesystem.yaml config/opex.toml
git commit -m "config: disable mcp-filesystem, block its 5 tools globally (native workspace_* covers it)"
```

- [ ] **Step 5: Deploy (ONLY after operator approval to push)** — Rust rebuild (Task 1.1 code) + sync the two config files.

```bash
git push origin master
ssh aronmav@188.246.224.118 'bash ~/opex-src/scripts/server-deploy.sh'
# server-deploy.sh does NOT sync workspace/ or config/ runtime files — scp them:
# First diff the server copy vs repo to confirm no divergence, then overwrite.
ssh aronmav@188.246.224.118 'cat ~/opex/config/opex.toml' > /tmp/srv_toml
diff <(git show HEAD:config/opex.toml) /tmp/srv_toml   # expect only the block= line differs (server=old)
scp config/opex.toml aronmav@188.246.224.118:~/opex/config/opex.toml
scp workspace/mcp/filesystem.yaml aronmav@188.246.224.118:~/opex/workspace/mcp/filesystem.yaml
ssh aronmav@188.246.224.118 'systemctl --user restart opex-core'
```

- [ ] **Step 6: Smoke**

```bash
ssh aronmav@188.246.224.118 'curl -sf http://localhost:18789/health; echo; for s in opex-core; do systemctl --user show $s -p NRestarts --value; done'
# Expect: {"status":"ok",...}, NRestarts 0
# Confirm the filesystem tools are gone from a base agent schema:
ssh aronmav@188.246.224.118 "TOKEN=\$(grep -oP 'OPEX_AUTH_TOKEN=\K.*' ~/opex/.env|tr -d '\"'|head -1); curl -sf -H \"Authorization: Bearer \$TOKEN\" http://localhost:18789/api/tool-definitions | grep -o read_file || echo 'read_file absent (good)'"
```
Expected: `read_file absent (good)`; native `workspace_read` still present.

- [ ] **Step 7: Record baseline for the metric** (so Batch's effect is measurable):

```bash
ssh aronmav@188.246.224.118 "docker exec docker-postgres-1 psql -U opex -d opex -c \"SELECT tool_name, sum(total_calls) tc, sum(fail_calls) fc FROM tool_quality WHERE tool_name IN ('read_file','write_file','edit_file','list_directory','search_files') GROUP BY tool_name;\""
```
Note the counts; after 2–3 days these tools should show no NEW calls (gone from schema).

---

# BATCH 2 — Config gaps (config-only, no Rust rebuild)

Independent of Batch 1. Each task verifies the gap on the server first, then fixes.

## Task 2.1: `core_get_skills_repairs` missing `OPEX_AUTH_TOKEN`

**Files:** MCP server config that provides `core_get_skills_repairs` (identify in Step 1), likely `workspace/mcp/<server>.yaml` or its container env in `docker/docker-compose.yml`.

- [ ] **Step 1: Locate the tool + its server** on the server:

```bash
ssh aronmav@188.246.224.118 "grep -rl core_get_skills_repairs ~/opex/workspace/mcp/ ~/opex/docker/ 2>/dev/null; docker exec docker-postgres-1 psql -U opex -d opex -tAc \"SELECT tool_name,left(last_error,120) FROM tool_quality WHERE tool_name='core_get_skills_repairs';\""
```
Expected: identifies the MCP server (a core-callback MCP) and confirms `env var 'OPEX_AUTH_TOKEN'` in `last_error`.

- [ ] **Step 2: Inject the token** into that server's environment. If it's a docker MCP, add to its `environment:` in `docker/docker-compose.yml`:

```yaml
    environment:
      - OPEX_AUTH_TOKEN=${OPEX_AUTH_TOKEN}
```
(the value is already in the host env / core `.env`). If it's a native/yaml tool, add the env passthrough where that server is spawned.

- [ ] **Step 3: Apply on server + restart that server only:**

```bash
scp docker/docker-compose.yml aronmav@188.246.224.118:~/opex/docker/docker-compose.yml
ssh aronmav@188.246.224.118 'cd ~/opex/docker && docker compose up -d <server-name>'
```

- [ ] **Step 4: Verify** the tool now authenticates (call it via an agent turn or check next `tool_quality` update shows a success).

- [ ] **Step 5: Commit**

```bash
git add docker/docker-compose.yml
git commit -m "fix(mcp): inject OPEX_AUTH_TOKEN for core-callback MCP (core_get_skills_repairs)"
```

## Task 2.2: `query_db` targets wrong relation ("agents")

**Files:** the `query_db` tool config (identify in Step 1 — YAML tool `workspace/tools/query_db.yaml` or an MCP-postgres server).

- [ ] **Step 1: Locate + inspect** the tool's DB target:

```bash
ssh aronmav@188.246.224.118 "grep -rn 'query_db' ~/opex/workspace/tools/ ~/opex/workspace/mcp/ 2>/dev/null; docker exec docker-postgres-1 psql -U opex -d opex -tAc \"SELECT left(last_error,140) FROM tool_quality WHERE tool_name='query_db' ORDER BY last_call_at DESC LIMIT 1;\""
```
Expected: shows the DSN/database and the `relation "agents" does not exist` error → the tool points at a DB/schema without the `agents` table (agents live in config TOMLs, not a DB table — so either the query is wrong or the DSN is wrong).

- [ ] **Step 2: Decide the fix** from Step 1's evidence: (a) if the DSN points at the wrong database → correct it to the opex DB; (b) if the intended target has no `agents` relation by design → this tool's contract is wrong; disable it (`enabled:false` / remove) rather than leave a permanently-failing tool. Record the decision inline in the commit message.

- [ ] **Step 3: Apply** the chosen fix (edit the yaml, `scp` to server, no restart needed for YAML tools — read per-request).

- [ ] **Step 4: Verify** next call succeeds or the tool is gone from the schema.

- [ ] **Step 5: Commit** with the decision recorded.

## Task 2.3: Normalize stale `process_start` in agent deny-lists

**Files:** `config/agents/*.toml` (non-base agents: Alma, Aria, Arty, Tyler — deny-lists contain phantom `process_start`).

- [ ] **Step 1: Confirm the stale entries:**

Run: `grep -rn 'process_start' config/agents/`
Expected: `process_start` in non-base deny-lists.

- [ ] **Step 2: Replace** `process_start` → `process` in each (the real tool name; harmless for non-base but correct):

Run: `sed -i 's/"process_start"/"process"/' config/agents/*.toml` (verify with `grep -rn 'process' config/agents/`).

- [ ] **Step 3: Sync to server** (config watcher watches `opex.toml` only, not agent TOMLs — a PUT or restart is needed; scp then restart core, OR PUT each agent via API). Simplest:

```bash
for f in Alma Aria Arty Tyler; do scp config/agents/$f.toml aronmav@188.246.224.118:~/opex/config/agents/$f.toml; done
ssh aronmav@188.246.224.118 'systemctl --user restart opex-core'
```

- [ ] **Step 4: Commit**

```bash
git add config/agents/*.toml
git commit -m "config: normalize phantom process_start -> process in agent deny-lists"
```

---

# BATCH 3 — Flaky MCP audit (verify-then-disable)

Each server: confirm a native equivalent covers it AND it's chronically failing, THEN disable. Never disable blind.

## Task 3.1: `fetch` MCP (45/51 fail) vs native `web_fetch`

- [ ] **Step 1: Confirm native `web_fetch` is available to the failing agents** and covers the use:

```bash
ssh aronmav@188.246.224.118 "TOKEN=\$(grep -oP 'OPEX_AUTH_TOKEN=\K.*' ~/opex/.env|tr -d '\"'|head -1); curl -sf -H \"Authorization: Bearer \$TOKEN\" http://localhost:18789/api/tool-definitions | grep -o web_fetch"
```
Expected: `web_fetch` present. (Design already verified it's not in any non-base deny-list.)

- [ ] **Step 2: Disable the MCP `fetch` server** + block its tool name:
  - `workspace/mcp/fetch.yaml` → `enabled: false`
  - add `"fetch"` to `config/opex.toml` `[tool_dispatcher] block`.

- [ ] **Step 3: Sync + restart:** `scp` both files to server, `systemctl --user restart opex-core`.

- [ ] **Step 4: Verify** `fetch` absent from schema, `web_fetch` present; a fetch-style task now routes to `web_fetch`.

- [ ] **Step 5: Commit** `config: disable flaky MCP fetch in favour of native web_fetch`.

## Task 3.2: `get_transcript` MCP (process timeout)

- [ ] **Step 1: Confirm the native path** — transcription is a toolgate handler (`transcribe`/`summarize_video`), not this MCP. Verify the handler works:

```bash
ssh aronmav@188.246.224.118 'curl -sf http://localhost:9011/health && echo toolgate-ok'
```

- [ ] **Step 2: Verify no unique dependency** on `get_transcript` (grep skills/agents for it):

```bash
ssh aronmav@188.246.224.118 "grep -rln get_transcript ~/opex/workspace/skills/ ~/opex/config/skills/ 2>/dev/null || echo 'no skill depends on it'"
```

- [ ] **Step 3: If no dependency → disable** its MCP server (`enabled:false`) + block the tool name. If a skill depends on it → repoint the skill to the toolgate handler instead (record which skill).

- [ ] **Step 4: Sync + restart + verify + commit.**

## Task 3.3: MCP browser (`browser_navigate`/`browser_evaluate`) vs native `browser_action`

- [ ] **Step 1: Confirm native `browser_action` reachable** (A8 fix pointed it at localhost:9020):

```bash
ssh aronmav@188.246.224.118 'curl -sf -o /dev/null -w "%{http_code}\n" -X POST http://localhost:9020/automation -H "Content-Type: application/json" -d "{\"action\":\"create_session\"}"'
```
Expected: `200`.

- [ ] **Step 2: Decide** — if the MCP browser server (browser-cdp) duplicates `browser_action`, add `browser_navigate`/`browser_evaluate` (and siblings) to the global `block`, keeping native `browser_action`. If the MCP browser offers something native doesn't (e.g. CDP-specific ops an agent relies on), keep it and instead note the overlap. Base the decision on `tool_quality` usage + skill references (`grep -rln browser_navigate ~/opex/workspace/skills/`).

- [ ] **Step 3: Apply chosen block, sync, restart, verify, commit.**

---

# BATCH 4 — Visibility: `tool_quality` degradation report

Surface degrading tools so regressions are caught by data, not anecdote.

## Task 4.1: Degradation report endpoint

**Files:**
- Modify: `crates/opex-core/src/gateway/handlers/monitoring/mod.rs` (add route) + a handler (new fn in an existing monitoring handler file, e.g. `monitoring/usage.rs` or a new `monitoring/tool_health.rs`).

**Interfaces:**
- Produces: `GET /api/tools/health` → JSON list of `{agent_name, tool_name, total_calls, fail_calls, fail_rate, penalty_score, last_error}` for tools with `fail_calls > 0`, ordered by `fail_rate * total_calls` desc (worst impact first).

- [ ] **Step 1: Write the failing test** (handler-level, in the new handler file's `#[cfg(test)]`): assert the SQL query string selects the expected columns and orders by impact. (Full HTTP test needs DB — gated on server; a string/shape unit test is the CI-safe check.)

```rust
#[test]
fn tool_health_query_selects_expected_columns() {
    let q = tool_health_query();
    for col in ["agent_name","tool_name","total_calls","fail_calls","penalty_score","last_error"] {
        assert!(q.contains(col), "missing {col}");
    }
    assert!(q.contains("fail_calls > 0"));
}
```

- [ ] **Step 2: Run — fails** (`tool_health_query` undefined). `cargo test -p opex-core --bins tool_health_query`.

- [ ] **Step 3: Implement** the query fn + handler + row struct (mirror an existing monitoring handler, e.g. `api_list_failures` in `session_failures.rs` for the shape). `tool_health_query()` returns the `SELECT ... FROM tool_quality WHERE fail_calls > 0 ORDER BY (fail_calls::float/NULLIF(total_calls,0)) * total_calls DESC` string; the handler runs it and returns JSON. Register `GET /api/tools/health` in `monitoring/mod.rs::routes()`.

- [ ] **Step 4: Run — passes.** Full gate (`cargo check --all-targets` + clippy).

- [ ] **Step 5: Commit** `feat(monitoring): GET /api/tools/health — tool_quality degradation report`.

- [ ] **Step 6: Deploy (operator approval) + smoke:**

```bash
git push origin master && ssh aronmav@188.246.224.118 'bash ~/opex-src/scripts/server-deploy.sh'
ssh aronmav@188.246.224.118 "TOKEN=\$(grep -oP 'OPEX_AUTH_TOKEN=\K.*' ~/opex/.env|tr -d '\"'|head -1); curl -sf -H \"Authorization: Bearer \$TOKEN\" http://localhost:18789/api/tools/health | head -c 300"
```
Expected: JSON list of degrading tools.

---

# Final validation (after 2–3 day prod window)

- [ ] Re-run the Batch 1 baseline query — the 5 filesystem tools show no NEW calls.
- [ ] `GET /api/tools/health` — filesystem-family and `fetch` no longer in the degraded list; overall fail-rate down.
- [ ] Optional (Section 4 prompt note, DEFERRED): only if data still shows systematic mis-selection, add a short tool-selection note to `crates/opex-core/scaffold/base/*.md` — separate follow-up, not in this plan.
