# Refactoring roadmap — large-module decomposition

**Date:** 2026-05-14
**Status:** approved decomposition map (no implementation yet — each wave gets its own spec → plan → impl cycle)
**Context:** follow-up to `docs/architecture/2026-05-06-architecture-review.md`, which flagged "six handler modules >1000 lines" and "decomposition work needed before scaling". This roadmap turns those findings into a concrete sequenced plan.

## Goal

Lower the cognitive load of working in `hydeclaw-core` and the UI by splitting the largest modules along boundaries that already exist in the code. Three intertwined sub-goals:

1. **Readability** — no production source file >500 LoC. Exceptions: pure-data files (large `#[derive(Deserialize)]` struct trees, generated TS types, sqlx query macros that expand inline) where line count reflects schema width rather than logic; documented when invoked.
2. **Single responsibility** — each file answers *one* of (request build / response parse / streaming / persistence / dispatch / routing / config-shape). Today several 1000+ LoC files mix three or four of those.
3. **Extensibility** — surface the seams that already exist (Trait, Registry, Factory) so adding a new provider / handler / tool / config section does not require touching megafiles.

Functional behaviour stays identical at every commit. No public API change (REST, SSE event types, TOML config shape, sqlx queries) — modules are internal detail.

## Decomposition principle

Each wave groups files by **type of responsibility**, not by "N files per sprint". Files inside a wave share:

- the same test surface (mock_provider, integration tests, sqlx tests, …),
- the same risk class,
- the same seam (trait or boundary) that the split will sharpen.

Between waves the seams are stable: `LlmProvider` trait, `pipeline::EventSink` trait, `db::*` query functions, `gateway/handlers/*` route signatures, `MemoryStore` trait. A change inside one wave cannot cascade into another wave unless one of those seams shifts — which is out of scope for refactoring.

## Acceptance criteria — every wave

Each wave passes the same gate:

- `cargo clippy --all-targets -- -D warnings` clean
- `cargo test --workspace` clean against the test DB (existing baseline failures unchanged)
- rustls invariant holds (`cargo tree --workspace` shows no `openssl-sys` / `native-tls`; verified by existing CI jobs `rustls-only (default)` and `rustls-only (otel)`)
- Every commit independently builds — no half-merged states
- Behaviour-preserving: no SSE event field added/removed, no DB column touched, no TOML key renamed
- Each "split" commit moves code without rewriting it; rewrites are separate follow-up commits if needed
- Wave-end (acceptance) commit message summarises the new module tree; this repo pushes direct to `master` (no PR workflow)
- No public surface (re-exports, trait shapes) changes mid-wave — only at the wave-final cleanup commit

## Waves

### Wave 1 — providers

**Files (3996 LoC total → target ~8 modules, average ~400-500 LoC):**

| File                                                    | LoC  | Action                                                       |
| ------------------------------------------------------- | ---- | ------------------------------------------------------------ |
| `crates/hydeclaw-core/src/agent/providers/anthropic.rs` | 1643 | full split (5 modules)                                       |
| `crates/hydeclaw-core/src/agent/providers/openai.rs`    | 1230 | full split (5 modules)                                       |
| `crates/hydeclaw-core/src/agent/providers/google.rs`    | 619  | optional partial split (2-3 modules) if W1 brainstorm agrees |
| `crates/hydeclaw-core/src/agent/providers/http.rs`      | 350  | leave as-is (already under threshold)                        |
| `crates/hydeclaw-core/src/agent/providers/claude_cli.rs`| 154  | leave as-is                                                  |

**Target split per adapter:**

```text
agent/providers/anthropic/
  mod.rs              (~200 LoC — LlmProvider impl, public re-exports)
  request.rs          (~300 LoC — Request building, message conversion)
  response.rs         (~300 LoC — Response parsing, content blocks)
  stream.rs           (~400 LoC — SSE chunk decoding, tool-call streaming)
  tool_calls.rs       (~300 LoC — tool_use blocks, parallel/serial)
```

Same shape for `openai/`, smaller wedges for `google/`, `claude_cli/`, `http/`.

**Risk:** low. `LlmProvider` trait is stable since the routing/registry split (commits `f3356ada` and earlier). `integration_mock_provider.rs` and per-adapter unit tests cover the public contract.

**Test guards (pre-existing):**

- `agent::providers::anthropic::tests` (request/response/stream)
- `agent::providers::openai::tests`
- `tests/integration_mock_provider.rs`

**Discovery commit:** measure per-fn coverage via `cargo llvm-cov` for the two largest adapters. Add golden-fixture tests if any branch is uncovered (e.g. unusual content-block types, parallel tool_use).

**Effort:** ~2-3 days for an experienced engineer (split-by-grep is mechanical; the slow part is the discovery commit).

---

### Wave 2 — pipeline

**Files (6993 LoC total → target ~15 modules, average ~400-500 LoC):**

- `crates/hydeclaw-core/src/agent/pipeline/execute.rs` — 1514 LoC
- `crates/hydeclaw-core/src/agent/pipeline/parallel.rs` — 1426 LoC
- `crates/hydeclaw-core/src/agent/pipeline/media_background.rs` — 1735 LoC
- `crates/hydeclaw-core/src/agent/pipeline/handlers.rs` — 1202 LoC
- `crates/hydeclaw-core/src/agent/pipeline/finalize.rs` — 1116 LoC

**Target split:**

- `execute.rs` → `execute/{loop_core, llm_call, tool_dispatch, behaviour_apply}.rs` (~400 each). Keep the orchestration `pub fn execute(...)` in `mod.rs`.
- `parallel.rs` → `parallel/{batch_runner, persistence, tool_executor}.rs`.
- `media_background.rs` → `media_background/{photo, voice, video, common}.rs`.
- `handlers.rs` → split per tool handler family (workspace_*, memory_*, code_exec, agent, browser, tool_test).
- `finalize.rs` → `finalize/{persist, lifecycle, knowledge_extract}.rs`.

**Risk:** medium-high. The hottest code path in the project; subtle invariants around SSE streaming, parallel tool execution, error-class propagation, partial-chain persistence.

**Test guards (must exist before any split lands):**

- `tests/integration_partial_chain.rs`
- `tests/integration_parallel_batch_id.rs`
- `tests/integration_iteration_id_wire.rs`
- `tests/integration_mock_provider.rs`
- `tests/integration_aborted_usage.rs`
- `tests/integration_dashboard_metrics.rs` (cardinality)
- New: golden SSE-stream snapshot for a representative tool-using turn (record `StreamEvent` sequence, replay in test)

**Discovery commit:** add the missing golden snapshot test before the first split commit. If the snapshot is stable across two consecutive `cargo test` runs, the freeze line for behaviour preservation is set.

**Effort:** ~5-7 days. Worth doing only after Wave 1 builds muscle memory for the patterns.

---

### Wave 3 — data + config + tools

**Files (11030 LoC total → target ~25 modules, average ~400-500 LoC):**

- `crates/hydeclaw-core/src/config/mod.rs` — 2648 LoC
- `crates/hydeclaw-db/src/sessions.rs` — 2550 LoC
- `crates/hydeclaw-core/src/tools/yaml_tools.rs` — 2646 LoC
- `crates/hydeclaw-core/src/agent/workspace.rs` — 1582 LoC
- `crates/hydeclaw-core/src/agent/history.rs` — 1504 LoC

**Target split:**

- `config/mod.rs` → `config/{gateway, limits, agents, scheduler, resources, uploads, otel, sandbox, docker, backup, curator, cleanup, shutdown, agent_tool, tools_cache, mcp, memory}.rs`. Keep `AppConfig` aggregator in `mod.rs`.
- `db/sessions.rs` → `db/sessions/{read, write, lifecycle, branching, timeline, fork}.rs`. Keep `sessions/mod.rs` as re-export façade.
- `tools/yaml_tools.rs` → `tools/yaml/{def, cache, pagination, graphql, pipeline, auth, retry, openapi, render, execute}.rs`.
- `agent/workspace.rs` → `agent/workspace/{paths, read, write, watcher, protection}.rs`.
- `agent/history.rs` → `agent/history/{convert, query, compact}.rs`.

**Risk:** medium. Serde and sqlx are sensitive to field order and column order. The fast feedback loop is `cargo test`; the slow loop is "did I break TOML deserialization for a niche config section". Drift-checks help here.

> The module layouts above are **proposed targets**; each W3 sub-spec re-opens them via its own brainstorm (in particular: is `tools/yaml/` the right home, should `[tools_cache]` nest under `[tools]`, etc. — see Open questions).

**Test guards:**

- `cargo test -p hydeclaw-core --bin hydeclaw-core config::tests` (parse round-trip)
- `cargo test -p hydeclaw-db` (full sqlx test matrix)
- `cargo test -p hydeclaw-core --bin hydeclaw-core tools::yaml_tools` (already 73 tests after the cache work)
- `cargo test -p hydeclaw-core --bin hydeclaw-core agent::workspace` + `agent::history`
- New: TOML round-trip golden snapshots — write a representative `hydeclaw.toml` fixture, deserialize → serialize, snapshot the JSON. Any reorder caught immediately.

**Discovery commit:** add the TOML round-trip golden snapshot. Add per-table `db::sessions` invariant tests (insert/read same row, compare hashes) before splitting.

**Effort:** ~5-7 days, split across three sub-projects (one per file family: config, db/sessions, yaml_tools+workspace+history).

---

### Wave 4 — handlers + scheduler + main

**Files (7407 LoC total → target ~15 modules, average ~450 LoC):**

- `crates/hydeclaw-core/src/gateway/handlers/agents/crud.rs` — 1393 LoC
- `crates/hydeclaw-core/src/gateway/handlers/providers.rs` — 1327 LoC
- `crates/hydeclaw-core/src/gateway/handlers/sessions.rs` — 1237 LoC
- `crates/hydeclaw-core/src/scheduler/mod.rs` — 2089 LoC
- `crates/hydeclaw-core/src/main.rs` — 1361 LoC

**Target split:**

- `handlers/agents/crud.rs` → split per action group (`list`, `get`, `create`, `update`, `delete`, `rename`, `channels`, `github_repos`).
- `handlers/providers.rs` → split per resource family (CRUD, active-providers, secrets, validation, model-listing).
- `handlers/sessions.rs` → split per operation (CRUD, fork, active-path, stuck, retry).
- `scheduler/mod.rs` → `scheduler/{cron_parse, timezone, heartbeat, announce, job_runner, target_resolution}.rs`.
- `main.rs` → `startup/{config, db, services, managed_processes, gateway, signals, shutdown}.rs`. Keep `main()` as the orchestration entry point (~150 LoC).

**Risk:** low-medium. Handlers are exercised by `integration_*` REST tests. Scheduler is tested by `scheduler::tests` (53 tests, including Task 1 additions). `main.rs` is the startup sequence — risk is in ordering (e.g. metrics registry must exist before db pool subscribers).

**Test guards:**

- `tests/integration_gateway_no_leak.rs`
- `tests/integration_approval_id.rs`, `integration_approval_race.rs`, `integration_approval_security.rs` (handler-layer behaviour)
- `tests/integration_csp_report.rs`, `integration_mock_provider.rs`
- All `gateway::handlers::*::tests` (per-handler unit suites)
- `scheduler::tests` (53 tests; already comprehensive, including T1 additions)
- New: a "boot smoke" integration test for `main.rs` — spawn the binary against a fixture config + ephemeral postgres, hit `/api/doctor`, assert all 16 checks `ok`.

**Discovery commit:** add the boot smoke test before splitting `main.rs`.

**Effort:** ~5-6 days.

---

### Wave 5 — UI

**Files (6876 LoC total → target ~30 components):**

- `ui/src/app/(authenticated)/monitor/page.tsx` — 1796 LoC
- `ui/src/app/(authenticated)/providers/page.tsx` — 1144 LoC
- `ui/src/app/(authenticated)/tools/page.tsx` — 1104 LoC
- `ui/src/app/(authenticated)/agents/AgentEditDialog.tsx` — 1056 LoC
- `ui/src/app/(authenticated)/chat/page.tsx` — 937 LoC
- `ui/src/app/setup/page.tsx` — 843 LoC

**Target split:** for each page, extract sub-components into a sibling folder (`monitor/components/{Chart, Filters, MetricCard, …}.tsx`) and behaviour into hooks (`monitor/use-metrics.ts`). Keep the page file as a layout shell (<200 LoC) that wires components + hooks together. The `chat-store` decomposition (Phase 54+, now 70 LoC for `chat-store.ts` + 6 sibling files) is the proven pattern.

**Risk:** low. UI is functionally tested by vitest unit tests; visual changes caught by manual smoke or a future Playwright pass.

**Test guards:**

- `ui/src/__tests__/*.test.{ts,tsx}` (existing vitest suite)
- New: per-extracted-component unit test if logic is non-trivial

**Effort:** ~5-7 days, can run in parallel with any Rust wave (no shared files). The wider range vs Rust waves reflects the absence of a Playwright E2E baseline — visual regressions require manual smoke.

---

## Dependencies

| Wave         | Depends on | Reason                                                               |
| ------------ | ---------- | -------------------------------------------------------------------- |
| W1 providers | —          | Trait is stable; touches only `agent/providers/*`                    |
| W2 pipeline  | —          | Calls Provider via trait; structural changes inside W1 are invisible |
| W3 data      | —          | Public re-exports preserved; downstream callers see the same API     |
| W4 handlers  | —          | Calls db / config / pipeline / provider via public surface           |
| W5 UI        | —          | Talks to backend via REST / SSE only                                 |

All five waves are technically independent. **The recommended order optimizes for risk-graduated learning, not for hard dependencies.**

## Recommended order

```text
W1 (providers, warm-up)
  ↓
W3a (yaml_tools — recent code, 73 baseline tests already)
  ↓
W4a (scheduler — already partially split during T1 of reliability-gaps work)
  ↓
W4b (main.rs — mechanical startup-sequence split)
  ↓
W2 (pipeline — apply the muscle memory)
  ↓
W3b (config/mod.rs — careful TOML drift testing)
  ↓
W3c (db/sessions.rs — careful sqlx schema testing)
  ↓
W4c (handlers — last because they consume everything below)
  ↓
W5 (UI — parallel-safe with any of the above)
```

**Rationale:** the easiest, lowest-blast-radius wedges go first (`yaml_tools` + `scheduler` already have strong tests from recent work). The riskiest hotspots (pipeline, config, db) go in the middle of the run when test scaffolding is fresh and we have momentum. Handlers go last because their surface area is the largest *and* they call into everything below — splitting them earlier would risk re-splitting after lower-level rework.

## Per-wave delivery cadence

Each wave follows this rhythm:

1. **Discovery commit** — measure coverage, freeze golden snapshots, mark hot spots in the file we're about to split. Lands as a single commit `chore(<scope>): freeze test baseline before refactor`.
2. **One-extract-per-commit** — each `refactor(<scope>): extract X to <path>` moves code **without rewriting it**. The diff for these commits is dominated by `mv`-shaped patterns: old file shrinks, new file appears, public re-exports updated.
3. **Cleanup commit** — after all extracts, one `chore(<scope>): tighten visibility and re-exports` commit narrows `pub` → `pub(crate)` / `pub(super)` where the new boundaries allow it.
4. **Acceptance commit** — runs the full test matrix locally; if anything broke, fix in this commit (not the extract commits — those stay clean). Drops golden snapshots that are no longer needed.

The wave is "done" when every extract commit independently builds + tests + clippy clean.

## Cross-cutting rules

- **One Rust wave at a time.** UI wave (W5) is parallel-safe with any Rust wave (touches no shared files). Two Rust waves running in parallel risk conflict in `crates/hydeclaw-core/src/lib.rs` re-exports and `agent/mod.rs` / `gateway/mod.rs` module declarations — serialize at those merge points.
- **No rewrites in extract commits.** If you change *what* code does in the same commit that moves it, the diff is unreviewable. Rewrites are separate follow-up commits clearly labelled `refactor(<scope>): simplify <thing>`.
- **No new features.** If discovery turns up a bug, file an issue and fix it in a separate commit on the same wave branch — the bug fix is reviewed on its own merits.
- **No tool-policy change.** `agent::engine::dispatch_impl::SUBAGENT_DENIED_TOOLS` and friends are out of scope unless a wave's seam crosses them (it shouldn't).
- **No DB migration.** Schema is frozen during refactoring waves. If a column needs renaming for clarity, it lands in a separate non-refactor commit before or after the wave.

## Open questions (per-wave brainstorm fills these)

- W1: which adapter goes first — Anthropic (largest) or OpenAI (more callers)?
- W2: how do we capture the SSE-stream golden snapshot — record-replay, hand-written?
- W3a: is the `tools/yaml` directory the right home, or should some pieces (e.g. `pipeline.rs` for response transforms) live elsewhere?
- W3b: should `[tools_cache]` move under a parent `[tools]` section as part of the config split? (compat shim if yes)
- W4a: scheduler's `tz_offset` helper — extract or leave inline?
- W4c: handler split — by HTTP verb, by domain object, or by action lifecycle?
- W5: which page first — most-visited (`chat`) or biggest (`monitor`)?

Each of these is resolved during that wave's design brainstorm. The roadmap does not pre-decide them.

## Out of scope (deliberately)

- **No behavioural changes.** No new SSE event types. No new config keys. No new DB columns.
- **No feature work.** This is a structure-only refactor.
- **No CI/CD changes.** Workflows stay as-is.
- **No dependency upgrades.** Crate versions frozen during a wave.
- **No documentation rewrites** beyond updating `CLAUDE.md`'s file-path references at the end of each wave.

## Effort summary

| Wave                       | Source LoC | Target modules | Risk        | Wall-clock estimate       |
| -------------------------- | ---------- | -------------- | ----------- | ------------------------- |
| W1 providers               | 3996       | ~8             | low         | 2-3 days                  |
| W2 pipeline                | 6993       | ~15            | medium-high | 5-7 days                  |
| W3 data+config+tools       | 10930      | ~25            | medium      | 5-7 days (3 sub-projects) |
| W4 handlers+scheduler+main | 7407       | ~15            | low-medium  | 5-6 days                  |
| W5 UI                      | 6880       | ~30            | low         | 5-7 days                  |
| **Total**                  | **36206**  | **~93**        | —           | **22-30 days**            |

Estimates assume the work is the *only* thing on a focused engineer's plate. With normal interrupt-driven work this stretches to ~6-9 weeks elapsed time.

## Next step

User picks the first wave to brainstorm in detail. That wave gets its own design spec at `docs/superpowers/specs/YYYY-MM-DD-<wave>-refactor-design.md` and follows the brainstorming → writing-plans → execution loop.
