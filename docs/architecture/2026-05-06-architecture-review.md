# HydeClaw Architecture Review

**Date:** 2026-05-06
**Reviewer:** Architectural audit pass
**Repository state:** `master` at v0.26.0, working tree clean
**Scope:** End-to-end review of the Rust core, frontend, sidecar processes, persistence, observability, and supporting tooling.

## Executive summary

HydeClaw is a single-binary Rust AI gateway (with two satellite Rust binaries and two managed-process sidecars) targeting Raspberry Pi-class hardware. At ~96k LoC of Rust across six crates, ~46k LoC of TypeScript in a Next.js 16 UI, ~44k LoC of Python in a FastAPI media hub, and ~6k LoC of TypeScript in the channel adapter, it is a substantial but coherent codebase.

The project shows the marks of a small team working at high velocity with strong engineering hygiene: 137 design specs and plans under `docs/superpowers/`, 46 forward-only SQL migrations, 53 integration test files, ~1.4k tests across the workspace, three CI matrices (Rust/UI/toolgate/channels) including a generated-types drift gate, deterministic ARM64 cross-compilation via `cargo zigbuild`, end-to-end W3C-traceparent propagation across four processes, and zero `unsafe` blocks in the Rust code. Code-quality signals are strong: only three apparent TODO/FIXME/HACK markers and no `#[deprecated]` annotations.

The dominant architectural risks are weight, not rot. The `hydeclaw-core` crate carries 213 source files and a single-responsibility-per-module structure that has begun to strain at the edges: the LLM/tools loop is split between `pipeline::execute` and a parallel `engine::stream::handle_isolated` path that share helpers but not control flow; six handler modules in `gateway/handlers/` exceed 1000 lines; `agent/providers.rs` is a 2179-line file that does both provider construction and routing; and the `lib.rs` facade has accumulated a tangle of `#[doc(hidden)]` re-mounts to expose leaf modules to integration tests without dragging in the full agent subtree. Documentation drift is small but real (CLAUDE.md still describes OTel 0.26-0.28, but Cargo.toml is on 0.31/0.32; chat-store.ts is described as 451 lines, actual is 70).

The system is production-ready for its stated target — a single-tenant Pi-class deployment with a small number of agents — and shows visible investment in the things that matter on that target (boot-time provider hot-reload, graceful shutdown drain, watchdog resource recovery, SSE replay via `Last-Event-ID`, chaos tests against a real Pi). The main work ahead is consolidating the duplicated LLM-loop code path, splitting the handler god-modules into per-resource sub-routers in the same way the providers already are, and reducing the surface area of `lib.rs` by promoting the most-needed leaves into a real public API. None of these are urgent.

**Maturity rating: 7.5/10.** Solid for the stated target. Would need decomposition work before scaling to multi-tenant or beyond a few hundred agents per instance.

## Architecture overview

### 1. Top-level decomposition

The Cargo workspace is six crates ([Cargo.toml](../../Cargo.toml)):

| Crate | LoC | Role |
| --- | --- | --- |
| `hydeclaw-core` | 80,174 | The HTTP API, agent engine, providers, tools, channels, memory, secrets, gateway, scheduler, MCP, curator, skills |
| `hydeclaw-types` | 1,462 | Shared serde DTOs (Message, ToolCall, ChannelInbound/Outbound, MediaAttachment, …) |
| `hydeclaw-db` | 3,597 | Extracted DB query module set (sessions, memory_queries, approvals, session_wal, usage, notifications, session_failures) |
| `hydeclaw-watchdog` | 977 | Standalone systemd-friendly health monitor binary |
| `hydeclaw-memory-worker` | 327 | Standalone embedding-reindex worker (PostgreSQL LISTEN/NOTIFY) |
| `hydeclaw-gateway-util` | 597 | Leaf utilities reused by core's lib facade and the binary: rate limiter, restore-stream parser, W3C trace_context middleware |

The `hydeclaw-types` and `hydeclaw-db` and `hydeclaw-gateway-util` crates were extracted to give integration tests a path to leaf modules without dragging the agent subtree into the lib facade — the boundary is **test-driven, not architecture-driven**. The split is real (no cycles) but the seams are accidental: `hydeclaw-db/src/sessions.rs` is 1762 lines and clearly grew large enough to warrant its own crate; `hydeclaw-gateway-util` is 597 lines of mostly unrelated helpers that happen to be `crate::*`-free.

There is no integration crate / SDK / contract crate mediating between the binary and external clients (channels, UI). Channel TS code in [channels/src/types.ts](../../channels/src/types.ts) hand-mirrors [crates/hydeclaw-types/src/lib.rs](../../crates/hydeclaw-types/src/lib.rs); the comment at the top of `types.ts` literally says "Port of crates/hydeclaw-types/src/lib.rs:138-325". A `ts-rs` codegen path (`make gen-types`, gated behind `ts-gen` feature) handles UI types via the `register_ts_dto!` macro (47 registrations across the core source) and is enforced by a CI drift check, but it does **not** target the channels adapter.

### 2. The Rust core (`crates/hydeclaw-core/`)

213 Rust source files; 80,174 LoC. Top-level modules (from `main.rs`): `agent`, `channels`, `config`, `containers`, `curator`, `db`, `dto_export`, `gateway`, `mcp`, `memory`, `metrics`, `net`, `oauth`, `process_manager`, `scheduler`, `secrets`, `shutdown`, `skills`, `tasks`, `tools`, `trace_propagation`, `uploads`.

**Top 10 largest modules** (LoC, by single file):

1. [src/config/mod.rs](../../crates/hydeclaw-core/src/config/mod.rs) — 2,602 — TOML schema + serde defaults for the entire config tree
2. [src/tools/yaml_tools.rs](../../crates/hydeclaw-core/src/tools/yaml_tools.rs) — 2,360 — YAML tool loader/runner, SSRF protection, response_transform
3. [src/agent/providers.rs](../../crates/hydeclaw-core/src/agent/providers.rs) — 2,179 — `LlmProvider` trait, `RoutingProvider`, `build_provider`, fallback logic, `UnconfiguredProvider` sentinel
4. [src/scheduler/mod.rs](../../crates/hydeclaw-core/src/scheduler/mod.rs) — 2,116 — cron + heartbeat scheduler
5. [src/gateway/handlers/monitoring.rs](../../crates/hydeclaw-core/src/gateway/handlers/monitoring.rs) — 1,575 — `/api/health/*` handlers + dashboard aggregator
6. [src/agent/cli_backend.rs](../../crates/hydeclaw-core/src/agent/cli_backend.rs) — 1,541 — CLI provider runner (claude/gemini CLI processes)
7. [src/agent/history.rs](../../crates/hydeclaw-core/src/agent/history.rs) — 1,491 — message history loading, pruning, reconstruction
8. [src/gateway/handlers/chat.rs](../../crates/hydeclaw-core/src/gateway/handlers/chat.rs) — 1,420 — SSE chat endpoint + Last-Event-ID resume
9. [src/agent/workspace.rs](../../crates/hydeclaw-core/src/agent/workspace.rs) — 1,349 — workspace file IO + path-readonly checks
10. [src/agent/providers_anthropic.rs](../../crates/hydeclaw-core/src/agent/providers_anthropic.rs) — 1,315 — Anthropic Messages API adapter (incl. extended thinking)

`main.rs` is 1,294 lines doing config load, migrations, agent spawning, process_manager startup, axum bind, and graceful shutdown.

#### Pipeline architecture

[src/agent/pipeline/](../../crates/hydeclaw-core/src/agent/pipeline/) is the unified execution pipeline introduced in spec [2026-04-20-execution-pipeline-unification-design.md](../superpowers/specs/2026-04-20-execution-pipeline-unification-design.md). 26 modules, total ~10k LoC. Responsibilities are clearly named:

- `sink.rs` — `EventSink` trait + `PipelineEvent` (Stream / Phase) + `SseSink`/`ChannelStatusSink`/`ChunkSink`
- `bootstrap.rs` — entry: persists user message, opens WAL, builds context, fast-paths slash commands
- `execute.rs` — main LLM+tools loop (1193 lines), `#[tracing::instrument(name = "pipeline.execute")]`
- `finalize.rs` — single exit point, persists assistant or partial, WAL `done|failed|interrupted`, enqueues knowledge extraction
- `parallel.rs` — concurrent tool dispatch (semaphore-limited)
- `tool_loop_helpers.rs` — extracted helpers shared between `execute::execute` and `engine::stream::handle_isolated`
- `tool_defs.rs` — system tool schemas (1152 lines)
- `handlers.rs` — system tool implementations (1204 lines; agent/canvas/cron/sandbox/sessions etc. dispatched via `dispatch.rs`)

The decomposition is genuinely good: each file has one job and the dependencies form a DAG. The design spec is followed in spirit. Pipeline lengths (execute 1193, finalize 1051, handlers 1204, agent_tool 1019, llm_call 942, parallel 836) are still bigger than feels right for "free functions per step", but the slicing is along the actual control-flow joints, not arbitrary.

#### Engine entry points

[src/agent/engine/run.rs](../../crates/hydeclaw-core/src/agent/engine/run.rs) (468 lines) hosts three `impl AgentEngine` adapter methods that all delegate into the pipeline:

- `handle_sse(msg, event_tx, resume_session_id, force_new_session, cancel) -> Result<Uuid>` — the production path; constructs `SseSink`, registers cancel guard, propagates session_id back.
- `handle_with_status(msg, status_tx, chunk_tx) -> Result<String>` — channel-adapter path; `ChannelStatusSink` carries Phase events for typing indicators plus optional chunk channel.
- `handle_streaming(msg, chunk_tx) -> Result<String>` — plain-chunk SSE-less path; uses `use_history: false` (no prior context).

The three are largely cosmetic variants of the same flow. Each does the same eight-step dance: hook fire → cancel guard → sink construct → bootstrap → unpack BootstrapOutcome → optional slash-command shortcut → `execute::execute` → `finalize::finalize`. The `BootstrapOutcome` is even un-packed and re-packed (with `compressor: Compressor::new(0)` placeholder) because `compressor` is now passed as `&mut` and the original field would be unused. This is one of the rough edges of the unification — the design spec documents it as Phase 66 (deferred), and a comment in `execute.rs` lists what's still missing from the unified path: fallback provider switching, SessionCorruption recovery, empty-response auto-retry, auto-continue detection. So `engine::stream::handle_isolated` (469 lines) still exists for the cron RPC path, and the two share `tool_loop_helpers` rather than the full pipeline.

#### Provider abstraction

[src/agent/providers.rs](../../crates/hydeclaw-core/src/agent/providers.rs) defines `pub trait LlmProvider: Send + Sync` with `chat`/`chat_stream`/`name`. Six implementations live in sibling files:

- `providers_openai.rs` (1232 lines) — OpenAI-compatible (also handles Ollama, OpenRouter, etc.)
- `providers_anthropic.rs` (1315 lines) — Anthropic Messages API + extended thinking
- `providers_google.rs` — Gemini
- `providers_claude_cli.rs` — wraps `cli_backend.rs` (claude/gemini CLI invocation)
- `providers_http.rs` — generic HTTP provider for less-common backends
- The `UnconfiguredProvider` sentinel inside `providers.rs` for misconfigured agents

`RoutingProvider` is the multi-provider fan-out: it owns a list of `Route { provider, cooldown_until, consecutive_failures }` and implements `LlmProvider` itself, so callers see a single trait object. `handle_provider_error` decides retry vs failover vs surface. The pattern is correct and the routing/timeout/cancellation logic in `cancellable_stream.rs`, `error.rs`, and `timeouts.rs` is well-factored. The complaint is purely size — `providers.rs` does both the trait + sentinel + routing + DB-row-to-provider mapping + tests in 2179 lines.

### 3. State and persistence

**Schema:** 46 forward-only migrations in [migrations/](../../migrations) (numbered 001–046, with 005 missing — likely a renumber during PR review). The cadence is high: 11 migrations between 2026-04-30 and 2026-05-05 covering compression chains, mirroring, dry-run curator runs, message compression, and per-step ID grouping. No down migrations; sqlx auto-runs forward at startup. Latest is `046_messages_step_id.sql` adding the per-iteration step_id column for the ID-based dedup architecture documented in [docs/architecture/2026-05-05-id-based-dedup.md](2026-05-05-id-based-dedup.md).

Recent schema-affecting work clusters around three themes: (a) curator/skills self-improvement infrastructure (037–039, 044), (b) compression and mirroring (040–043, 045), (c) observability identity (046). Earlier migrations layered onto the base schema gradually — there are several "drop column" migrations (015, 027–033) that remove fields added by previous migrations. This is healthy churn (cleaning up after experimentation) but it also means the schema has ~32 net tables driven by an evolving design.

**Memory layer:** [src/memory/](../../crates/hydeclaw-core/src/memory/) is small (~1.5k LoC) and architecturally clean. `MemoryStore::search_hybrid` ([store.rs:141](../../crates/hydeclaw-core/src/memory/store.rs#L141)) fans out to three searches in parallel — `search_semantic` (pgvector halfvec), `search_fts` (PostgreSQL FTS), `search_trigram` (pg_trgm) — and combines via Reciprocal Rank Fusion with weights `W_SEM`, `W_FTS`, `W_TRGM`. There's a deliberate "single-branch shortcut" that bypasses RRF when only one branch returned results, and the final sort uses chunk-id as tiebreaker so integration tests are deterministic. MMR reranking with λ=0.75 lives only in `search_semantic`. `pinned` (L0 context) and `raw` (time-decay searchable) tiers are real, distinguished at the SQL layer, and cleanly separated. Embeddings are delegated to Toolgate via `EmbeddingService::embed` so Core never touches Ollama or OpenAI directly. This is one of the cleanest subsystems in the codebase.

**Secrets vault:** [src/secrets.rs](../../crates/hydeclaw-core/src/secrets.rs) is 577 lines, ChaCha20Poly1305 with a 32-byte master key from `HYDECLAW_MASTER_KEY` env. The master key is wrapped in `Arc<Zeroizing<[u8; 32]>>` so the bytes are zeroed when the last reference drops. Resolution order is `(name, scope)` → `(name, "")` → env var. Backup export includes decrypted secrets by design (portability across master keys), guarded by `X-Confirm-Restore` header on import. Channel credentials live under scope = channel UUID, redacted from the JSONB `agent_channels.config` column. The audit comment block at the top of the file documents the credential-leak threat model. This module is mature.

### 4. Cross-process boundaries

**Toolgate (Python, ~44k LoC):** [toolgate/](../../toolgate/) is a FastAPI sidecar managed via `[[managed_process]]` in `hydeclaw.toml`. Owns 26 provider implementations across STT (8), TTS (6), Vision (7), ImageGen (5), Embedding (2). All share the `providers/base.py` abstract base; provider selection comes from Core's `provider_active` table proxied via REST. Routers under `toolgate/routers/` expose `/transcribe-url`, `/describe-url`, `/v1/audio/speech`, `/v1/embeddings`, `/v1/imagegen`. **Why Python:** native libraries — pymupdf for PDFs, OpenAI SDKs, ElevenLabs, deepgram, transformer-based local STT models. Could be Rust in principle but the ecosystem cost is real.

**Channels (TypeScript/Bun, ~6k LoC):** [channels/](../../channels/) is an in-process child process speaking JSON over a loopback WebSocket. Eight drivers (`telegram.ts`, `discord.ts`, `slack.ts`, `matrix.ts`, `irc.ts`, `email.ts`, `whatsapp.ts`, `common.ts`). The protocol is a tagged enum: `ChannelInbound` (8 message variants) + `ChannelOutbound` (10 variants) defined identically in [channels/src/types.ts](../../channels/src/types.ts) and [crates/hydeclaw-types/src/lib.rs:204](../../crates/hydeclaw-types/src/lib.rs#L204). The TS file is a hand-written port — comment header literally says "Port of crates/hydeclaw-types/src/lib.rs:138-325". **Why TypeScript:** grammy, discord.js, matrix-bot-sdk, slack/bolt are first-class. Same trade-off as toolgate.

**WS loopback contract:** Documented inline in `types.ts` only. There is no schema (JSON Schema, protobuf, OpenAPI) and no test that validates the Rust serde output round-trips against the TS parser. Drift between the two is currently caught only by integration testing in development. The pattern is fine for a small team but is the most fragile cross-process boundary in the project.

### 5. Streaming + observability

**SSE event taxonomy:** Defined in [src/agent/stream_event.rs](../../crates/hydeclaw-core/src/agent/stream_event.rs) (the `StreamEvent` enum) and serialized to Vercel AI SDK v3 wire format in `gateway/handlers/chat.rs`. 18 variants including `SessionId`, `MessageStart`, `StepStart { step_id, message_id }`, `TextDelta`, `ToolCallStart/Args/Result`, `StepFinish`, `RichCard`, `File`, `Finish`, `ApprovalNeeded/Resolved`, `AgentSwitch`, `Error`, `Reconnecting`, `Usage`. The `Usage` variant with `cache_read_tokens`/`cache_creation_tokens`/`reasoning_tokens` (subsets of input/output, not additive) is well-documented. Frontend mirror is in [ui/src/stores/sse-events.ts](../../ui/src/stores/sse-events.ts) (193 lines). `Last-Event-ID` resume is wired through `gateway/sse/coalescer.rs` and chaos-tested by [test-pi-chaos.py](../../tests/integration/pi/test-pi-chaos.py).

**OpenTelemetry instrumentation:** Recently added in v0.26.0 (release notes: "Phase 67 — Observability & Architecture Polish"). [src/trace_propagation.rs](../../crates/hydeclaw-core/src/trace_propagation.rs) (182 lines) provides three primitives gated behind the `otel` feature:

- `inject_trace_context(RequestBuilder) -> RequestBuilder` — injects W3C `traceparent` into outbound `reqwest` calls
- `extract_trace_context_layer` — Axum middleware that opens an `http_request` span parented to the incoming `traceparent`
- `spawn_traced(future)` — wraps `tokio::spawn` with `.instrument(Span::current())` to preserve span across the spawn boundary

Instrumented spans (per [docs/architecture/observability-setup.md](observability-setup.md) and the v0.26.0 release notes): `http_request`, `pipeline.execute`, `pipeline.execute_tools`, `pipeline.finalize`, `llm.call`, `llm.request`. Sampling uses `Sampler::ParentBased(TraceIdRatioBased(ratio))` so cross-process traces are never half-sampled. Propagation is end-to-end across `hydeclaw-core` → `toolgate` → `channels` → `hydeclaw-memory-worker`, with the Python and Bun sides using `opentelemetry-instrumentation-fastapi`/`@opentelemetry/auto-instrumentations-node`. This is unusually well-done for a project of this size.

**Note:** CLAUDE.md states OTel `0.27`, `opentelemetry_sdk 0.27`, `opentelemetry-otlp 0.27`, `tracing-opentelemetry 0.28`. [Cargo.toml:60-63](../../Cargo.toml#L60-L63) is on `0.31`/`0.31`/`0.31`/`0.32`. Documentation drift.

**Metrics:** [src/metrics.rs](../../crates/hydeclaw-core/src/metrics.rs) is 1130 lines, with cardinality-guarded label tracking (`assert_label_allowed`, `unique_series_count`), per-tool latency, per-LLM-call duration, DB query duration, token usage, CSP violations with overflow counter, SSE drop counter, LLM timeout/failover counters. `MetricsRegistry` is a global `Arc` registered via `install_global` and read via `global()`. OTel instruments are registered via `install_otel_instruments`. The exposed dashboard surface (`build_dashboard_body` / `build_dashboard_body_with_snapshot`) drives `/api/health/dashboard`. Healthy.

### 6. Frontend (`ui/`)

Next.js 16 App Router, React 19, Tailwind 4, shadcn/ui, Zustand + Immer, React Query. 249 .ts/.tsx files, ~46k LoC. RSC chunk flattening is done at build time via [ui/build/adapter.cjs](../../ui/build/adapter.cjs) wired through `next.config.ts`'s `experimental.adapterPath`.

**Chat store decomposition (Phase 54+):** CLAUDE.md says `chat-store.ts` is 451 lines. The actual file is **70 lines**, having been further decomposed since CLAUDE.md was last updated:

- [chat-store.ts](../../ui/src/stores/chat-store.ts) (70) — Zustand `create()` wrapper that wires four action factories
- [chat-types.ts](../../ui/src/stores/chat-types.ts) (327) — type definitions
- [chat-history.ts](../../ui/src/stores/chat-history.ts) (377) — convertHistory, getCachedRawMessages, findSiblings
- [chat-reconciliation.ts](../../ui/src/stores/chat-reconciliation.ts) (48) — contentHash, reconcileLiveWithHistory (post-ID-based-dedup, dramatically smaller than its predecessor)
- [chat-persistence.ts](../../ui/src/stores/chat-persistence.ts) (57) — localStorage round-trip
- [chat-overlay-dedup.ts](../../ui/src/stores/chat-overlay-dedup.ts) (105) — ID-based merge
- [chat-selectors.ts](../../ui/src/stores/chat-selectors.ts) (194) — derived selectors
- [streaming-renderer.ts](../../ui/src/stores/streaming-renderer.ts) (591) — `createStreamingRenderer()` factory: SSE parsing, rAF throttling, reconnection
- [stream/stream-processor.ts](../../ui/src/stores/stream/stream-processor.ts) (629) — accumulates SSE events into `StreamBuffer`
- [chat/actions/{navigation,stream-control,session-crud,composer}.ts](../../ui/src/stores/chat/actions/) — four action factories totalling ~870 lines

Live-vs-history reconciliation is the architecturally interesting part. It used to be content-hash-based; the v0.26.0 redesign (see [docs/architecture/2026-05-05-id-based-dedup.md](2026-05-05-id-based-dedup.md)) replaced 137 lines of heuristic dedup with a 15-line ID-based merge by ensuring the SSE `step-start` event carries the same UUID the DB row will be saved under. This is a textbook example of finding the real architectural fix to a problem that had been patched several times.

### 7. Testing infrastructure

- **Unit tests:** 1071 `#[test]` and 350 `#[tokio::test]` annotations across the workspace, mostly inline with `#[cfg(test)] mod tests`. Test ratio is healthy.
- **Integration tests:** 53 files in `crates/*/tests/`. 55 `#[sqlx::test]` annotations real-DB-bound, 42 of them in `hydeclaw-core/tests/`. Test names are descriptive and scope-targeted: `integration_approval_race`, `integration_session_chain`, `integration_sse_coalescing`, `integration_ssrf_guard`, `integration_path_canonicalize`, `integration_csp_report`, `integration_otel_export`, `integration_trace_context`, etc.
- **Test database orchestration:** [docker/docker-compose.test.yml](../../docker/docker-compose.test.yml) boots an isolated Postgres on port 5434 with tmpfs storage. `make test-db` drives the full backend suite; v0.26.0 release notes claim **1122/1122 backend tests passing**.
- **CI:** Four jobs — Rust check + test + clippy (with `-D warnings`, currently `continue-on-error: true`), UI tsc + build, toolgate pytest (47 tests), channels bun test (193 tests), and a `types-drift` job that runs `make gen-types` and `git diff --exit-code` against the committed `ui/src/types/api.generated.ts`. The drift gate is exactly the right thing to do.
- **E2E / chaos:** [test-pi-e2e.py](../../tests/integration/pi/test-pi-e2e.py), [test-pi-chaos.py](../../tests/integration/pi/test-pi-chaos.py), [test-pi-concurrency.py](../../tests/integration/pi/test-pi-concurrency.py), [test-pi-trace-correlation.py](../../tests/integration/pi/test-pi-trace-correlation.py) all run against a real Pi at `192.168.1.82:18789`. The chaos test simulates random mid-stream SSE drops and asserts `Last-Event-ID` resume produces a deduplicated event timeline — this is the right way to validate streaming reliability.

### 8. Code quality signals

- **Largest files:** Top six listed in §2 (config 2602, yaml_tools 2360, providers 2179, scheduler 2116, monitoring 1575, cli_backend 1541). Below ~1500 most files have a clear single responsibility; above that, splitting would help.
- **`unsafe` blocks:** Zero (the three matches in `grep -n "unsafe " ` are all inside comments — `path-unsafe characters`, `safe for HTTP headers, unsafe for JSON bodies`, `MUST NOT contain ... unsafe chars`).
- **`unwrap()` density:** 503 in `crates/hydeclaw-core/src/`. Heaviest: `workspace.rs` (57), `yaml_tools.rs` (39), `net/ssrf.rs` (31), `gateway/handlers/workspace_files.rs` (24). Many will be in test modules; the absolute count is high but not pathological for an 80k-LoC crate.
- **`.expect()` density:** 249, mostly used as documented invariant assertions (`bootstrap always sets lifecycle_guard`).
- **`panic!`:** 63 occurrences total across all six crates — almost all inside `#[cfg(test)]` blocks based on a sampling of the matches.
- **TODO/FIXME/HACK:** 3 markers total in source code. Two are explanatory references in comments, one is a single deferred task in `bootstrap.rs:20` (`TODO: Task 10 inlines enrichment`). This is exceptionally low.
- **`#[deprecated]`:** None. No formal deprecation surface.
- **Cyclomatic complexity hotspots:** `pipeline::execute::execute` (1193 lines, deep loop with multiple early-exit paths), `RoutingProvider::chat`/`chat_stream` (failover state machine), `cli_backend.rs` (process management, parsing 7 different output formats). These are inherent to what they're doing, not gratuitous.
- **Dependency drift:** [Cargo.lock](../../Cargo.lock) has 463 `[[package]]` entries. 29 crate names appear with multiple versions — the worst is `windows-sys` (5 versions) and `hashbrown` (4). Most are transitive. `testcontainers` is pinned to 0.23 (not 0.27) because 0.24+ requires bollard 0.19+ which is incompatible with the workspace bollard 0.18 — documented in [Cargo.toml:138-145](../../crates/hydeclaw-core/Cargo.toml#L138-L145). No alarming staleness.

### 9. Documentation

- **ADRs:** [docs/architecture/](.) has just two files — [2026-05-05-id-based-dedup.md](2026-05-05-id-based-dedup.md) (the dedup architecture decision record) and [observability-setup.md](observability-setup.md) (Pi rollout runbook). The ADR pattern is right, but only one ADR exists.
- **Operational runbooks:** [docs/DEPLOYMENT.md](../DEPLOYMENT.md) (805 lines), [docs/CONFIGURATION.md](../CONFIGURATION.md), [docs/UPGRADE_NOTES.md](../UPGRADE_NOTES.md). [docs/API.md](../API.md) is 1906 lines covering the HTTP surface.
- **Architectural overview:** [docs/ARCHITECTURE.md](../ARCHITECTURE.md) (995 lines, 71 sections) is comprehensive and includes a usable ASCII diagram of the process topology.
- **CLAUDE.md accuracy:** The 47k file is mostly accurate but has a few stale spots: chat-store.ts described as 451 lines (actual 70); OTel versions described as 0.26-0.28 (actual 0.31/0.32); the description of CLAUDE.md's own structure ("Phase 54 decomposition") is a snapshot from an earlier moment in the project.
- **Specs/plans:** [docs/superpowers/specs/](../superpowers/specs/) has 68 design spec files; [docs/superpowers/plans/](../superpowers/plans/) has 69 plan files. They're dated and form a coherent narrative when read in order. The cadence (~2 specs/week recently) is sustainable.
- **Workspace-level planning:** [.planning/](../../.planning/) has milestones, phases, todos, ui-reviews directories — a separate forward-planning surface that complements the spec/plan archive.

### 10. Architectural risks and debt

#### A. Two parallel LLM/tools loops

`pipeline::execute::execute` and `engine::stream::handle_isolated` both run a turn-by-turn LLM-call + tool-loop. They share `tool_loop_helpers.rs`, but the control flow is duplicated. The header of `execute.rs` explicitly documents the "safe subset" — no fallback provider switching, no SessionCorruption recovery, no empty-response retry, no auto-continue, no WAL warm-up replay. These features only exist on the `engine::stream` path, used by cron RPC and a few legacy entry points. Spec 2026-04-20 calls this Phase 66 deferred work; until it's done, behaviour diverges between SSE chat and cron-triggered runs.

#### B. The `lib.rs` facade is doing more work than it should

[crates/hydeclaw-core/src/lib.rs](../../crates/hydeclaw-core/src/lib.rs) (329 lines, 80% comments) is a tangle of `#[doc(hidden)]` re-mounts to expose leaf modules to integration tests without dragging the agent subtree in. Three `pub mod` blocks redirect via `#[path]` into the binary's source tree; a `__memory_bridge`, `__memory_pipeline_bridge`, and `memory_test_facade` exist solely to satisfy `crate::*` references in three re-mounted memory files. Each addition is clearly justified in the comments, but the pattern is fundamentally a workaround for not having a real lib API. The lib facade is meant to be minimal (10-module cap mentioned in the comments), but every "exception" is a sign that the underlying agent subtree wants to be exposed and isn't being. The Phase 66 plan to split `engine.rs` is the real fix.

#### C. Handler god-modules

Six gateway handler files exceed 1000 lines: `monitoring.rs` (1575), `chat.rs` (1420), `providers.rs` (1233), `channel_ws.rs` (1083), `sessions.rs` (1066), `agents/crud.rs` (1059). Each is a single sub-router covering one resource. The CLAUDE.md "27 handler modules merged via `mod.rs`" pattern is solid, but six of those modules have grown into mini-monoliths. By contrast, `agents/` is already a directory because `crud.rs` was extracted from `agents.rs`; the same pattern would help `chat.rs` and `monitoring.rs`.

#### D. Channel WS contract is hand-mirrored, not generated

The single most fragile cross-process boundary. `channels/src/types.ts` literally says "Port of crates/hydeclaw-types/src/lib.rs:138-325" in its header. There's no schema, no codegen, no round-trip test. The `ts-rs` codegen path that drives `ui/src/types/api.generated.ts` could in principle be extended to `channels/src/types.generated.ts` — same mechanism, same crate.

#### E. `gateway-util` and `db` crates are extraction-driven, not domain-driven

Both crates exist primarily so integration tests can reach leaf modules without the lib facade cascading the full agent subtree. `hydeclaw-gateway-util` is 597 lines of mostly unrelated utilities (rate limiter, restore-stream parser, trace_context middleware). `hydeclaw-db` is 3597 lines, 49% of which is `sessions.rs` (1762 lines — bigger than most "monolithic" modules in `hydeclaw-core`). The crate boundaries don't follow a domain — they follow what the test harness needs. This is fine in practice but means the workspace topology is a build-system artifact, not an architectural statement.

#### F. Documentation drift

CLAUDE.md describes `chat-store.ts` as 451 lines (actual: 70). OTel versions in CLAUDE.md (0.26-0.28) don't match Cargo.toml (0.31/0.32). The "Phase 54 decomposition" section is a snapshot from an earlier point in time. None of these break anything, but they reduce trust in CLAUDE.md as a working source of truth. CLAUDE.md is large enough (47k bytes) that drift is hard to police manually.

#### G. The `agent/providers.rs` 2179-line file

It is the trait + the routing logic + the build-from-DB-row code + the sentinel + tests. The five provider impls are already in sibling files. The same split would benefit this file: extract `RoutingProvider` and `build_provider*` into their own modules, leaving the trait + sentinel in `providers.rs`.

#### H. CLAUDE.md memory describes "MAX_AGENT_TURNS configurable ✅ DONE" but design assumes single-tenant Pi

The auto-memory snippet says "single session + orchestrator, not independent agent sessions" is the user's vision. The current `SessionAgentPool` model puts each agent in its own tokio task with its own LLM context. The "single session + orchestrator" model would fundamentally change the threading topology. This is a forward-looking design tension worth surfacing — not a current bug.

#### I. `clippy --all-targets -- -D warnings` is `continue-on-error: true` in CI

[.github/workflows/ci.yml:48-50](../../.github/workflows/ci.yml#L48-L50). It runs but failures don't block the build. The flag was almost certainly added during a clippy upgrade and hasn't been removed.

#### J. No formal API versioning

[docs/API.md](../API.md) is 1906 lines but the URLs are unprefixed (`/api/agents`, not `/api/v1/agents`). There's no `Accept-Version` header convention, no deprecation policy. This is fine for a single-tenant tool used by a known UI, but locks the project into either backward-compatible-forever or a breaking-change announcement. v0.x version numbering in Cargo.toml (currently 0.26.0) implies this is acknowledged.

## Strengths

1. **Pipeline unification (specs/2026-04-20).** The `pipeline/{bootstrap, execute, finalize, sink}` decomposition reflects an actual design (free functions, transport-agnostic, `EventSink` trait), not refactor-for-its-own-sake. `execute.rs` documents what it intentionally omits and points to Phase 66 for the rest. This is what good architectural change looks like.

2. **ID-based dedup ADR (architecture/2026-05-05).** The ADR walks through how content-hash heuristics accumulated, identifies the missing identity contract, prescribes one fix (carry the same UUID from `step-start` SSE event through the DB row to the frontend), and documents the five concrete code changes that delivered it. 137 lines of heuristic dedup → 15 lines of ID-based merge. Textbook architecture work.

3. **Cross-process W3C trace propagation.** Three primitives (`inject_trace_context`, `extract_trace_context_layer`, `spawn_traced`) cleanly gated behind the `otel` feature, with default builds remaining OTel-free. End-to-end Jaeger traces span `core` → `toolgate` → `channels` → `memory-worker`. `Sampler::ParentBased(TraceIdRatioBased(ratio))` is the right primitive choice. Most projects this size never get this right.

4. **Zero `unsafe`, zero meaningful TODOs.** 80k LoC of Rust with zero `unsafe` blocks and three TODO markers total. ChaCha20Poly1305 + zeroize is pure Rust. `reqwest` is rustls-tls. ARM64 cross-compile via `cargo zigbuild` works because no OpenSSL anywhere.

5. **Test database orchestration.** `docker-compose.test.yml` with tmpfs Postgres on 5434 + `make test-db` + 55 `#[sqlx::test]` annotations. Real-DB integration tests are a habit, not an exception. The release notes for v0.26.0 claim 1122/1122 backend tests passing.

6. **ts-rs codegen with CI drift gate.** `register_ts_dto!` macro at struct definition sites + a CI job that runs `make gen-types` and fails on `git diff --exit-code`. UI never goes out of sync with backend types. The mechanism is deliberately not extended to channels/types.ts (a deficiency, but one chosen consciously).

7. **Memory subsystem.** 3-way RRF (semantic + FTS + trigram) with single-branch shortcut and deterministic tiebreaker. MMR reranking inside semantic search. Pinned vs raw tiers cleanly separated. Embedding delegated to Toolgate so Core never touches Ollama. ~1.5k LoC.

8. **Process manager handles native sidecars.** Channels (Bun) and Toolgate (Python) are managed as systemd-style child processes by Core, not Docker containers. Restart-on-crash is automatic, log capture goes through the broadcast tracing layer. This is the right call for a Pi deployment.

9. **Chaos and concurrency tests against real Pi.** [test-pi-chaos.py](../../tests/integration/pi/test-pi-chaos.py) exercises the SSE `Last-Event-ID` resume by simulating mid-stream drops. [test-pi-concurrency.py](../../tests/integration/pi/test-pi-concurrency.py) and [test-pi-trace-correlation.py](../../tests/integration/pi/test-pi-trace-correlation.py) cover other axes. Most projects only test the happy path.

10. **Spec-then-plan-then-implement discipline.** 68 design specs and 69 plan documents in `docs/superpowers/`, dated and roughly paired. Recent cadence is ~2 specs/week. The specs are not aspirational — they correspond to landed PRs and the comments in the code reference them by date.

## Weaknesses & risks

1. **Two parallel LLM-loop implementations (`pipeline::execute` vs `engine::stream::handle_isolated`).** Documented as Phase 66 deferred work; behavioural divergence is currently real (no fallback provider, no SessionCorruption recovery, no auto-continue on the unified path). Merging requires care because the unified path has stricter guarantees.

2. **`agent/providers.rs` is 2179 lines.** Contains the trait, sentinel, routing, DB-row-to-provider mapping, and tests. Sibling provider impl files (openai, anthropic, google, claude_cli, http) suggest the right factoring; `providers.rs` itself should become a thin trait + sentinel module.

3. **Six gateway handler god-modules (>1000 lines each).** `monitoring.rs` (1575), `chat.rs` (1420), `providers.rs` (1233), `channel_ws.rs` (1083), `sessions.rs` (1066), `agents/crud.rs` (1059). Pattern of sub-routers per resource is solid, but the resources are big. `agents/` already became a directory; same pattern needed for `chat/` and `monitoring/`.

4. **Channel WS contract is hand-mirrored.** [channels/src/types.ts](../../channels/src/types.ts) header says "Port of crates/hydeclaw-types/src/lib.rs:138-325". No schema, no codegen, no round-trip test. The single fragile cross-process boundary in the system. Drift is caught only by integration testing.

5. **`lib.rs` facade is a workaround tangle.** 329 lines mostly explaining why each `pub mod` exists, with `__memory_bridge`/`__memory_pipeline_bridge`/`memory_test_facade` `#[doc(hidden)]` modules existing solely so integration tests can reach leaf code without dragging the agent subtree in. The fact that the file caps itself at "10 modules" and every addition is justified by a comment is a signal — the underlying agent subtree wants to be exposed via a real lib API.

6. **Documentation drift in CLAUDE.md.** chat-store.ts described as 451 lines (actual 70). OTel versions described as 0.26-0.28 (actual 0.31/0.32). CLAUDE.md is the working source of truth for AI agents working on the codebase, so drift quietly degrades agent performance.

7. **Clippy `-D warnings` is `continue-on-error: true` in CI.** The job runs but failures are advisory. Almost certainly a leftover from a clippy upgrade.

8. **Migration churn (46 forward-only migrations, 8 are drop-column).** The 008 drop migrations (015, 027–033) reflect productive iteration but also that the schema was experimented on in production. No down migrations means the only escape from a bad migration is a forward fix. For a single-tenant Pi this is fine; for multi-tenant SaaS it would be a risk.

9. **`hydeclaw-db` and `hydeclaw-gateway-util` crates are extraction-driven.** Both exist mainly so integration tests can reach leaf modules without `lib.rs` cascading the full subtree. `hydeclaw-db/sessions.rs` is 1762 lines — bigger than most "monolithic" core modules. The crate split solves a build-system problem, not a domain problem.

10. **No formal API versioning.** URLs are unprefixed (`/api/agents`, not `/api/v1/agents`). No `Accept-Version` header. No deprecation policy. Acknowledged implicitly via v0.x version numbering, but every UI/channel adapter pinned to a specific Core version creates coupling that will hurt during the transition to 1.0.

## Suggestions (prioritised)

I'd tackle these in roughly this order:

### P0 — Closing existing decomposition bets

1. **Finish Phase 66** (per spec 2026-04-20). Consolidate `engine::stream::handle_isolated` into `pipeline::execute` so the cron RPC path goes through the same loop as SSE chat. This finally removes the "safe subset" caveat from `execute.rs` and lets the lib facade expose the unified path. Estimate: 1-2 weeks given the existing helper extraction.

2. **Split `agent/providers.rs`.** Extract `RoutingProvider` into `providers/routing.rs`, `build_provider*` into `providers/build.rs`, leave the trait + `UnconfiguredProvider` + the `LlmProvider` definition in `providers.rs`. This drops the largest file in the agent module by ~60% with zero behaviour change. Estimate: 1-2 days.

3. **Split the six handler god-modules.** Same pattern as `gateway/handlers/agents/` (which is already a directory): `gateway/handlers/chat/` (sse_emit, last_event_id_resume, abort, sync), `gateway/handlers/monitoring/` (dashboard, metrics, doctor, network), etc. Each sub-module stays at <500 lines. Estimate: 2-3 days per handler.

### P1 — Reducing fragility

4. **Codegen `channels/src/types.ts` from `hydeclaw-types`.** Extend the `register_ts_dto!` mechanism to emit a second file (`channels/src/types.generated.ts`) covering the `ChannelInbound`/`ChannelOutbound`/`IncomingMessageDto`/`ChannelActionDto` set. Add a CI drift gate. Removes the single hand-mirrored cross-process boundary in the system. Estimate: 2-3 days.

5. **Refresh CLAUDE.md.** Re-run the chat-store decomposition section against actual file sizes; update the OTel version references; note that `chat-store.ts` is now a 70-line wrapper over four action factories. Add a CI hint that warns when CLAUDE.md hasn't been touched in N weeks. Estimate: half a day, but needs the discipline of a recurring sweep.

6. **Promote `clippy -D warnings` to a hard CI gate.** Run it locally, fix the warnings (if any), remove `continue-on-error: true`. Estimate: depends on how many warnings exist today; my guess is small given the absence of `#[deprecated]` and the small TODO surface.

### P2 — Refactors that pay off later, not today

7. **Decide whether `hydeclaw-db` and `hydeclaw-gateway-util` are domain crates or test-extraction crates, and act on the answer.** Either rename them to reflect what they actually are, or split them by domain (sessions / memory / approvals / WAL). The current name promises a coherent layer that the contents don't quite deliver.

8. **Introduce a v1 API prefix.** Add `/api/v1/` aliases that route to the existing handlers; document the deprecation policy in `docs/API.md`. The UI and channels can keep using unprefixed paths for backward compat, and new clients use v1. Costs almost nothing today, saves a hard migration before 1.0.

9. **Consider a real public API surface for `hydeclaw-core`.** The `lib.rs` facade is the wrong shape for what it's used for. Define what an external consumer (e.g. an embedding Hydeclaw in a different binary) would actually need, expose those as proper `pub` modules, and let integration tests use the same surface as external consumers. This dissolves the `__memory_bridge` tangle and lets the Phase 66 work expose its results cleanly.

10. **Add a contract test for the Channel WS protocol.** Even before codegen, a test that round-trips every `ChannelInbound`/`ChannelOutbound` variant through Rust serde → JSON string → TS parser would catch drift at PR time. The TS side needs only `Bun.file` + `JSON.parse`.

## Verdict

**Maturity: 7.5/10.** Production-ready for the stated target — single-tenant Raspberry Pi-class deployment with a small number of agents and one or two human users. The hard parts are done well: streaming with idempotent replay, distributed tracing across four processes, ID-based message identity, ChaCha20Poly1305 secrets vault with zeroize, pgvector + FTS + trigram hybrid search, ARM64 cross-compile without OpenSSL, graceful shutdown drain, real-Pi chaos testing.

The gap to 9/10 is decomposition and contract-formalisation work: finishing the engine/pipeline unification (Phase 66), splitting the six handler god-modules, codegenning the channels WS types, and cleaning up the lib facade. None of this is urgent; all of it would pay off as the project scales beyond a single-Pi single-user deployment.

The codebase shows a small team working with strong engineering discipline: ADRs for architectural decisions, deferred-work documented in code comments with spec references, ts-rs CI drift gate, sqlx::test real-DB harness, chaos tests against real hardware. The dominant risks are size — `hydeclaw-core` at 80k LoC across 213 files is at the edge of what one crate should hold, and the workarounds in `lib.rs` are the canary. Address that and HydeClaw moves from "well-engineered Pi-class gateway" to "well-engineered platform that happens to deploy well on a Pi."
