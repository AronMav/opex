# Wave 1 — providers split (Anthropic / OpenAI / Google)

**Date:** 2026-05-14
**Wave:** W1 of refactoring roadmap (`docs/superpowers/specs/2026-05-14-refactoring-roadmap.md`)
**Status:** approved design, ready for plan

## Goal

Split the three largest LLM adapter files (`anthropic.rs`, `openai.rs`, `google.rs`) into focused sibling modules per concern. Surface the seams that already exist in the code — request build / response parse / streaming / tool-call accumulation / variant-specific quirks — without changing the `LlmProvider` trait contract or any wire-level behaviour.

The `LlmProvider` trait is stable since the earlier routing/registry split. This wave continues that established pattern (`factory.rs`, `registry.rs`, `routing.rs`, `error.rs`, `cancellable_stream.rs`, `timeouts.rs` are already extracted as siblings of `anthropic.rs` & `openai.rs`).

## Current state

```text
crates/hydeclaw-core/src/agent/providers/
├── mod.rs                       (LlmProvider trait, CallOptions, ModelOverride, …)
├── anthropic.rs                 1643 LoC  ← target
├── openai.rs                    1230 LoC  ← target
├── google.rs                     619 LoC  ← target (light split)
├── claude_cli.rs                 154 LoC  ← leave as-is
├── http.rs                       350 LoC  ← leave as-is
├── factory.rs                   (provider construction)
├── registry.rs                  (provider-type metadata, default URLs)
├── routing.rs                   (RoutingProvider impl, capability dispatch)
├── error.rs                     (LlmCallError, CancelReason)
├── cancellable_stream.rs        (CancelSlot, set_and_cancel)
├── timeouts.rs                  (TimeoutsConfig)
├── build_provider_tests.rs      (test artifact, not a megafile)
└── routing_tests.rs             (test artifact, not a megafile)
```

Public surface that *must not change*:

- `LlmProvider` trait (in `mod.rs`)
- `CallOptions`, `ModelOverride` types (in `mod.rs`)
- `AnthropicProvider`, `OpenAiCompatibleProvider`, `GoogleProvider` structs (re-exported from `mod.rs`)
- `messages_to_openai_format`, `messages_to_gemini_format` helpers (called from siblings via `pub(super)`)

## Target state

```text
crates/hydeclaw-core/src/agent/providers/
├── anthropic/
│   ├── mod.rs                   LlmProvider impl + AnthropicProvider struct + re-exports     (~250 LoC)
│   ├── request.rs               Message → Anthropic conversion, request body building       (~300 LoC)
│   ├── response.rs              AnthropicResponse / AnthropicContentBlock / AnthropicUsage,
│   │                            parse_anthropic_response                                     (~300 LoC)
│   ├── stream.rs                process_sse_event, StreamingAnthropicUsage,
│   │                            streaming_thinking_tests                                     (~350 LoC)
│   ├── tool_calls.rs            tool_use block parsing, partial-call accumulation            (~250 LoC)
│   └── thinking.rs              thinking_mode, thinking_config, thinking_config_tests        (~200 LoC)
├── openai/
│   ├── mod.rs                   LlmProvider impl + OpenAiCompatibleProvider struct           (~200 LoC)
│   ├── request.rs               messages_to_openai_format, request body building             (~250 LoC)
│   ├── response.rs              Chat completion parse, deserialize_null_as_empty_vec         (~200 LoC)
│   ├── stream.rs                SSE delta-chunk accumulator, StreamingUsage                  (~250 LoC)
│   ├── tool_calls.rs            OpenAI tool-call delta accumulation                          (~150 LoC)
│   └── minimax_xml.rs           extract_minimax_xml_tool_calls, parse_xml_*, xml_tests       (~250 LoC)
├── google/
│   ├── mod.rs                   LlmProvider impl + GoogleProvider struct + streaming inline  (~250 LoC)
│   ├── request.rs               messages_to_gemini_format, strip_empty_required, body build  (~200 LoC)
│   └── response.rs              Gemini response parse                                        (~200 LoC)
├── claude_cli.rs                (unchanged, 154 LoC)
├── http.rs                      (unchanged, 350 LoC)
└── mod.rs                       (unchanged: trait + types + re-exports of all adapters)
```

Per-module LoC estimates are derived from current file structure (functions and types map onto the proposed modules); actual sizes will surface during the extract commits and may shift ±15 % without renegotiating the design.

## Per-module responsibilities

### Anthropic

- **`mod.rs`** — public face of the adapter. `AnthropicProvider` struct, `impl LlmProvider`. Imports from siblings via `mod request; mod response; mod stream; mod tool_calls; mod thinking;`. Re-exports `pub use request::…`, `pub use response::…` only where call sites in `factory.rs` / `routing.rs` need it.
- **`request.rs`** — pure functions that turn `&[Message]` + `CallOptions` into the JSON body for `POST /v1/messages`. No HTTP. No `Arc<SecretsManager>`. Inputs/outputs are owned values.
- **`response.rs`** — non-streaming response parsing. `AnthropicResponse`, `AnthropicContentBlock`, `AnthropicUsage` types. `parse_anthropic_response(api_resp, model) -> LlmResponse`.
- **`stream.rs`** — SSE event handling. `process_sse_event`, `process_sse_events_for_test`, `parse_streaming_usage_for_test`. `StreamingAnthropicUsage` aggregator. Owns the `streaming_thinking_tests` submodule (it tests this code).
- **`tool_calls.rs`** — tool_use content blocks: extraction from non-streaming responses, delta accumulation in streaming. Used by both `response.rs` and `stream.rs`.
- **`thinking.rs`** — `thinking_mode(model: &str) -> ThinkingMode` and `thinking_config(level, model, max_tokens) -> Option<Value>`. Owns the `thinking_config_tests` submodule.

### OpenAI

- **`mod.rs`** — `OpenAiCompatibleProvider` struct, `impl LlmProvider`. Sibling-module declarations.
- **`request.rs`** — `messages_to_openai_format(messages)`, body building including model-specific overrides (`o1-*` reasoning, `gpt-4o-*` defaults). `pub(super)` only.
- **`response.rs`** — non-streaming chat completion parsing. `deserialize_null_as_empty_vec` helper.
- **`stream.rs`** — SSE chunk accumulator. `StreamingUsage`. `From<StreamingUsage> for hydeclaw_types::TokenUsage`.
- **`tool_calls.rs`** — OpenAI tool-call delta merging (function name + arguments built across chunks). Pure functions, called from `stream.rs`.
- **`minimax_xml.rs`** — `extract_minimax_xml_tool_calls`, `parse_xml_invoke_blocks`, `parse_xml_parameters`, `xml_extract_attr`. Owns the `xml_tests` submodule.

### Google

- **`mod.rs`** — `GoogleProvider` struct, `impl LlmProvider`, streaming inline (small enough). Sibling declarations.
- **`request.rs`** — `messages_to_gemini_format`, `strip_empty_required`, body building.
- **`response.rs`** — Gemini response parsing.

## Migration mechanics

Linear sequence of small, independently-buildable commits. Every commit passes `cargo clippy --all-targets -- -D warnings` and `cargo test --workspace`.

1. **Discovery commit** — `chore(providers): freeze test baseline before W1 refactor`
   - Run `cargo llvm-cov test -p hydeclaw-core agent::providers` (or `cargo-tarpaulin` if simpler on Windows); attach an LCOV snapshot to the discovery commit as `docs/architecture/2026-05-14-providers-coverage-baseline.lcov` (or inline summary in commit message)
   - Identify under-tested branches (likely candidates: Anthropic `redacted_thinking`/`server_tool_use` content blocks; MiniMax XML edge cases; Gemini `safetyRatings`)
   - Add golden-fixture tests inline (`#[cfg(test)] mod golden_fixtures` in each existing megafile) covering each gap. These tests must be co-moved with their production code during extraction.
   - Commit baseline + new tests as a single commit.

Each extract commit does only this: create the new file, `mv` the relevant items into it, add the `mod foo;` line in `mod.rs`, update `use super::…` paths inside the moved code, fix any `pub(super)` visibility needed for cross-module calls. No rewriting. Test set unchanged.

**Why the rename-to-`mod.rs` commit comes first.** Rust allows `mod foo;` to resolve to either `foo.rs` *or* `foo/mod.rs`, but not both — introducing a sibling `foo/request.rs` while the parent is still `foo.rs` would be a compile error. So for each adapter the sequence is: (a) `git mv adapter.rs adapter/mod.rs` as its own commit (mechanical, no content change), (b) then extract siblings out of `mod.rs` one at a time.

1. **Anthropic — 6 commits**
   - `refactor(providers/anthropic): rename anthropic.rs to anthropic/mod.rs` (git mv only)
   - `refactor(providers/anthropic): extract request to anthropic/request.rs`
   - `refactor(providers/anthropic): extract response to anthropic/response.rs`
   - `refactor(providers/anthropic): extract stream to anthropic/stream.rs`
   - `refactor(providers/anthropic): extract tool_calls to anthropic/tool_calls.rs`
   - `refactor(providers/anthropic): extract thinking to anthropic/thinking.rs`

2. **OpenAI — 6 commits** (same pattern)
   - `refactor(providers/openai): rename openai.rs to openai/mod.rs`
   - `refactor(providers/openai): extract request`
   - `refactor(providers/openai): extract response`
   - `refactor(providers/openai): extract stream`
   - `refactor(providers/openai): extract tool_calls`
   - `refactor(providers/openai): extract minimax_xml`

3. **Google — 3 commits**
   - `refactor(providers/google): rename google.rs to google/mod.rs`
   - `refactor(providers/google): extract request`
   - `refactor(providers/google): extract response`

4. **Cleanup commit** — `chore(providers): tighten visibility after W1 split`
   - Narrow `pub` → `pub(crate)` / `pub(super)` where the new module boundaries allow.
   - Drop any temporary `pub use` re-exports that were needed mid-sequence.
   - Single coherent diff, easily revertible if a downstream consumer breaks.

5. **Acceptance commit** — `chore(providers): W1 refactor acceptance`
   - Run full test matrix locally (clippy + tests + rustls invariant via cargo tree)
   - Drop any golden-fixture tests that became redundant after the split
   - Commit-message body summarises new module tree

Total: **~18 commits** (1 discovery + 6 Anthropic + 6 OpenAI + 3 Google + cleanup + acceptance).

## Test guards

**Pre-existing (must keep passing through every commit):**

- `agent::providers::anthropic::tests` (request building, response parsing)
- `agent::providers::anthropic::thinking_config_tests` (thinking helpers)
- `agent::providers::anthropic::streaming_thinking_tests` (SSE streaming)
- `agent::providers::openai::xml_tests` (MiniMax XML parsing)
- Any inline `#[cfg(test)] mod tests` in `google.rs`
- `tests/integration_mock_provider.rs` (provider trait contract)
- `tests/integration_aborted_usage.rs` (streaming-usage invariant)

**New (added in discovery commit):**

- Golden-fixture tests covering under-tested branches identified by `cargo llvm-cov`. Concrete candidates to verify during discovery:
  - Anthropic `content_block_delta` for `thinking` and `redacted_thinking` (rare blocks)
  - Anthropic `server_tool_use` content blocks
  - OpenAI MiniMax XML with multiple `<invoke>` blocks in one stream
  - OpenAI tool-call delta where `arguments` arrives across 3+ chunks
  - Google `safetyRatings` block in response (must not crash parser)

These are added *before* the first extract and removed if redundant in the acceptance commit.

## Risks and mitigations

| Risk | Mitigation |
| ---- | ---------- |
| `mod tests`-style submodules disconnect from their production code during extract | Run `cargo test agent::providers --no-run` after each extract; CI green is mandatory between commits |
| `pub(super)` shrink in cleanup commit breaks `routing.rs` / `factory.rs` | Cleanup is the **last** commit; can be reverted independently. Audit `factory.rs` and `routing.rs` imports before tightening |
| `parse_streaming_usage_for_test` (test-only function) lives in production module after extract | The function is `#[cfg(test)] pub` — it travels with the test it serves, into `stream.rs`. Verify in discovery commit that test still resolves it post-move |
| File-rename commits (`anthropic.rs` → `anthropic/mod.rs`) hide diffs | Each rename is its own commit with `git mv`. The follow-up extract commit shows the actual content delta cleanly |
| Hot-reload / hot-restart paths import these adapters via `factory::create_chat_provider*` | `factory.rs` imports the adapter structs (`AnthropicProvider`, etc.) via `pub use` from `mod.rs`. As long as `mod.rs` re-exports the struct, factory call sites compile unchanged. Verified in discovery commit |

## Acceptance criteria (Wave 1)

- All 15 commits build independently (`cargo check -p hydeclaw-core` clean)
- `cargo clippy --all-targets -- -D warnings` clean at every commit
- `cargo test --workspace` baseline failures unchanged (same 3 pre-existing failures as before W1: 2 stale dto snapshots if not yet fixed; 1 sqlx VersionMismatch on local DB)
- `cargo tree --workspace | grep -E 'openssl-sys|native-tls'` returns nothing (rustls invariant)
- Public surface unchanged: `mod.rs` re-exports identical to pre-W1
- Module-tree summary in acceptance commit message

## Out of scope (deliberately)

- **`claude_cli.rs` (154 LoC)** — already under threshold.
- **`http.rs` (350 LoC)** — already under threshold.
- **New common helpers** between adapters — deferred to a follow-up wave if duplication remains after split. Anthropic's event-typed SSE and OpenAI's delta-chunk SSE are different enough that premature abstraction is the larger risk.
- **`LlmProvider` trait shape** — frozen during W1.
- **`build_provider_tests.rs` / `routing_tests.rs`** — testing artifacts, not megafiles.
- **Behavioural changes** of any kind. No new fields, no new error variants, no new event types. Renaming an internal function is allowed only if the new name is a strict improvement *and* the rename lives in its own commit.

## Effort

~2-3 focused days:

- Discovery commit: 4-6 hours (coverage measurement, golden-fixture authoring is the slow part)
- Extract commits: ~30 minutes each × ~14 = 7 hours
- Cleanup + acceptance: 2-3 hours

## Next step

Hand off to `superpowers:writing-plans` to expand this design into a step-by-step plan: per-commit `git mv` source/destination, per-commit visibility delta, per-commit test command to run, per-commit expected diff shape. The plan should be detailed enough that an engineer (or subagent) can execute it linearly without re-deciding any design point.
