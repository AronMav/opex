# Agent Tool-Reliability — Design

**Date:** 2026-07-18
**Status:** design (pending user review)
**Context:** After switching all agents to `zai-coding-plan`/glm-5.2, the operator
reports agents "often miss" tool calls / bugs appear in sessions. This design is
a comprehensive tool-reliability remediation grounded in production `tool_quality`
data, not speculation.

## Problem — grounded in prod `tool_quality`

Failure patterns observed (window: recent days, per-agent × tool):

| Pattern | Evidence | Root |
|---|---|---|
| Filesystem tool confusion | `read_file` 64 fails, `edit_file`/`write_file`/`list_directory`/`search_files` — all "Access denied: path outside /workspace" | Agents have BOTH native `workspace_*` AND MCP-filesystem duplicates; glm-5.2 picks the jailed MCP one with host paths |
| Flaky MCP servers | `fetch` 45/51 fail (robots/JSON), `get_transcript` MCP timeout, `browser_navigate` Page.goto | Broken/duplicate MCP servers |
| Config gaps | `core_get_skills_repairs` → `env var 'OPEX_AUTH_TOKEN'` unset; `query_db` → relation "agents" (wrong DB) | Missing env injection / wrong tool target |
| External/service | `its` 502, `search_web`/`analyze_image` 5xx, `generate_image` occasional 503 | External health / retry policy |

Amplifier: glm-5.2 has a known weak tool-selection tendency in this project (the
`always_core` promotion was a prior fix for MCP-call hallucination). Now that
**all** agents run on glm-5.2, ambiguous/overlapping tools hurt more.

## Guiding principle

Remove ambiguity from the tool surface (fewer, non-overlapping tools) so a
weaker tool-selector can't pick the wrong one — then fix the genuinely broken
servers and config gaps, and make degradation visible.

---

## Section 1 — Filesystem tool de-duplication (primary lever)

**Decision (owner-approved):** disable the MCP-filesystem server + globally deny
its tool names. Agents already have everything they need:
- Native `workspace_read/write/edit/list` — the agent's own dir, plus workspace
  root for base agents (`workspace.rs::agent_dir` + base-relaxed rules).
- `code_exec` — full host access for base agents (the `/home/aronmav` paths the
  agents were wrongly sending to the jailed MCP-filesystem).

The MCP-filesystem (jailed to `/workspace`, port 9045) is a redundant,
differently-jailed duplicate that currently mostly **fails** — nothing working
is lost by removing it.

**Mechanism (owner-approved: global, in `opex.toml`):**
1. `workspace/mcp/filesystem.yaml` → `enabled: false` (removes read_file/
   write_file/edit_file/list_directory/search_files from every agent's schema —
   MCP tools are only offered when the server is enabled).
2. Add a new global tool-deny to `[tool_dispatcher]`:
   `block = ["read_file", "write_file", "edit_file", "list_directory", "search_files"]`
   as a belt-and-suspenders + the single global place to manage future dedup.
   This is a new field `GlobalToolDispatcherConfig.block: Vec<String>` mirroring
   the existing `always_core: Vec<String>`, applied in the dispatch tool filter
   (`agent/pipeline/dispatch.rs`) before per-agent policy — a tool in the global
   `block` list is never offered to any agent.

**Unit:** `block` is a pure name filter over the assembled tool list; testable in
isolation (list in → filtered list out). Depends only on config.

**Reversible:** re-enable the yaml + clear `block`.

---

## Section 2 — Flaky MCP servers

Per-server audit (in the plan). Known cases:
- `fetch` (45/51 fail: robots.txt / "No valid JSON"): investigate the MCP fetch
  server config; if unfixable cheaply, prefer the native `web_fetch` tool where
  it overlaps and disable the MCP `fetch`.
- `get_transcript` (MCP process timeout): the transcript path already exists as a
  toolgate handler (`summarize_video`/transcribe); evaluate disabling the MCP
  duplicate.
- `browser_navigate` / `browser_evaluate` (MCP browser vs native `browser_action`):
  overlap. Prefer native `browser_action` (fixed in the A8 batch to localhost),
  add the MCP browser tool names to the global `block` if redundant.

Each disable/keep decision follows the Section-1 pattern (yaml `enabled:false`
and/or global `block`). No decision is made blind — the plan verifies each server
still has a live consumer before disabling.

---

## Section 3 — Config gaps

- **`core_get_skills_repairs` → `OPEX_AUTH_TOKEN` unset:** the MCP/tool that calls
  back into core lacks the auth token in its environment. Fix the env injection
  for that server (the token is already in core's `.env`; the MCP container/tool
  needs it passed through).
- **`query_db` → relation "agents":** the tool targets a database/schema that
  lacks the expected relation. Fix the tool's connection target (correct DB/DSN).
- **External 5xx/502/503** (`its`, `search_web`, `analyze_image`, `generate_image`):
  these are upstream health, not code defects. Scope here is limited to
  confirming the retry/failover policy is sane (transient 5xx should retry, not
  hard-fail the turn) — no code change unless the policy is found broken.

---

## Section 4 — glm-5.2 guardrails + visibility

- **Reduce overlap** (delivered by Sections 1-2) is the main guardrail: a smaller,
  unambiguous tool surface is what a weak tool-selector needs.
- **Visibility:** `tool_quality` already tracks per-agent/tool success, penalty,
  last_error. Add a lightweight operator report (a query/endpoint or a scheduled
  summary) surfacing tools whose penalty/fail-rate degraded — so regressions are
  caught early instead of felt anecdotally. Exact surface (endpoint vs cron
  notification) decided in the plan.
- **Prompt note (optional, low priority):** only if data after Sections 1-3 still
  shows systematic mis-selection, add a short tool-selection note to the base
  scaffold. Deferred until measured.

---

## Validation

- **Primary metric:** `tool_quality` fail-rate (fail_calls / total_calls) and
  penalty_score per tool, before vs after, over a 2-3 day prod window. Target:
  the filesystem-family fails (read_file/write_file/edit_file/list_directory/
  search_files) drop to ~0 (tools gone); `fetch` fails drop after Section 2.
- **Smoke:** after each batch — `/health` 200, NRestarts=0, agents still list a
  coherent tool set (native workspace_* + code_exec present; removed duplicates
  absent), a real agent turn completes and calls a file tool correctly.
- **Rust gate:** `cargo check --all-targets` + `clippy -D warnings`; the new
  `block` filter gets a unit test (a blocked name is absent from the filtered
  list; a non-blocked one survives).

## Rollout — batches by risk

1. **Batch 1 — filesystem dedup + global `block` config** (primary win, low risk,
   reversible): Rust `block` field + filter + test; disable mcp-filesystem yaml;
   set `block` in opex.toml. Deploy, measure.
2. **Batch 2 — config gaps** (`core_get_skills_repairs` auth env, `query_db`
   target): targeted config/env fixes.
3. **Batch 3 — flaky MCP audit** (fetch/get_transcript/browser dedup): per-server
   verify-then-disable/keep.
4. **Batch 4 — visibility** (tool_quality degradation report) + optional prompt
   note if still warranted by data.

Batches deploy independently (Rust rebuild for B1/B4-code; config sync for
B2/B3), each smoke-verified, mirroring the cycle-A cadence.

## Non-goals

- Fixing upstream external services (z.ai/ollama/its availability) — out of scope.
- Changing the LLM model or provider routing — the operator chose glm-5.2.
- Rewriting the MCP subsystem — we only enable/disable/dedup existing servers.
