# Agent Tool-Reliability Implementation Plan (rev.2 — post-review)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

> **rev.2:** rewritten after a 3-reviewer verification pass (Batch-1 Rust anchors; Batches 2-3 server reality; Batch-4 + spec coverage + risks). All review findings are baked in; the biggest corrections: server `opex.toml` is NEVER scp'd (targeted append only), Task 2.1/2.2 re-diagnosed (reserved-secret guard / not-a-config-gap), `block` semantics honestly scoped, a write-path audit gates the mcp-filesystem disable, Batch 4 extends `get_degraded_tools` instead of duplicating it.

**Goal:** Cut agent tool-call failures (the operator's "agents often miss") by removing an ambiguous/duplicate tool surface, correcting broken server-side tools, and pruning overlapping MCP servers — validated against prod `tool_quality` deltas.

**Architecture:** A new global `[tool_dispatcher] block` list — a pure name filter applied at the main tool-assembly chokepoint — plus disabling the redundant `mcp-filesystem` server removes the native-vs-MCP filesystem overlap that glm-5.2 mis-selects. **`block` scope (verified):** it filters only the MAIN agent's native `tools[]` schema; the dispatcher catalogue (`tool_use`), trigger-hint, suppressor, the subagent assembly path, and the openai-compat path all bypass it. For MCP tools the actual guarantee is the server's `enabled: false` — the MCP registry is the single source feeding every assembly path, so disabling the server removes the tools everywhere. **Rule: blocking an MCP tool ALWAYS pairs with disabling its server; `block` alone is only valid for non-MCP names.** Server-side corrections remove a by-construction-broken YAML tool and normalize stale deny-lists; a per-server audit dedups overlapping MCP servers in favour of native equivalents; the existing `tool_quality` degradation query gains an operator endpoint.

**Tech Stack:** Rust 2024 (opex-core), TOML config, workspace YAML MCP configs, PostgreSQL (`tool_quality`, `messages`). Deploy: `server-deploy.sh` (Rust rebuild). Spec: `docs/superpowers/specs/2026-07-18-agent-tool-reliability-design.md`.

## Global Constraints

- Rust + rustls-tls only; never add OpenSSL. No API-contract or migration breakage.
- Rust gate before deploy: `cargo check -p opex-core --all-targets` + `cargo clippy -p opex-core --all-targets` clean. `#[sqlx::test]` tests need `DATABASE_URL` (server / `make test-db`); failing locally without it is expected.
- No push/deploy without explicit operator approval. Work in `master`. No `Co-Authored-By`.
- **Server config files DIVERGE from repo — NEVER blind-scp them (review BLOCKER B1):**
  - `~/opex/config/opex.toml` (134 lines) carries prod-only sections (`[typing]`, `[otel]`, `[curator]`, `[lsp]`) absent from the repo copy. Targeted in-place edits only. `[tool_dispatcher]` is currently its LAST section (lines 133-134) — verify before appending.
  - `~/opex/docker/docker-compose.yml`: `tts-silero` is intentionally commented out on the server (active TTS = qwen3 on the GPU PC); repo copy has it active. Never scp compose repo→server without backporting first.
  - `config/agents/*.toml` exist ONLY on the server (repo tracks just `.gitkeep`).
  - `~/opex/workspace/mcp/` is a server-curated set (differs from repo).
- `rg` is absent on the server — use `grep`. Server restarts: `systemctl --user restart opex-core`. Config watcher watches `opex.toml` only (agent TOMLs need a restart or PUT).
- Validation metric: **deltas** of `tool_quality.fail_calls`/`total_calls` over the window (the counters are cumulative and never truncated; lifetime ratios are diluted by history — review m2). Baseline snapshot before, re-snapshot after 2-3 days.

---

## File Structure

| File | Responsibility | Change |
|---|---|---|
| `crates/opex-core/src/config/mod.rs` | `GlobalToolDispatcherConfig` (L1818) | Add `block: Vec<String>` after `always_core` (L1825); extend parse test (L3782) |
| `crates/opex-core/src/agent/pipeline/dispatch.rs` | tool-policy filtering | Add pure `apply_global_block` + unit tests (tests mod at L227) |
| `crates/opex-core/src/agent/context_builder.rs` | `ContextBuilderDeps` trait (L96) | Add `dispatcher_block()` decl after L232; apply block after L702 |
| `crates/opex-core/src/agent/engine/context_builder.rs` | trait impl (single impl site, L207) | Impl `dispatcher_block()` next to L675 |
| `crates/opex-core/src/db/tool_quality.rs` | tool_quality queries (has `get_degraded_tools`) | Add `get_tool_health()` + `#[sqlx::test]` (pattern at L236) |
| `crates/opex-core/src/gateway/handlers/monitoring/mod.rs` | monitoring routes | Register `GET /api/tools/health` |
| `config/opex.toml` (repo) | reference config | Add `block = [...]` under `[tool_dispatcher]` (for history/fresh installs) |
| `workspace/mcp/filesystem.yaml` (repo) | mcp-filesystem | `enabled: false` (server copy is md5-identical — safe to mirror) |
| Server-only: `~/opex/config/opex.toml`, `~/opex/workspace/mcp/*.yaml`, `~/opex/config/agents/*.toml`, `~/opex/workspace/tools/core_get_skills_repairs.yaml` | prod runtime config | targeted in-place edits (ssh sed/append), no scp of diverged files |

---

# BATCH 1 — Filesystem tool de-duplication + global `block` (+ Batch 4 code in the same deploy)

Primary lever. One Rust deploy carries Task 1.1 AND Task 4.1 (both are opex-core code — review m3: merging deploys also puts the measurement endpoint live for the before/after window).

## Task 1.1: Global `block` config + pure filter + wiring

**Files:**
- Modify: `crates/opex-core/src/config/mod.rs` (struct L1818-1826; test L3782-3808)
- Modify: `crates/opex-core/src/agent/pipeline/dispatch.rs` (pure fn + tests; tests mod at L227 has `use super::*`)
- Modify: `crates/opex-core/src/agent/context_builder.rs` (trait decl after L232; apply after L702)
- Modify: `crates/opex-core/src/agent/engine/context_builder.rs` (impl next to L675)

**Interfaces:**
- Produces: `config::GlobalToolDispatcherConfig.block: Vec<String>`; `dispatch::apply_global_block(tools: Vec<ToolDefinition>, block: &[String]) -> Vec<ToolDefinition>`; trait method `dispatcher_block(&self) -> &[String]` on `ContextBuilderDeps`.
- Consumes: existing `dispatcher_always_core()` pattern — impl path is verbatim `&self.cfg().app_config.tool_dispatcher.<field>` (verified against the existing impl at `engine/context_builder.rs:675-677`).

- [ ] **Step 1: Write the failing tests** in `crates/opex-core/src/agent/pipeline/dispatch.rs`, inside the existing `#[cfg(test)] mod tests` (L227, already has `use super::*`):

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

- [ ] **Step 2: Run to verify FAIL**

Run: `cargo test -p opex-core --bins apply_global_block 2>&1 | grep -E 'error|test result'`
Expected: compile error `cannot find function 'apply_global_block'`.

- [ ] **Step 3: Implement the pure filter** in `dispatch.rs` below `filter_tools_by_policy`, with the HONEST scope contract (review M1 — do not overclaim):

```rust
/// Remove tools whose name is in the global `[tool_dispatcher] block` list.
///
/// SCOPE (important): this filters the MAIN agent's native `tools[]` schema at
/// the context_builder assembly chokepoint — for every agent, base and
/// non-base. It does NOT filter the dispatcher catalogue (`tool_use`
/// search/describe/call), the trigger-hint, the suppressor, the subagent
/// assembly path, or the openai-compat path. Therefore an MCP tool name in
/// this list MUST always be paired with `enabled: false` on its MCP server —
/// the registry feeds all assembly paths, so disabling the server is what
/// actually removes the tool everywhere. `block` alone is only sufficient for
/// non-MCP (native/yaml) names.
pub fn apply_global_block(tools: Vec<ToolDefinition>, block: &[String]) -> Vec<ToolDefinition> {
    if block.is_empty() {
        return tools;
    }
    tools.into_iter().filter(|t| !block.iter().any(|b| b == &t.name)).collect()
}
```

- [ ] **Step 4: Run to verify PASS**

Run: `cargo test -p opex-core --bins apply_global_block 2>&1 | grep 'test result'`
Expected: `test result: ok. 2 passed`.

- [ ] **Step 5: Add the config field AND extend the parse test** (review m5) in `crates/opex-core/src/config/mod.rs`.

(a) Inside `GlobalToolDispatcherConfig`, right after `always_core` (L1825):

```rust
    /// Tool names removed from every MAIN agent's native tools[] schema at the
    /// assembly chokepoint (base and non-base). Does NOT cover the dispatcher
    /// catalogue / subagents / openai-compat — MCP names here must be paired
    /// with the server's `enabled: false` (see `apply_global_block`). Empty by
    /// default (no behaviour change).
    #[serde(default)]
    pub block: Vec<String>,
```

(b) In the existing test `global_tool_dispatcher_defaults_empty_and_parses` (L3782-3808): add to the cfg2 TOML literal, under `always_core = ["sequentialthinking"]`:

```toml
block = ["read_file"]
```

and extend the asserts:

```rust
assert!(cfg.tool_dispatcher.block.is_empty());
assert_eq!(cfg2.tool_dispatcher.block, vec!["read_file".to_string()]);
```

- [ ] **Step 6: Add the trait method decl** in `agent/context_builder.rs`, right after the `dispatcher_always_core` decl (L232):

```rust
    /// Global `[tool_dispatcher] block` list — tool names removed from the
    /// main agent's assembled native schema. Mirrors `dispatcher_always_core`.
    fn dispatcher_block(&self) -> &[String];
```

- [ ] **Step 7: Implement the trait method** in `agent/engine/context_builder.rs`, right after the `dispatcher_always_core` impl (L675-677):

```rust
    fn dispatcher_block(&self) -> &[String] {
        &self.cfg().app_config.tool_dispatcher.block
    }
```

- [ ] **Step 8: Apply the block at the chokepoint** in `agent/context_builder.rs`, immediately after `let mut all_tools = deps.filter_tools_by_policy(tool_list);` (L702):

```rust
            // Global dedup: drop [tool_dispatcher] block names from the native
            // schema (before dispatcher partition / top-K). MCP names in the
            // list are additionally removed everywhere by their server's
            // enabled:false — see apply_global_block's scope contract.
            all_tools = crate::agent::pipeline::dispatch::apply_global_block(
                all_tools, deps.dispatcher_block(),
            );
```

- [ ] **Step 9: Verify the single impl site** (verified in review: `ContextBuilderDeps` has exactly one impl, `engine/context_builder.rs:207`; `MockContextBuilder` implements a different trait and won't break):

Run: `grep -rn 'dispatcher_always_core' crates/opex-core/src | grep -v 'context_builder.rs'`
Expected: empty (no other impl sites). If a future mock impls the trait, it must add `fn dispatcher_block(&self) -> &[String] { &[] }`.

- [ ] **Step 10: Full gate**

Run: `cargo check -p opex-core --all-targets 2>&1 | tail -3` → `Finished`.
Run: `cargo clippy -p opex-core --all-targets 2>&1 | grep -iE 'warning|error' | head` → empty.

- [ ] **Step 11: Commit**

```bash
git add crates/opex-core/src/config/mod.rs crates/opex-core/src/agent/pipeline/dispatch.rs crates/opex-core/src/agent/context_builder.rs crates/opex-core/src/agent/engine/context_builder.rs
git commit -m "feat(tools): global [tool_dispatcher] block for native-schema dedup (paired with MCP enabled:false)"
```

## Task 1.2: Activate — write-path audit, disable mcp-filesystem, set block, deploy, measure

**Files:**
- Modify (repo): `workspace/mcp/filesystem.yaml`, `config/opex.toml`
- Modify (server, targeted): `~/opex/config/opex.toml` (append), `~/opex/workspace/mcp/filesystem.yaml` (sed)

- [ ] **Step 0: SPEC OPEN-ITEM GATE (review M2) — audit where successful MCP writes went.** Prod data shows Arty succeeded via MCP `write_file` 5/12 and `edit_file` 16/28 — someone DID write successfully. `tool_quality` stores no args; extract them from message history:

```bash
ssh aronmav@188.246.224.118 "docker exec docker-postgres-1 psql -U opex -d opex -c \"SELECT created_at, agent_id, left(tool_calls::text, 400) FROM messages WHERE tool_calls::text LIKE '%write_file%' OR tool_calls::text LIKE '%edit_file%' ORDER BY created_at DESC LIMIT 30;\""
```

Classify every successful call's `path` argument against the native `workspace_write` jail for that agent (`agents/{name}/…` + shared dirs `tools/skills/mcp/uploads` + shared root files):
- ALL inside the jail → no capability loss; proceed.
- ANY outside (e.g. Arty writing into another agent's dir or an arbitrary /workspace path) → **STOP: present the path list to the operator** and get an explicit decision (per spec: such a workflow needs a purpose-built mechanism — a shared drop-dir or explicit cross-agent tool — NOT unrestricted MCP-filesystem) BEFORE proceeding.

- [ ] **Step 1: Repo edits.** `workspace/mcp/filesystem.yaml` → `enabled: false`. `config/opex.toml` `[tool_dispatcher]` → add:

```toml
block = ["read_file", "write_file", "edit_file", "list_directory", "search_files"]
```

- [ ] **Step 2: Verify parse test covers it**

Run: `cargo test -p opex-core --bins global_tool_dispatcher 2>&1 | grep 'test result'`
Expected: pass — the test extended in Task 1.1 Step 5 asserts `block` parses.

- [ ] **Step 3: Commit**

```bash
git add workspace/mcp/filesystem.yaml config/opex.toml
git commit -m "config: disable mcp-filesystem, block its 5 tools in native schema (native workspace_* covers reads; write jail is intended)"
```

- [ ] **Step 4: Deploy (ONLY after operator approval to push).** Single Rust deploy for Tasks 1.1 + 4.1; then TARGETED server config edits (review B1 — never scp opex.toml: the server copy has prod-only sections `[typing]`/`[otel]`/`[curator]`/`[lsp]`):

```bash
git push origin master
ssh aronmav@188.246.224.118 'bash ~/opex-src/scripts/server-deploy.sh'
# 1) Confirm [tool_dispatcher] is still the LAST section of the server opex.toml:
ssh aronmav@188.246.224.118 'tail -5 ~/opex/config/opex.toml'
# 2) Append the block line (safe because the section is last; if it is no longer
#    last, insert after the always_core line with sed instead):
ssh aronmav@188.246.224.118 "printf 'block = [\"read_file\", \"write_file\", \"edit_file\", \"list_directory\", \"search_files\"]\n' >> ~/opex/config/opex.toml && grep -n 'block' ~/opex/config/opex.toml"
# 3) Disable the server's mcp-filesystem (server copy verified md5-identical to repo):
ssh aronmav@188.246.224.118 "sed -i 's/^enabled: true/enabled: false/' ~/opex/workspace/mcp/filesystem.yaml && cat ~/opex/workspace/mcp/filesystem.yaml"
ssh aronmav@188.246.224.118 'systemctl --user restart opex-core'
```

- [ ] **Step 5: Smoke** (NB, review m4: `/api/tool-definitions` assembles system+yaml+MCP directly and BYPASSES the chokepoint — `read_file` absent there proves the server-disable worked; the `block` mechanism itself is proven by the unit tests, not this smoke):

```bash
ssh aronmav@188.246.224.118 'curl -sf http://localhost:18789/health; echo; systemctl --user show opex-core -p NRestarts --value'
ssh aronmav@188.246.224.118 "TOKEN=\$(grep -oP 'OPEX_AUTH_TOKEN=\K.*' ~/opex/.env|tr -d '\"'|head -1); curl -sf -H \"Authorization: Bearer \$TOKEN\" http://localhost:18789/api/tool-definitions | grep -o read_file || echo 'read_file absent (server-disable OK)'"
```

Expected: health ok, NRestarts 0, `read_file absent (server-disable OK)`, native `workspace_read` still present.

- [ ] **Step 6: Baseline snapshot for the DELTA metric** (review m2 — counters are cumulative; compare deltas, not lifetime ratios):

```bash
ssh aronmav@188.246.224.118 "docker exec docker-postgres-1 psql -U opex -d opex -c \"SELECT agent_name, tool_name, total_calls, fail_calls FROM tool_quality ORDER BY tool_name, agent_name;\"" > /tmp/tool_quality_baseline_$(date +%Y%m%d).txt
```

Success criterion after 2-3 days: zero delta on the 5 filesystem tools (no new calls — gone from every schema), and shrinking fail deltas overall.

---

# BATCH 2 — Server-side corrections (no Rust rebuild; server-only files)

Review re-diagnosed all three tasks. Server-side edits only; nothing here is committed to the repo except Task 2.2's optional description.

## Task 2.1: `core_get_skills_repairs` — by-construction broken YAML tool (review B2)

**Reality (verified):** NOT a docker MCP. It is an agent-created YAML tool that exists ONLY on the server — `~/opex/workspace/tools/core_get_skills_repairs.yaml` (`created_by: agent`), calling core `GET /api/skills/repairs` with `auth: {type: bearer_env, key: OPEX_AUTH_TOKEN}`. The failure is the INTENTIONAL reserved-secret guard (F002): `tools/yaml_tools.rs:27-30` → `secrets::is_reserved_secret_name()` (`secrets.rs:43-45` — `OPEX_MASTER_KEY`/`OPEX_AUTH_TOKEN`/`DATABASE_URL` may never be tool credentials; admin-token exfiltration guard). The guard fires BEFORE env is read — no env injection can ever fix this. **Do NOT touch docker-compose.yml** (irrelevant here; also the server compose intentionally diverges — tts-silero commented out).

- [ ] **Step 1: Confirm current state** (read-only):

```bash
ssh aronmav@188.246.224.118 "cat ~/opex/workspace/tools/core_get_skills_repairs.yaml"
```

Expected: `auth: {type: bearer_env, key: OPEX_AUTH_TOKEN}`.

- [ ] **Step 2: Operator decision (AskUserQuestion):**
  - **(a) Delete the tool (recommended):** it cannot work by construction; skills-repairs data is already reachable via `GET /api/skills/repairs` for the operator, and base agents can query it through other sanctioned paths.
  - **(b) Dedicated non-reserved secret:** mint `CORE_API_TOKEN` in the vault (same token value) via `POST /api/secrets`, repoint the yaml's `auth.key` to it. This is a deliberate, operator-approved narrow bypass of F002 — do NOT weaken `is_reserved_secret_name` itself.

- [ ] **Step 3: Apply on the server.** (a): `ssh aronmav@188.246.224.118 'rm ~/opex/workspace/tools/core_get_skills_repairs.yaml'`. (b): create the secret, then `sed -i 's/OPEX_AUTH_TOKEN/CORE_API_TOKEN/' ~/opex/workspace/tools/core_get_skills_repairs.yaml`.

- [ ] **Step 4: Verify.** (a): the tool disappears from `/api/tool-definitions`. (b): its next invocation succeeds (watch `tool_quality.last_error` clear).

No repo commit — the file exists only on the server.

## Task 2.2: `query_db` — NOT a config gap (review: hypothesis refuted); optional description hardening

**Reality (verified):** served by mcp-postgres (`workspace/mcp/postgres.yaml` → `docker/mcp/postgres/app.py`), read-only/SELECT-only, DSN already targets the correct opex DB. 33 calls / 5 fails (85% success). The `relation "agents" does not exist` errors are AGENT-AUTHORED SQL against a table that doesn't exist by design (agents live in TOML). Changing the DSN or disabling the tool would be WRONG — outcome (c): the tool is fine; the incident is bad model SQL.

- [ ] **Step 1 (optional, low priority): harden the description** in `docker/mcp/postgres/app.py` (repo-tracked): append to the `query_db` tool description: `"This is the OPEX application DB. Agent definitions live in TOML files — there is no 'agents' table. Call list_tables first when unsure."`

- [ ] **Step 2: Commit (repo)**: `git add docker/mcp/postgres/app.py && git commit -m "docs(mcp): query_db description — no agents table, list_tables first"`.

- [ ] **Step 3: Defer the container rebuild** — the description only takes effect on an mcp-postgres image rebuild; batch it with the next compose-touching deploy rather than a dedicated one (note: compose is NOT synced by server-deploy.sh, and the server compose has intentional divergence — see Global Constraints).

## Task 2.3: Normalize stale `process_start` in agent deny-lists — SERVER-side only (review M4)

**Reality (verified):** repo `config/agents/` contains only `.gitkeep` — agent TOMLs are server-runtime files, intentionally untracked. The phantom sits in server copies: `Alma.toml:23`, `Aria.toml:22`, `Arty.toml:27`, `Tyler.toml:23` (Opex is base — unaffected). No rollback risk: drift-correct is off; a UI PUT serializes current state.

- [ ] **Step 1: Confirm on the server:**

```bash
ssh aronmav@188.246.224.118 "grep -n 'process_start' ~/opex/config/agents/*.toml"
```

- [ ] **Step 2: In-place replace on the server:**

```bash
ssh aronmav@188.246.224.118 "sed -i 's/\"process_start\"/\"process\"/' ~/opex/config/agents/Alma.toml ~/opex/config/agents/Aria.toml ~/opex/config/agents/Arty.toml ~/opex/config/agents/Tyler.toml && grep -n '\"process\"' ~/opex/config/agents/*.toml"
```

- [ ] **Step 3: Restart core** (config watcher covers opex.toml only). Fold into the Batch-3 restart to avoid extra restarts. No git commit (files are not repo-tracked).

---

# BATCH 3 — Overlapping/duplicate MCP servers (verify-then-disable) + retry sanity

Pairing rule everywhere: MCP tool removal = server `enabled: false` (the effective mechanism) + name in `block` (native-schema hygiene). One core restart at the end of the batch covers 2.3 + 3.1 + 3.3.

## Task 3.1: `fetch` MCP (45/51 fail) → native `web_fetch`

**Verified:** server `~/opex/workspace/mcp/fetch.yaml` exists, `enabled: true`. Failures: robots.txt fetch errors + "No valid JSON from MCP". Native `web_fetch` is an unconditional core tool (`static_core_tool_names`, `tool_defs.rs:62`; toolgate `/web` readability extraction + SSRF + domain blocklist; does not check robots.txt — succeeds where mcp-fetch broke). Parity note: neither does JS rendering (that's `browser_action`); minor loss of mcp-fetch's `raw`/`start_index` paging vs `web_fetch`'s `max_length` — acceptable.

- [ ] **Step 1: Confirm `web_fetch` present** in `/api/tool-definitions` (same curl as Task 1.2 Step 5, grep `web_fetch`).

- [ ] **Step 2: Disable + block (server, targeted):**

```bash
ssh aronmav@188.246.224.118 "sed -i 's/^enabled: true/enabled: false/' ~/opex/workspace/mcp/fetch.yaml"
# extend the server block list (single line added in Task 1.2) — replace in place:
ssh aronmav@188.246.224.118 "sed -i 's/^block = \[\"read_file\"/block = [\"fetch\", \"read_file\"/' ~/opex/config/opex.toml && grep '^block' ~/opex/config/opex.toml"
```

- [ ] **Step 3: Mirror in repo** `config/opex.toml` (add `"fetch"` to the block list) and, if `workspace/mcp/fetch.yaml` is repo-tracked (`git ls-files workspace/mcp/fetch.yaml`), mirror `enabled: false` there too. Commit: `git commit -m "config: dedup MCP fetch in favour of native web_fetch"`.

## Task 3.2: `get_transcript` — already removed in prod; cleanup only (review: nothing to disable)

**Verified:** the mcp-youtube-transcript server is ALREADY gone — no yaml in `~/opex/workspace/mcp/` (MCP servers load exclusively from there), no service in the active compose. Last failing call 2026-07-07; zero since; no skill references it.

- [ ] **Step 1: Confirm absence:** `ssh aronmav@188.246.224.118 "ls ~/opex/workspace/mcp/ | grep -i transcript || echo gone"` → `gone`.
- [ ] **Step 2 (hygiene): remove the stale catalogue cache** `~/opex/workspace/mcp/.cache/mcp-youtube-transcript.json`; note the stale nested `~/opex/docker/docker/` copy for a later cleanup pass (do not delete it in this plan).

## Task 3.3: MCP browser (browser-cdp) — dedup, not flakiness (review: motive corrected)

**Verified:** browser-cdp's 7 tools (`browser_navigate`, `browser_click`, `browser_type`, `browser_extract_text`, `browser_screenshot`, `browser_evaluate`, `browser_close`) are a strict functional subset of native `browser_action` (17 actions incl. scroll/hover/drag/press/fill/set_dialog); session persistence is not unique (browser-renderer holds sessions too); no `/profiles` volume on the MCP container. ITS does NOT depend on it (`its.yaml` → toolgate; its-skills use `its`/`search_web`/`web_fetch`). The MCP browser is actually HEALTHY (`browser_evaluate` 29/0; the 14 navigate fails were unreachable-target addresses) — removal is justified by SURFACE DEDUP for glm-5.2 mis-selection, not by flakiness. Per the pairing rule the server MUST be disabled (block alone would leave the tools callable via `tool_use` — review M1).

- [ ] **Step 1: Confirm native `browser_action` reachable** (A8 fix): `ssh aronmav@188.246.224.118 'curl -sf -o /dev/null -w "%{http_code}\n" -X POST http://localhost:9020/automation -H "Content-Type: application/json" -d "{\"action\":\"create_session\"}"'` → `200`.

- [ ] **Step 2: Disable + block ALL SEVEN names (server, targeted):**

```bash
ssh aronmav@188.246.224.118 "sed -i 's/^enabled: true/enabled: false/' ~/opex/workspace/mcp/browser-cdp.yaml"
ssh aronmav@188.246.224.118 "sed -i 's/^block = \[/block = [\"browser_navigate\", \"browser_click\", \"browser_type\", \"browser_extract_text\", \"browser_screenshot\", \"browser_evaluate\", \"browser_close\", /' ~/opex/config/opex.toml && grep '^block' ~/opex/config/opex.toml"
```

- [ ] **Step 3: Mirror in repo** `config/opex.toml` + `workspace/mcp/browser-cdp.yaml` if tracked. Commit: `git commit -m "config: dedup MCP browser-cdp in favour of native browser_action"`.

- [ ] **Step 4: Single restart + verify for the whole batch:**

```bash
ssh aronmav@188.246.224.118 'systemctl --user restart opex-core'
ssh aronmav@188.246.224.118 "TOKEN=\$(grep -oP 'OPEX_AUTH_TOKEN=\K.*' ~/opex/.env|tr -d '\"'|head -1); curl -sf -H \"Authorization: Bearer \$TOKEN\" http://localhost:18789/api/tool-definitions | grep -oE 'browser_navigate|fetch\"|read_file' || echo 'all dedup targets absent'"
```

Expected: `all dedup targets absent`; `browser_action`, `web_fetch`, `workspace_read` still present; agents answer normally.

## Task 3.4: Retry/failover sanity for external 5xx (review M3 — spec requirement, read-only)

Spec Section 3 scope: "confirm the retry/failover policy is sane (transient 5xx should retry, not hard-fail the turn) — no code change unless the policy is found broken." Targets: `its` 502, `search_web` 5xx, `analyze_image` 5xx, `generate_image` 503.

- [ ] **Step 1: Read the YAML/capability tool execution path** (`agent/engine_dispatch.rs` + `tools/` HTTP client usage): does a 5xx response from a yaml/capability tool get retried, or returned to the LLM as a tool-error string immediately?
- [ ] **Step 2: Classify.** Returning "tool error: 502" to the LLM (which can retry itself) IS sane for tool calls (unlike LLM-call failover). Verify no path turns a tool 5xx into a hard turn failure. Check `tool_quality` penalty behaviour doesn't permanently bury a tool after a transient burst (`penalty_score` recovers via rolling window — confirm from `db/tool_quality.rs`).
- [ ] **Step 3: Verdict.** If sane (expected): record "no change needed" in the batch commit message. If a tool 5xx hard-fails a turn: file it as a follow-up fix with the exact code path — do NOT hotfix inside this batch.

---

# BATCH 4 — Visibility: `/api/tools/health` (code ships with Batch 1's deploy)

Review m1: `src/db/tool_quality.rs` ALREADY has `get_degraded_tools()` (penalty<0.8, feeds `/api/doctor`) — extend that module rather than duplicating in a handler.

## Task 4.1: `get_tool_health()` query + endpoint

**Files:**
- Modify: `crates/opex-core/src/db/tool_quality.rs` (query + `#[sqlx::test]` — the file already has the pattern at L236 with `migrations` path)
- Modify: `crates/opex-core/src/gateway/handlers/monitoring/mod.rs` (route `GET /api/tools/health`) + handler fn in `monitoring/usage.rs` (or sibling)

**Interfaces:**
- Produces: `db::tool_quality::get_tool_health(db: &PgPool) -> Result<Vec<ToolHealthRow>>` where `ToolHealthRow { agent_name: String, tool_name: String, total_calls: i64, fail_calls: i64, penalty_score: f64, last_error: Option<String> }`; handler `GET /api/tools/health` returns `{ tools: [...] }` ordered worst-impact first. Route is auto-covered by the bearer middleware (verified: not in PUBLIC_EXACT/PUBLIC_PREFIX) and does not conflict with `/api/tools` routes (axum merge panics only on exact path+method duplicates).

- [ ] **Step 1: Write the failing `#[sqlx::test]`** in `db/tool_quality.rs` tests (mirror the existing test at L236 — same `migrations` attribute):

```rust
#[sqlx::test(migrations = "../../../migrations")]
async fn tool_health_orders_by_impact(db: sqlx::PgPool) {
    // two tools: one high-impact failer, one healthy
    record_tool_result(&db, "A", "bad_tool", false, 100, Some("boom")).await.unwrap();
    record_tool_result(&db, "A", "bad_tool", false, 100, Some("boom")).await.unwrap();
    record_tool_result(&db, "A", "good_tool", true, 50, None).await.unwrap();
    let rows = get_tool_health(&db).await.unwrap();
    assert_eq!(rows[0].tool_name, "bad_tool");
    assert!(rows.iter().all(|r| r.fail_calls > 0), "healthy tools excluded");
}
```

(Adapt `record_tool_result`'s exact signature to the file's existing helper — it already exists and is used by the L236 test; copy its call shape from there.)

- [ ] **Step 2: Run — expected FAIL** locally with `EnvVar(NotPresent)` (no DATABASE_URL — project norm; authoritative run is on the server via `make test-db`). Compile-fail first proves the test exercises the new fn.

- [ ] **Step 3: Implement** `ToolHealthRow` + `get_tool_health` in `db/tool_quality.rs`:

```rust
#[derive(Debug, serde::Serialize, sqlx::FromRow)]
pub struct ToolHealthRow {
    pub agent_name: String,
    pub tool_name: String,
    pub total_calls: i64,
    pub fail_calls: i64,
    pub penalty_score: f64,
    pub last_error: Option<String>,
}

/// Failing tools ordered by impact (fail-share × volume), worst first.
/// Complements `get_degraded_tools` (penalty<0.8 → /api/doctor) with the raw
/// counters an operator needs to see WHAT is failing and how often.
pub async fn get_tool_health(db: &PgPool) -> Result<Vec<ToolHealthRow>> {
    let rows = sqlx::query_as::<_, ToolHealthRow>(
        "SELECT agent_name, tool_name, total_calls, fail_calls, penalty_score, last_error \
         FROM tool_quality WHERE fail_calls > 0 \
         ORDER BY (fail_calls::float / NULLIF(total_calls, 0)) * fail_calls DESC",
    )
    .fetch_all(db)
    .await?;
    Ok(rows)
}
```

(Adjust column types to the actual schema — `total_calls`/`fail_calls` are integers in m070/001; if they're `INT4`, use `i32` in the struct.)

- [ ] **Step 4: Handler + route.** In `monitoring/usage.rs` (or a sibling monitoring handler): fetch + wrap in `Json(json!({"tools": rows}))` with the module's usual 500-on-error shape; register in `monitoring/mod.rs::routes()`:

```rust
.route("/api/tools/health", get(api_tools_health))
```

- [ ] **Step 5: Gate** — `cargo check -p opex-core --all-targets` + clippy clean. Commit: `git commit -m "feat(monitoring): GET /api/tools/health — tool_quality degradation report"`.

- [ ] **Step 6: Ships with Batch 1's deploy** (same Rust rebuild — review m3). Smoke after that deploy:

```bash
ssh aronmav@188.246.224.118 "TOKEN=\$(grep -oP 'OPEX_AUTH_TOKEN=\K.*' ~/opex/.env|tr -d '\"'|head -1); curl -sf -H \"Authorization: Bearer \$TOKEN\" http://localhost:18789/api/tools/health | head -c 300"
```

Expected: JSON with the current failing tools (read_file family present in history until the window resets the deltas).

- [ ] **Step 7: Server-side full test run** (authoritative, per project norm): `CARGO_BUILD_JOBS=4 nice ionice` + `make test-db` on the server (or targeted `cargo test tool_health` with DATABASE_URL) — the `#[sqlx::test]` from Step 1 must pass there.

---

# Final validation (after 2-3 day prod window)

- [ ] Re-snapshot the Task 1.2 Step 6 query; diff against the baseline file. Success: zero delta for the 5 filesystem tools + `fetch` + the 7 browser-cdp tools; shrinking fail deltas overall.
- [ ] `GET /api/tools/health` — the dedup targets show no fresh `last_error` timestamps; remaining entries are external-service 5xx (its/search_web/…) or agent-SQL (`query_db`) — both understood, not tool-surface defects.
- [ ] Optional (spec Section 4 prompt note, DEFERRED): only if mis-selection persists in the data, add a short tool-selection note to `crates/opex-core/scaffold/base/*.md` — separate follow-up, not this plan.
