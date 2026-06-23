# Wave 1 — Providers split implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Split three large LLM adapter files (`anthropic.rs` 1643, `openai.rs` 1230, `google.rs` 619 LoC) into 15 focused sibling modules without changing any wire-level behaviour.

**Architecture:** For each adapter, `git mv adapter.rs adapter/mod.rs` first (so Rust's `mod adapter;` keeps resolving), then extract sibling modules one at a time. Public surface (`LlmProvider` trait, provider structs re-exported via `mod.rs` of the providers directory) is frozen. Internal `pub(super)` visibility is narrowed in a single final cleanup commit.

**Tech Stack:** Rust 2024 edition, cargo, rustls (no OpenSSL), tokio, serde, reqwest.

**Spec:** `docs/superpowers/specs/2026-05-14-providers-w1-design.md`. Read it once before starting; this plan references its decisions verbatim and does not re-derive them.

**Total commits:** 18. **Estimated time:** 2-3 focused days.

---

## Standard extract procedure

Every Task 2-16 (extract commits) follows the same shape. The steps are:

1. **Read** the source file with `Read` tool to confirm current line ranges of the items being moved.
2. **Edit** the source file to remove the items (Edit tool, replacing the items' text with empty content) — or use a single Edit that moves them. The cleanest approach is: first Write the new file with the moved items + their imports, then Edit the source file to delete those items.
3. **Add** `mod <name>;` (and `use <name>::*;` if the parent needs anything from it) to the parent module.
4. **Update visibility** for items now crossing the new module boundary: change `pub(super)` → `pub(super)` (no-op, stays correct since parent is still `super`), `fn foo` → `pub(super) fn foo` (so siblings can see it), etc. Inside the new sibling, any item used by *another* sibling (not just by `mod.rs`) needs `pub(super)` too.
5. **`cargo check -p opex-core`** — fast feedback.
6. **`cargo clippy -p opex-core --all-targets -- -D warnings`** — strict.
7. **`cargo test -p opex-core --bin opex-core agent::providers`** — adapter-suite, no DB needed.
8. **Commit** with the exact message specified in the task.

If clippy fails on something **not introduced** by the move (pre-existing warning the move surfaced because of a new module boundary), fix it minimally in the same commit — keep the diff focused.

If a test fails, **stop** and investigate before continuing. The whole premise of W1 is no behavioural change; a test regression means the move broke an invariant.

---

## Task 1: Discovery commit

**Files:**

- Create (transient): `target/llvm-cov-providers-baseline.txt` (not committed)
- Modify: `crates/opex-core/src/agent/providers/anthropic.rs` — add `#[cfg(test)] mod golden_fixtures` block
- Modify: `crates/opex-core/src/agent/providers/openai.rs` — add `#[cfg(test)] mod tests` + `#[cfg(test)] mod golden_fixtures` blocks
- Modify: `crates/opex-core/src/agent/providers/google.rs` — add `#[cfg(test)] mod golden_fixtures` block

- [ ] **Step 1: Inventory current test modules**

Run:

```bash
grep -nE "^#\[cfg\(test\)\]|^mod [a-z_]*tests" \
  crates/opex-core/src/agent/providers/anthropic.rs \
  crates/opex-core/src/agent/providers/openai.rs \
  crates/opex-core/src/agent/providers/google.rs
```

Expected output includes (locked baseline, 2026-05-14):

```
anthropic.rs: mod tests (line 962), mod thinking_config_tests (line 1480), mod streaming_thinking_tests (line 1593)
openai.rs:    mod xml_tests (line 1109)
google.rs:    mod tests (line 597)
```

Record the actual line numbers in the commit body — any future drift is justification for re-running this inventory.

- [ ] **Step 2: Measure baseline coverage**

Run (no commit of artifacts):

```bash
cargo llvm-cov test -p opex-core --bin opex-core agent::providers --summary-only
```

Capture per-file coverage % from the summary. Numbers go into the commit-message body — no LCOV file is committed.

If `cargo-llvm-cov` is not installed:

```bash
cargo install cargo-llvm-cov
rustup component add llvm-tools-preview
```

- [ ] **Step 3: Add OpenAI baseline `mod tests`**

Insert at the END of `crates/opex-core/src/agent/providers/openai.rs`, immediately before the existing `mod xml_tests` block (so the two test modules sit adjacent):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_null_as_empty_vec_handles_null() {
        #[derive(Deserialize)]
        struct Holder {
            #[serde(deserialize_with = "deserialize_null_as_empty_vec")]
            items: Vec<String>,
        }
        let h: Holder = serde_json::from_str(r#"{"items": null}"#).unwrap();
        assert!(h.items.is_empty());
    }

    #[test]
    fn deserialize_null_as_empty_vec_handles_array() {
        #[derive(Deserialize)]
        struct Holder {
            #[serde(deserialize_with = "deserialize_null_as_empty_vec")]
            items: Vec<String>,
        }
        let h: Holder = serde_json::from_str(r#"{"items": ["a", "b"]}"#).unwrap();
        assert_eq!(h.items, vec!["a", "b"]);
    }

    #[test]
    fn streaming_usage_to_token_usage_includes_cache_fields() {
        let s = StreamingUsage {
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
            prompt_tokens_details: Some(ChatPromptTokensDetails {
                cached_tokens: Some(30),
            }),
            completion_tokens_details: None,
        };
        let tu: opex_types::TokenUsage = s.into();
        assert_eq!(tu.input_tokens, 100);
        assert_eq!(tu.output_tokens, 50);
        assert_eq!(tu.cache_read_input_tokens, Some(30));
    }
}
```

Note: field names in the struct-literal above (`prompt_tokens_details`, `cached_tokens`) must match the actual struct definitions in `openai.rs` (see lines 938-944). If the engineer finds a mismatch (e.g. field renamed since this plan was written), update the test to match the struct — do **not** rename the production fields.

- [ ] **Step 4: Add golden-fixtures stubs**

Add to the END of each adapter file (after all existing `#[cfg(test)] mod ...` blocks):

`crates/opex-core/src/agent/providers/anthropic.rs`:

```rust
#[cfg(test)]
mod golden_fixtures {
    use super::*;

    /// Regression: Anthropic content_block_delta of type "thinking" must
    /// parse without crashing the SSE handler.
    #[test]
    fn content_block_delta_thinking_parses() {
        let lines = vec![
            r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#.to_string(),
            r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Hmm..."}}"#.to_string(),
            r#"data: {"type":"content_block_stop","index":0}"#.to_string(),
        ];
        let usage = parse_streaming_usage_for_test(&lines);
        // We only assert the parser did not panic and returned a value.
        let _ = usage;
    }

    /// Regression: Anthropic tool_use content block in non-streaming response.
    #[test]
    fn tool_use_block_in_response_parses() {
        let raw = r#"{
            "id": "msg_x",
            "type": "message",
            "role": "assistant",
            "model": "claude-3-5-sonnet",
            "content": [
                {"type": "text", "text": "Calling tool"},
                {"type": "tool_use", "id": "toolu_1", "name": "search", "input": {"q": "rust"}}
            ],
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 10, "output_tokens": 5}
        }"#;
        let parsed: AnthropicResponse = serde_json::from_str(raw).unwrap();
        let resp = parse_anthropic_response(parsed, "claude-3-5-sonnet");
        assert_eq!(resp.tool_calls.len(), 1);
        assert_eq!(resp.tool_calls[0].function.name, "search");
    }
}
```

`crates/opex-core/src/agent/providers/openai.rs`:

```rust
#[cfg(test)]
mod golden_fixtures {
    use super::*;

    /// Regression: MiniMax XML with two `<invoke>` blocks in one response
    /// must yield two tool calls.
    #[test]
    fn minimax_xml_two_invoke_blocks() {
        let content = r#"prefix <tool_use>
<invoke name="alpha"><parameter name="x">1</parameter></invoke>
<invoke name="beta"><parameter name="y">2</parameter></invoke>
</tool_use> suffix"#;
        let mut out = Vec::new();
        extract_minimax_xml_tool_calls(content, &mut out);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].function.name, "alpha");
        assert_eq!(out[1].function.name, "beta");
    }

    /// Regression: parse_xml_parameters with no <parameter> tags returns
    /// an empty map rather than crashing.
    #[test]
    fn xml_parameters_empty_body() {
        let mut params = serde_json::Map::new();
        parse_xml_parameters("", &mut params);
        assert!(params.is_empty());
    }
}
```

`crates/opex-core/src/agent/providers/google.rs`:

```rust
#[cfg(test)]
mod golden_fixtures {
    use super::*;

    /// Regression: Gemini response with `safetyRatings` block must parse
    /// without erroring (the field is ignored, not required).
    #[test]
    fn safety_ratings_block_does_not_crash_parser() {
        let raw = r#"{
            "candidates": [{
                "content": {"role": "model", "parts": [{"text": "hi"}]},
                "safetyRatings": [
                    {"category": "HARM_CATEGORY_HARASSMENT", "probability": "NEGLIGIBLE"}
                ]
            }],
            "usageMetadata": {"promptTokenCount": 5, "candidatesTokenCount": 1, "totalTokenCount": 6}
        }"#;
        let parsed: GeminiResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(parsed.candidates.len(), 1);
    }
}
```

If any of these tests references a struct field whose actual name differs (e.g. `GeminiResponse.candidates` vs `.candidate`), adjust the test — do not rename the struct.

- [ ] **Step 5: Verify the new tests pass**

```bash
cargo test -p opex-core --bin opex-core "agent::providers::anthropic::golden_fixtures::|agent::providers::openai::tests::|agent::providers::openai::golden_fixtures::|agent::providers::google::golden_fixtures::"
```

Expected: all new tests PASS. If a test fails with a struct-field error, this means the test was written against the wrong field name — fix the test, not the struct.

- [ ] **Step 6: Run full provider test suite**

```bash
cargo test -p opex-core --bin opex-core agent::providers
```

Expected: all tests pass.

- [ ] **Step 7: Commit**

```bash
git add crates/opex-core/src/agent/providers/anthropic.rs \
        crates/opex-core/src/agent/providers/openai.rs \
        crates/opex-core/src/agent/providers/google.rs
git commit -m "$(cat <<'EOF'
chore(providers): freeze test baseline before W1 refactor

Inventory of test modules locked at 2026-05-14:
* anthropic.rs: mod tests + mod thinking_config_tests +
  mod streaming_thinking_tests
* openai.rs:    mod xml_tests (only — gap below)
* google.rs:    mod tests

Gap closed: OpenAI adapter had no non-XML mod tests. Added baseline
mod tests covering deserialize_null_as_empty_vec round-trip and
StreamingUsage -> TokenUsage conversion (including cache_read fields).

Added one #[cfg(test)] mod golden_fixtures per adapter (3 modules
total) covering currently under-tested branches:
* Anthropic: content_block_delta of type "thinking", tool_use block
  in non-streaming response
* OpenAI:    MiniMax XML with two <invoke> blocks; parse_xml_parameters
  with empty body
* Google:    response with safetyRatings block (must not crash parser)

These tests travel with their production code through the W1 split.
Acceptance commit may drop any that become redundant.

Coverage baseline (cargo llvm-cov, summary-only):
[paste per-file % here from Step 2 — engineer fills in]
EOF
)"
```

---

## Task 2: Anthropic — rename to mod.rs

**Files:**

- Rename: `crates/opex-core/src/agent/providers/anthropic.rs` → `crates/opex-core/src/agent/providers/anthropic/mod.rs`

- [ ] **Step 1: Create the destination directory**

```bash
mkdir crates/opex-core/src/agent/providers/anthropic
```

- [ ] **Step 2: Perform the rename via git mv**

```bash
git mv crates/opex-core/src/agent/providers/anthropic.rs \
       crates/opex-core/src/agent/providers/anthropic/mod.rs
```

Note: this is a single commit with **no content change**. The `git mv` preserves history.

- [ ] **Step 3: Verify the build is unchanged**

```bash
cargo check -p opex-core
cargo test -p opex-core --bin opex-core agent::providers::anthropic
```

Expected: clean build, all tests pass. Rust resolves `mod anthropic;` to either `anthropic.rs` or `anthropic/mod.rs` automatically.

- [ ] **Step 4: Commit**

```bash
git commit -m "refactor(providers/anthropic): rename anthropic.rs to anthropic/mod.rs"
```

---

## Task 3: Anthropic — extract `thinking.rs`

**Files:**

- Create: `crates/opex-core/src/agent/providers/anthropic/thinking.rs`
- Modify: `crates/opex-core/src/agent/providers/anthropic/mod.rs`

The `thinking` extraction goes first because it is the most self-contained piece of Anthropic (no calls into other Anthropic internals).

**Items to move (from `anthropic/mod.rs`):**

- `enum ThinkingMode` (line ~9)
- `fn thinking_mode(model: &str) -> ThinkingMode` (line ~18)
- `fn thinking_config(level, model, effective_max_tokens) -> Option<serde_json::Value>` (line ~30)
- `#[cfg(test)] mod thinking_config_tests` (line ~1480)

- [ ] **Step 1: Read current location of the items to be moved**

```bash
grep -nE "^(enum ThinkingMode|fn thinking_mode|fn thinking_config|mod thinking_config_tests)" \
  crates/opex-core/src/agent/providers/anthropic/mod.rs
```

Expected: 4 matching lines. Note exact line numbers — they replace the "~" estimates above.

- [ ] **Step 2: Create the new file**

`crates/opex-core/src/agent/providers/anthropic/thinking.rs`:

```rust
//! Anthropic-specific "thinking" (extended reasoning) helpers. Decides
//! whether a model supports thinking, and builds the request-side config
//! block when it does.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ThinkingMode {
    // [paste the variants from anthropic/mod.rs verbatim]
}

pub(super) fn thinking_mode(model: &str) -> ThinkingMode {
    // [paste body verbatim from anthropic/mod.rs]
}

pub(super) fn thinking_config(
    level: u8,
    model: &str,
    effective_max_tokens: u32,
) -> Option<serde_json::Value> {
    // [paste body verbatim from anthropic/mod.rs]
}

#[cfg(test)]
mod thinking_config_tests {
    // [paste the entire mod block contents verbatim — including its `use super::*;`]
}
```

Visibility note: change `enum ThinkingMode { ... }` (private) → `pub(super) enum ThinkingMode { ... }`; same for `thinking_mode` and `thinking_config`. The `pub(super)` is required so `anthropic/mod.rs` can still call them.

- [ ] **Step 3: Remove the moved items from `anthropic/mod.rs`**

Use Edit to delete the four blocks identified in Step 1. Replace each with nothing (empty string).

- [ ] **Step 4: Add `mod thinking;` declaration to `anthropic/mod.rs`**

Near the top of `anthropic/mod.rs` (after the `use super::{…}` import line), add:

```rust
mod thinking;
use thinking::{ThinkingMode, thinking_mode, thinking_config};
```

This brings the three items back into `mod.rs`'s namespace so existing call sites compile without changes.

- [ ] **Step 5: Verify the build**

```bash
cargo check -p opex-core
```

Expected: clean. If you see `unresolved name` for `ThinkingMode`/`thinking_mode`/`thinking_config`, recheck Step 4's `use` line.

- [ ] **Step 6: Run clippy + tests**

```bash
cargo clippy -p opex-core --all-targets -- -D warnings
cargo test -p opex-core --bin opex-core agent::providers::anthropic
```

Expected: all pass. `thinking_config_tests` runs from its new home with no regression.

- [ ] **Step 7: Commit**

```bash
git add crates/opex-core/src/agent/providers/anthropic/mod.rs \
        crates/opex-core/src/agent/providers/anthropic/thinking.rs
git commit -m "refactor(providers/anthropic): extract thinking to anthropic/thinking.rs"
```

---

## Task 4: Anthropic — extract `response.rs`

**Files:**

- Create: `crates/opex-core/src/agent/providers/anthropic/response.rs`
- Modify: `crates/opex-core/src/agent/providers/anthropic/mod.rs`

**Items to move from `anthropic/mod.rs`:**

- `pub(super) struct AnthropicResponse` (line ~377)
- `pub(super) enum AnthropicContentBlock` (line ~385)
- `pub(super) struct AnthropicUsage` (line ~401)
- `pub(super) fn parse_anthropic_response` (line ~448)
- The `tool_use_block_in_response_parses` golden-fixture test (move WITH `parse_anthropic_response` since that test calls it)

Keep in `mod.rs`: anything that constructs `AnthropicResponse` from raw bytes (HTTP path).

- [ ] **Step 1: Read current ranges**

```bash
grep -nE "^pub\(super\) (struct AnthropicResponse|enum AnthropicContentBlock|struct AnthropicUsage|fn parse_anthropic_response)" \
  crates/opex-core/src/agent/providers/anthropic/mod.rs
```

- [ ] **Step 2: Create `response.rs`**

```rust
//! Non-streaming Anthropic response types and parser.
//!
//! Owns the JSON shape returned by `POST /v1/messages` (non-streaming
//! variant) and the conversion into `LlmResponse`.

use opex_types::LlmResponse;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub(super) struct AnthropicResponse {
    // [paste fields verbatim from mod.rs]
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum AnthropicContentBlock {
    // [paste variants verbatim]
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct AnthropicUsage {
    // [paste fields verbatim]
}

pub(super) fn parse_anthropic_response(
    api_resp: AnthropicResponse,
    model: &str,
) -> LlmResponse {
    // [paste body verbatim from mod.rs]
}
```

Note: the existing `#[cfg(test)]` annotation on the re-export in `agent/providers/mod.rs` (line 35: `#[cfg(test)] use anthropic::{AnthropicContentBlock, AnthropicResponse, AnthropicUsage, parse_anthropic_response};`) must continue to resolve. Since `mod.rs` re-exports `pub(super) use response::{AnthropicContentBlock, AnthropicResponse, AnthropicUsage, parse_anthropic_response};`, the test-only path keeps working.

- [ ] **Step 3: Delete moved items from `anthropic/mod.rs`**

Use Edit to remove the four definitions identified in Step 1.

- [ ] **Step 4: Move the `tool_use_block_in_response_parses` golden-fixture test**

Find the `#[test] fn tool_use_block_in_response_parses` block inside `#[cfg(test)] mod golden_fixtures` in `anthropic/mod.rs`. Move it to a new `#[cfg(test)] mod tests` block at the bottom of `response.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_use_block_in_response_parses() {
        // [paste body verbatim]
    }
}
```

Delete the function from `golden_fixtures` in `mod.rs`. If `golden_fixtures` becomes empty after this, leave the empty `mod` block — the next extract may use it.

- [ ] **Step 5: Add `mod response;` declaration to `anthropic/mod.rs`**

Near other `mod ...;` lines (after `mod thinking;`):

```rust
mod response;
pub(super) use response::{AnthropicContentBlock, AnthropicResponse, AnthropicUsage, parse_anthropic_response};
```

The `pub(super) use` re-export is critical: `providers/mod.rs` line 35 imports these names via `use anthropic::{…}`.

- [ ] **Step 6: Build + test**

```bash
cargo check -p opex-core
cargo clippy -p opex-core --all-targets -- -D warnings
cargo test -p opex-core --bin opex-core agent::providers::anthropic
```

Expected: clean.

- [ ] **Step 7: Commit**

```bash
git add crates/opex-core/src/agent/providers/anthropic/mod.rs \
        crates/opex-core/src/agent/providers/anthropic/response.rs
git commit -m "refactor(providers/anthropic): extract response types and parser to anthropic/response.rs"
```

---

## Task 5: Anthropic — extract `stream.rs`

**Files:**

- Create: `crates/opex-core/src/agent/providers/anthropic/stream.rs`
- Modify: `crates/opex-core/src/agent/providers/anthropic/mod.rs`

**Items to move from `anthropic/mod.rs`:**

- `struct StreamingAnthropicUsage` + `impl StreamingAnthropicUsage` (lines ~413-446)
- `struct ThinkingState` (line ~512)
- `fn process_sse_event` (line ~528)
- `#[cfg(test)] fn process_sse_events_for_test` (line ~638)
- `#[cfg(test)] fn parse_streaming_usage_for_test` (line ~665)
- `#[cfg(test)] mod streaming_thinking_tests` (line ~1593)
- The `content_block_delta_thinking_parses` golden-fixture test (move with `parse_streaming_usage_for_test`)

- [ ] **Step 1: Read current ranges + cross-module call audit**

```bash
grep -nE "^(struct StreamingAnthropicUsage|impl StreamingAnthropicUsage|struct ThinkingState|fn process_sse_event|fn process_sse_events_for_test|fn parse_streaming_usage_for_test|mod streaming_thinking_tests)" \
  crates/opex-core/src/agent/providers/anthropic/mod.rs
grep -nE "thinking_mode\(|thinking_config\(|AnthropicContentBlock|AnthropicUsage" \
  crates/opex-core/src/agent/providers/anthropic/mod.rs | head -20
```

The second grep tells you which functions inside the streaming code call into `thinking` or `response`. These calls become cross-module after extraction and need visibility.

- [ ] **Step 2: Create `stream.rs`**

```rust
//! Anthropic SSE streaming: event-typed delta accumulation.
//!
//! Owns the state machine that turns `content_block_start` /
//! `content_block_delta` / `content_block_stop` events into a streaming
//! `LlmResponse`. Aggregates token usage across `message_start` and
//! `message_delta` events.

use serde::Deserialize;

use super::response::{AnthropicContentBlock, AnthropicUsage};
use super::thinking::{ThinkingMode, thinking_mode};

#[derive(Debug, Default, Clone)]
struct StreamingAnthropicUsage {
    // [paste fields verbatim]
}

impl StreamingAnthropicUsage {
    // [paste impl block verbatim]
}

#[derive(Debug, Default, Clone)]
struct ThinkingState {
    // [paste fields verbatim]
}

pub(super) fn process_sse_event(
    // [paste full signature + body verbatim]
) {
    // ...
}

#[cfg(test)]
pub(super) fn process_sse_events_for_test(
    // [paste full signature + body verbatim]
) {
    // ...
}

#[cfg(test)]
pub(super) fn parse_streaming_usage_for_test(
    lines: &[String],
) -> Option<opex_types::TokenUsage> {
    // [paste body verbatim]
}

#[cfg(test)]
mod streaming_thinking_tests {
    // [paste entire mod contents verbatim]
}

#[cfg(test)]
mod golden_fixtures {
    use super::*;

    #[test]
    fn content_block_delta_thinking_parses() {
        // [paste body from old golden_fixtures in mod.rs]
    }
}
```

Visibility notes:

- `process_sse_event` becomes `pub(super) fn` so `LlmProvider::chat_stream` in `mod.rs` can still call it. (Was private; sibling-module access requires `pub(super)`.)
- `process_sse_events_for_test` and `parse_streaming_usage_for_test` become `#[cfg(test)] pub(super) fn` — tests in other Anthropic siblings (none today, but future-proof) and in `mod.rs` can still resolve them.
- `StreamingAnthropicUsage` and `ThinkingState` stay private to `stream.rs`.

If `process_sse_event` calls `parse_anthropic_response` or any `AnthropicContentBlock` variant directly, the `use super::response::{…}` line in `stream.rs` handles it.

- [ ] **Step 3: Delete moved items from `anthropic/mod.rs`**

- [ ] **Step 4: Add `mod stream;` declaration**

```rust
mod stream;
use stream::process_sse_event;
#[cfg(test)]
#[allow(unused_imports)]
use stream::{process_sse_events_for_test, parse_streaming_usage_for_test};
```

- [ ] **Step 5: Build + test**

```bash
cargo check -p opex-core
cargo clippy -p opex-core --all-targets -- -D warnings
cargo test -p opex-core --bin opex-core agent::providers::anthropic
```

The `streaming_thinking_tests` module should run from its new home with no regression.

- [ ] **Step 6: Commit**

```bash
git add crates/opex-core/src/agent/providers/anthropic/mod.rs \
        crates/opex-core/src/agent/providers/anthropic/stream.rs
git commit -m "refactor(providers/anthropic): extract SSE streaming to anthropic/stream.rs"
```

---

## Task 6: Anthropic — extract `tool_calls.rs`

**Files:**

- Create: `crates/opex-core/src/agent/providers/anthropic/tool_calls.rs`
- Modify: `crates/opex-core/src/agent/providers/anthropic/mod.rs`
- Modify: `crates/opex-core/src/agent/providers/anthropic/response.rs`
- Modify: `crates/opex-core/src/agent/providers/anthropic/stream.rs`

**Items to identify and move:**

The Anthropic tool-call code is spread across `response.rs` (non-streaming `tool_use` block extraction) and `stream.rs` (streaming `input_json_delta` accumulation). This task isolates the **shared helpers** that both paths use into `tool_calls.rs`. The call sites in `response.rs` and `stream.rs` remain, calling into the new module.

- [ ] **Step 1: Identify shared helpers**

Run:

```bash
grep -nE "tool_use|input_json_delta|ToolCall" \
  crates/opex-core/src/agent/providers/anthropic/mod.rs \
  crates/opex-core/src/agent/providers/anthropic/response.rs \
  crates/opex-core/src/agent/providers/anthropic/stream.rs
```

Look for:

- Helper functions called from *both* `parse_anthropic_response` and `process_sse_event`. These are the prime move candidates.
- Local state structs/types for partial tool calls (e.g. an `id`/`name`/`input_json_buffer` triple that accumulates during streaming).

If after the grep you find that all tool-call code is **uniquely local** to its caller (one helper in response, one in stream, no shared code), then `tool_calls.rs` makes less sense — in that case, document the finding in the commit body and **skip this commit** (close as no-op):

```bash
git commit --allow-empty -m "refactor(providers/anthropic): no shared tool-call helpers found; skipping extract"
```

Continue to Task 7.

- [ ] **Step 2: If shared helpers exist, create `tool_calls.rs`**

```rust
//! Anthropic tool_use content-block handling shared by streaming and
//! non-streaming response paths.

use serde::Deserialize;

// [paste shared helper fns/types here]
```

Make the moved items `pub(super)` so siblings can call them.

- [ ] **Step 3: Update call sites in `response.rs` and `stream.rs`**

Add `use super::tool_calls::{…};` to each consumer and rewrite the local call to the new path.

- [ ] **Step 4: Add `mod tool_calls;` to `anthropic/mod.rs`**

```rust
mod tool_calls;
```

No re-export needed if nothing outside `anthropic/` consumes it.

- [ ] **Step 5: Build + test**

```bash
cargo check -p opex-core
cargo clippy -p opex-core --all-targets -- -D warnings
cargo test -p opex-core --bin opex-core agent::providers::anthropic
```

- [ ] **Step 6: Commit**

If extract happened:

```bash
git add crates/opex-core/src/agent/providers/anthropic/
git commit -m "refactor(providers/anthropic): extract tool_use helpers to anthropic/tool_calls.rs"
```

If skipped (empty commit per Step 1):

```bash
# already committed in Step 1
```

---

## Task 7: Anthropic — extract `request.rs`

**Files:**

- Create: `crates/opex-core/src/agent/providers/anthropic/request.rs`
- Modify: `crates/opex-core/src/agent/providers/anthropic/mod.rs`

**Items to move from `anthropic/mod.rs`:**

Looking at lines 100-376 (`impl AnthropicProvider`), identify the methods that **build the request body** (message conversion, header construction, JSON serialization) as opposed to the methods that **perform the HTTP call** (those stay in `mod.rs`'s `impl LlmProvider for AnthropicProvider`).

Concrete candidates (verify by reading the impl block):

- Any private helper fn that takes `&[Message]` and returns Anthropic-shaped JSON or strings (e.g. `messages_to_anthropic_format` if it exists, or inline body builders).
- The `system` prompt extraction logic.
- The `max_tokens` / `temperature` / `stop_sequences` packaging logic.

- [ ] **Step 1: Read `impl AnthropicProvider` block**

Use `Read` on `anthropic/mod.rs` starting at line 100. Note which fn's are pure-conversion (no HTTP, no `self.client`) vs which call `self.client.post(...).send().await`. The pure-conversion ones move.

- [ ] **Step 2: Create `request.rs`**

```rust
//! Build the JSON body for Anthropic `POST /v1/messages` from a slice of
//! `Message` plus `CallOptions`. Pure functions — no HTTP, no async.

use opex_types::{Message, MessageRole, ToolDefinition};
use serde_json::Value;

use super::thinking::thinking_config;

pub(super) fn build_request_body(
    model: &str,
    messages: &[Message],
    tools: &[ToolDefinition],
    max_tokens: u32,
    temperature: Option<f32>,
    thinking_level: u8,
) -> Value {
    // [paste body — extracted from the existing impl methods]
}

// [add other pure-conversion helpers as identified in Step 1]
```

Function names and signatures should mirror what was inline in `impl AnthropicProvider` — do not rename mid-extract.

- [ ] **Step 3: Replace the inline code in `mod.rs` with calls into `request.rs`**

E.g. if `impl LlmProvider for AnthropicProvider` had 30 lines of body building inline:

```rust
async fn chat(&self, messages: &[Message], opts: &CallOptions) -> Result<LlmResponse> {
    let body = self::request::build_request_body(
        &self.model,
        messages,
        &opts.tools,
        opts.max_tokens.unwrap_or(self.default_max_tokens),
        opts.temperature,
        opts.thinking_level.unwrap_or(0),
    );
    let resp = self.http.post(&self.url).json(&body).send().await?;
    // ... rest unchanged
}
```

The `mod.rs` is now thinner; the same logic lives in `request.rs`.

- [ ] **Step 4: Add `mod request;` declaration**

```rust
mod request;
```

- [ ] **Step 5: Build + test**

```bash
cargo check -p opex-core
cargo clippy -p opex-core --all-targets -- -D warnings
cargo test -p opex-core --bin opex-core agent::providers::anthropic
```

- [ ] **Step 6: Commit**

```bash
git add crates/opex-core/src/agent/providers/anthropic/
git commit -m "refactor(providers/anthropic): extract request body builder to anthropic/request.rs"
```

---

## Task 8: OpenAI — rename to mod.rs

**Files:**

- Rename: `crates/opex-core/src/agent/providers/openai.rs` → `crates/opex-core/src/agent/providers/openai/mod.rs`

- [ ] **Step 1: Create directory**

```bash
mkdir crates/opex-core/src/agent/providers/openai
```

- [ ] **Step 2: git mv**

```bash
git mv crates/opex-core/src/agent/providers/openai.rs \
       crates/opex-core/src/agent/providers/openai/mod.rs
```

- [ ] **Step 3: Verify build**

```bash
cargo check -p opex-core
cargo test -p opex-core --bin opex-core agent::providers::openai
```

- [ ] **Step 4: Commit**

```bash
git commit -m "refactor(providers/openai): rename openai.rs to openai/mod.rs"
```

---

## Task 9: OpenAI — extract `minimax_xml.rs`

**Files:**

- Create: `crates/opex-core/src/agent/providers/openai/minimax_xml.rs`
- Modify: `crates/opex-core/src/agent/providers/openai/mod.rs`

This goes first among OpenAI extracts because it is the most self-contained block.

**Items to move from `openai/mod.rs`:**

- `pub(crate) fn extract_minimax_xml_tool_calls` (line ~996)
- `fn parse_xml_invoke_blocks` (line ~1035)
- `fn parse_xml_parameters` (line ~1068)
- `fn xml_extract_attr` (line ~1101)
- `#[cfg(test)] mod xml_tests` (line ~1109)
- The `minimax_xml_two_invoke_blocks` and `xml_parameters_empty_body` tests from `golden_fixtures` (move into the xml_tests mod or into a new `mod tests` at the bottom of `minimax_xml.rs`)

- [ ] **Step 1: Read current ranges**

```bash
grep -nE "^(pub\(crate\) fn extract_minimax_xml_tool_calls|fn parse_xml_invoke_blocks|fn parse_xml_parameters|fn xml_extract_attr|mod xml_tests)" \
  crates/opex-core/src/agent/providers/openai/mod.rs
```

- [ ] **Step 2: Create `minimax_xml.rs`**

```rust
//! MiniMax-specific XML tool-call extraction. MiniMax variant of the
//! OpenAI-compatible API encodes tool calls inside an XML payload within
//! the `content` field rather than via the `tool_calls` array. This
//! module parses that payload.

pub(crate) fn extract_minimax_xml_tool_calls(
    content: &str,
    out: &mut Vec<opex_types::ToolCall>,
) {
    // [paste body verbatim]
}

fn parse_xml_invoke_blocks(block: &str, out: &mut Vec<opex_types::ToolCall>) {
    // [paste body verbatim]
}

fn parse_xml_parameters(body: &str, out: &mut serde_json::Map<String, serde_json::Value>) {
    // [paste body verbatim]
}

fn xml_extract_attr(s: &str, attr: &str) -> Option<String> {
    // [paste body verbatim]
}

#[cfg(test)]
mod xml_tests {
    // [paste entire mod contents from openai/mod.rs verbatim]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimax_xml_two_invoke_blocks() {
        // [paste body from golden_fixtures]
    }

    #[test]
    fn xml_parameters_empty_body() {
        // [paste body from golden_fixtures]
    }
}
```

Note the visibility: `extract_minimax_xml_tool_calls` keeps `pub(crate)` (some sibling crate code may reference it; check via grep `extract_minimax_xml_tool_calls` across `crates/`). The other three helpers stay private to `minimax_xml.rs`.

- [ ] **Step 3: Delete moved items from `openai/mod.rs`** including the matching `golden_fixtures` entries.

- [ ] **Step 4: Add `mod minimax_xml;` declaration**

```rust
mod minimax_xml;
pub(crate) use minimax_xml::extract_minimax_xml_tool_calls;
```

- [ ] **Step 5: Audit cross-crate uses**

```bash
grep -rn "extract_minimax_xml_tool_calls" crates/
```

Expected: 1 call site in `openai/mod.rs` (now via `minimax_xml::extract_minimax_xml_tool_calls` or via the re-export). If there are call sites outside `agent/providers/openai/`, they keep working through the `pub(crate) use` re-export.

- [ ] **Step 6: Build + test**

```bash
cargo check -p opex-core
cargo clippy -p opex-core --all-targets -- -D warnings
cargo test -p opex-core --bin opex-core agent::providers::openai
```

- [ ] **Step 7: Commit**

```bash
git add crates/opex-core/src/agent/providers/openai/
git commit -m "refactor(providers/openai): extract MiniMax XML tool-call parsing to openai/minimax_xml.rs"
```

---

## Task 10: OpenAI — extract `response.rs`

**Files:**

- Create: `crates/opex-core/src/agent/providers/openai/response.rs`
- Modify: `crates/opex-core/src/agent/providers/openai/mod.rs`

**Items to move from `openai/mod.rs`:**

- `struct ChatCompletionResponse` (line ~889)
- `fn deserialize_null_as_empty_vec` (line ~895)
- `struct ChatChoice` (line ~904)
- `struct ChatMessage` (line ~910)
- `struct ChatToolCall` (line ~916)
- `struct ChatFunction` (line ~922)
- `struct ChatUsage` (line ~928)
- `struct ChatPromptTokensDetails` (line ~938)
- `struct ChatCompletionTokensDetails` (line ~944)
- The `deserialize_null_as_empty_vec_handles_null`, `deserialize_null_as_empty_vec_handles_array`, `streaming_usage_to_token_usage_includes_cache_fields` tests from the new `mod tests` (added in Task 1)

Note: `StreamingUsage` and its `From` impl stay in **stream.rs** (Task 12), even though it shares fields with `ChatUsage`. Per design spec — streaming and non-streaming live in separate modules.

- [ ] **Step 1: Read current ranges**

- [ ] **Step 2: Create `response.rs`**

```rust
//! Non-streaming response types for OpenAI-compatible chat completions.

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub(super) struct ChatCompletionResponse {
    // [paste fields]
}

pub(super) fn deserialize_null_as_empty_vec<'de, D, T>(
    deserializer: D,
) -> Result<Vec<T>, D::Error>
where
    // [paste verbatim]
{
    // [paste verbatim]
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct ChatChoice { /* ... */ }

#[derive(Debug, Clone, Deserialize)]
pub(super) struct ChatMessage { /* ... */ }

#[derive(Debug, Clone, Deserialize)]
pub(super) struct ChatToolCall { /* ... */ }

#[derive(Debug, Clone, Deserialize)]
pub(super) struct ChatFunction { /* ... */ }

#[derive(Debug, Clone, Deserialize)]
pub(super) struct ChatUsage { /* ... */ }

#[derive(Debug, Clone, Deserialize)]
pub(super) struct ChatPromptTokensDetails { /* ... */ }

#[derive(Debug, Clone, Deserialize)]
pub(super) struct ChatCompletionTokensDetails { /* ... */ }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_null_as_empty_vec_handles_null() { /* paste */ }

    #[test]
    fn deserialize_null_as_empty_vec_handles_array() { /* paste */ }

    // streaming_usage_to_token_usage_includes_cache_fields stays in
    // stream.rs since StreamingUsage moves there.
}
```

- [ ] **Step 3: Delete moved items from `openai/mod.rs`**.

- [ ] **Step 4: Add `mod response;`**

```rust
mod response;
use response::{
    ChatChoice, ChatCompletionResponse, ChatCompletionTokensDetails, ChatFunction,
    ChatMessage, ChatPromptTokensDetails, ChatToolCall, ChatUsage,
    deserialize_null_as_empty_vec,
};
```

- [ ] **Step 5: Build + test + commit**

```bash
cargo check -p opex-core
cargo clippy -p opex-core --all-targets -- -D warnings
cargo test -p opex-core --bin opex-core agent::providers::openai
```

```bash
git add crates/opex-core/src/agent/providers/openai/
git commit -m "refactor(providers/openai): extract non-streaming response types to openai/response.rs"
```

---

## Task 11: OpenAI — extract `stream.rs`

**Files:**

- Create: `crates/opex-core/src/agent/providers/openai/stream.rs`
- Modify: `crates/opex-core/src/agent/providers/openai/mod.rs`

**Items to move from `openai/mod.rs`:**

- `struct StreamingUsage` (line ~959)
- `impl From<StreamingUsage> for opex_types::TokenUsage` (line ~967)
- `struct StreamChunk` (line ~1198)
- `struct StreamChoice` (line ~1205)
- `struct StreamDelta` (line ~1211)
- `struct StreamToolCallDelta` (line ~1220)
- `struct StreamFunctionDelta` (line ~1227)
- The streaming-SSE handling fn(s) that consume `StreamChunk` and update `LlmResponse` — locate by grep:

```bash
grep -nE "StreamChunk|StreamDelta|StreamToolCallDelta" \
  crates/opex-core/src/agent/providers/openai/mod.rs
```

- The `streaming_usage_to_token_usage_includes_cache_fields` test from `mod tests` in `response.rs`.

- [ ] **Step 1: Read current ranges + locate SSE handlers**

- [ ] **Step 2: Create `stream.rs`**

```rust
//! Streaming SSE chunk handling for OpenAI-compatible chat completions.
//! Accumulates `delta`-shaped chunks into a final `LlmResponse`.

use serde::Deserialize;

use super::response::{ChatPromptTokensDetails, ChatCompletionTokensDetails};

#[derive(Debug, Clone, Default, Deserialize)]
pub(super) struct StreamingUsage {
    // [paste]
}

impl From<StreamingUsage> for opex_types::TokenUsage {
    fn from(s: StreamingUsage) -> Self {
        // [paste body]
    }
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct StreamChunk { /* ... */ }

#[derive(Debug, Clone, Deserialize)]
pub(super) struct StreamChoice { /* ... */ }

#[derive(Debug, Clone, Deserialize)]
pub(super) struct StreamDelta { /* ... */ }

#[derive(Debug, Clone, Deserialize)]
pub(super) struct StreamToolCallDelta { /* ... */ }

#[derive(Debug, Clone, Deserialize)]
pub(super) struct StreamFunctionDelta { /* ... */ }

// [paste streaming-SSE handler fn(s) here, mark them pub(super)]

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streaming_usage_to_token_usage_includes_cache_fields() {
        // [paste body from response.rs tests]
    }
}
```

- [ ] **Step 3: Delete moved items from `openai/mod.rs` and from `response.rs::tests`**

- [ ] **Step 4: Add `mod stream;`**

```rust
mod stream;
use stream::{
    StreamChoice, StreamChunk, StreamDelta, StreamFunctionDelta, StreamToolCallDelta,
    StreamingUsage,
};
```

- [ ] **Step 5: Build + test + commit**

```bash
cargo check -p opex-core
cargo clippy -p opex-core --all-targets -- -D warnings
cargo test -p opex-core --bin opex-core agent::providers::openai
git add crates/opex-core/src/agent/providers/openai/
git commit -m "refactor(providers/openai): extract SSE streaming to openai/stream.rs"
```

---

## Task 12: OpenAI — extract `tool_calls.rs`

**Files:**

- Create: `crates/opex-core/src/agent/providers/openai/tool_calls.rs`
- Modify: `crates/opex-core/src/agent/providers/openai/mod.rs`
- Modify: `crates/opex-core/src/agent/providers/openai/stream.rs`

Same pattern as Anthropic tool_calls (Task 6): identify whether there are **shared** tool-call helpers between `response.rs` and `stream.rs`. OpenAI's case is simpler because the non-streaming response has tool_calls inline in `ChatMessage`, while streaming accumulates via `StreamToolCallDelta`. Shared logic is likely just the JSON-arguments accumulator.

- [ ] **Step 1: Identify shared helpers**

```bash
grep -nE "tool_calls|ToolCall|merge.*delta" \
  crates/opex-core/src/agent/providers/openai/mod.rs \
  crates/opex-core/src/agent/providers/openai/response.rs \
  crates/opex-core/src/agent/providers/openai/stream.rs
```

If shared helpers exist, follow the extract recipe. If not, file an empty commit as in Task 6 Step 1 (Anthropic).

- [ ] **Step 2-5: As per Task 6 mechanics**

- [ ] **Step 6: Commit**

```bash
git commit -m "refactor(providers/openai): extract tool-call accumulator to openai/tool_calls.rs"
```

(or empty commit with the no-op message if no shared helpers).

---

## Task 13: OpenAI — extract `request.rs`

**Files:**

- Create: `crates/opex-core/src/agent/providers/openai/request.rs`
- Modify: `crates/opex-core/src/agent/providers/openai/mod.rs`

**Items to move:**

- `messages_to_openai_format` (likely in `impl OpenAiCompatibleProvider` body; verify location)
- Request body building helpers (the part of `chat()` that constructs the JSON body, including `o1-*` / `gpt-4o-*` reasoning-effort overrides)

Per `providers/mod.rs:5` the helper `messages_to_openai_format` is listed as a cross-cutting helper — locate its current home:

```bash
grep -n "fn messages_to_openai_format" crates/opex-core/src/agent/providers/openai/mod.rs
grep -n "messages_to_openai_format" crates/opex-core/src/agent/providers/
```

If it lives directly in `providers/mod.rs` (not `openai/mod.rs`), leave it there — it is shared across providers. The OpenAI-specific request shaping (the body-building wrapper around `messages_to_openai_format`) moves to `openai/request.rs`.

- [ ] **Step 1: Identify request-building code**

- [ ] **Step 2: Create `request.rs`**

```rust
//! Build the JSON body for OpenAI-compatible `POST /v1/chat/completions`
//! from messages + CallOptions, including model-specific overrides for
//! reasoning models (o1-*, gpt-4o-*) and tool definitions.

use opex_types::{Message, ToolDefinition};
use serde_json::Value;

pub(super) fn build_request_body(
    model: &str,
    messages: &[Message],
    tools: &[ToolDefinition],
    temperature: Option<f32>,
    max_tokens: Option<u32>,
    reasoning_effort: Option<&str>,
) -> Value {
    // [paste body — adapted from existing impl]
}
```

- [ ] **Step 3: Replace inline code in `mod.rs` with calls into `request::build_request_body`**

- [ ] **Step 4: Add `mod request;`**

- [ ] **Step 5: Build + test + commit**

```bash
cargo check -p opex-core
cargo clippy -p opex-core --all-targets -- -D warnings
cargo test -p opex-core --bin opex-core agent::providers::openai
git add crates/opex-core/src/agent/providers/openai/
git commit -m "refactor(providers/openai): extract request body builder to openai/request.rs"
```

---

## Task 14: Google — rename to mod.rs

**Files:**

- Rename: `crates/opex-core/src/agent/providers/google.rs` → `crates/opex-core/src/agent/providers/google/mod.rs`

- [ ] **Step 1-3: Same recipe as Tasks 2 / 8**

```bash
mkdir crates/opex-core/src/agent/providers/google
git mv crates/opex-core/src/agent/providers/google.rs \
       crates/opex-core/src/agent/providers/google/mod.rs
cargo check -p opex-core
cargo test -p opex-core --bin opex-core agent::providers::google
git commit -m "refactor(providers/google): rename google.rs to google/mod.rs"
```

---

## Task 15: Google — extract `response.rs`

**Files:**

- Create: `crates/opex-core/src/agent/providers/google/response.rs`
- Modify: `crates/opex-core/src/agent/providers/google/mod.rs`

**Items to move from `google/mod.rs`:**

- `struct GeminiResponse` (line ~163)
- `struct GeminiCandidate` (line ~170)
- `struct GeminiContent` (line ~177)
- `struct GeminiPart` (line ~182)
- `struct GeminiFunctionCall` (line ~189)
- `struct GeminiUsage` (line ~195)
- Any helper fn that turns `GeminiResponse` into `LlmResponse`
- The `safety_ratings_block_does_not_crash_parser` test from `golden_fixtures`

- [ ] **Step 1: Read current ranges**

- [ ] **Step 2: Create `response.rs`**

```rust
//! Non-streaming Gemini response types and parser.

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub(super) struct GeminiResponse {
    pub(super) candidates: Vec<GeminiCandidate>,
    pub(super) usage_metadata: Option<GeminiUsage>,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct GeminiCandidate { /* ... */ }

#[derive(Debug, Clone, Deserialize)]
pub(super) struct GeminiContent { /* ... */ }

#[derive(Debug, Clone, Deserialize)]
pub(super) struct GeminiPart { /* ... */ }

#[derive(Debug, Clone, Deserialize)]
pub(super) struct GeminiFunctionCall { /* ... */ }

#[derive(Debug, Clone, Deserialize)]
pub(super) struct GeminiUsage { /* ... */ }

// [paste any response→LlmResponse parser fn here, mark pub(super)]

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safety_ratings_block_does_not_crash_parser() {
        // [paste body from golden_fixtures]
    }
}
```

- [ ] **Step 3: Delete moved items from `google/mod.rs`**

- [ ] **Step 4: Add `mod response;`**

```rust
mod response;
use response::{
    GeminiCandidate, GeminiContent, GeminiFunctionCall, GeminiPart, GeminiResponse,
    GeminiUsage,
};
```

- [ ] **Step 5: Build + test + commit**

```bash
cargo check -p opex-core
cargo clippy -p opex-core --all-targets -- -D warnings
cargo test -p opex-core --bin opex-core agent::providers::google
git add crates/opex-core/src/agent/providers/google/
git commit -m "refactor(providers/google): extract response types to google/response.rs"
```

---

## Task 16: Google — extract `request.rs`

**Files:**

- Create: `crates/opex-core/src/agent/providers/google/request.rs`
- Modify: `crates/opex-core/src/agent/providers/google/mod.rs`

**Items to move from `google/mod.rs`:**

- `pub(super) fn messages_to_gemini_format` (line ~102)
- `fn strip_empty_required` (line ~583)
- Any inline request-body-building code from `impl GoogleProvider`

- [ ] **Step 1: Read current ranges + audit usages**

```bash
grep -n "messages_to_gemini_format\|strip_empty_required" crates/opex-core/
```

`messages_to_gemini_format` is referenced by `providers/mod.rs:39` (`#[cfg(test)] use google::messages_to_gemini_format;`). The re-export at the top of `google/mod.rs` must keep this path resolvable.

- [ ] **Step 2: Create `request.rs`**

```rust
//! Build the JSON body for Gemini API calls from messages + CallOptions.

use opex_types::Message;

pub(super) fn messages_to_gemini_format(
    messages: &[Message],
) -> (Option<String>, Vec<serde_json::Value>) {
    // [paste body verbatim]
}

pub(super) fn strip_empty_required(value: &mut serde_json::Value) {
    // [paste body verbatim]
}
```

- [ ] **Step 3: Delete moved items from `google/mod.rs`**

- [ ] **Step 4: Add `mod request;` with re-export**

```rust
mod request;
pub(super) use request::{messages_to_gemini_format, strip_empty_required};
```

The `pub(super) use messages_to_gemini_format` is what makes `providers/mod.rs:39` (`#[cfg(test)] use google::messages_to_gemini_format`) resolve correctly.

- [ ] **Step 5: Build + test + commit**

```bash
cargo check -p opex-core
cargo clippy -p opex-core --all-targets -- -D warnings
cargo test -p opex-core --bin opex-core agent::providers::google
git add crates/opex-core/src/agent/providers/google/
git commit -m "refactor(providers/google): extract request body builder to google/request.rs"
```

---

## Task 17: Cleanup — tighten visibility

**Files:**

- Modify: every file under `crates/opex-core/src/agent/providers/{anthropic,openai,google}/`

Goal: narrow `pub(super)` items that are *only* used within their own adapter directory to `pub(super)` (no change — already correct) or to nothing (private) where applicable. Drop any temporary `pub use` re-exports that were needed mid-sequence but no longer have an external consumer.

- [ ] **Step 1: Audit each `pub(super)` item per adapter**

For each `pub(super) fn` / `pub(super) struct` in the adapter:

```bash
# example for anthropic
for sym in $(grep -hE "pub\(super\) (fn|struct|enum) [a-zA-Z_]+" \
             crates/opex-core/src/agent/providers/anthropic/*.rs \
             | grep -oE "(fn|struct|enum) [a-zA-Z_]+" \
             | awk '{print $2}'); do
  count=$(grep -rn "\\b$sym\\b" \
          crates/opex-core/src/agent/providers/anthropic/ | wc -l)
  echo "$sym: $count references"
done
```

If a symbol has only **one** reference (its own definition), it can be narrowed to private (remove `pub(super)`). If it has references only inside the same file, narrow to private. If it has references across siblings in the same adapter, keep `pub(super)`. If it has references outside the adapter, **leave it alone** — that visibility is required.

- [ ] **Step 2: Apply narrowing edits**

For each item identified in Step 1 as "narrow to private", use Edit to remove the `pub(super)` keyword.

- [ ] **Step 3: Drop temporary re-exports**

Look in each `adapter/mod.rs` for `pub(super) use sibling::Item` declarations. If the only consumer of `Item` is `mod.rs` itself, the `use sibling::Item;` (non-pub) suffices — drop the `pub(super)`.

- [ ] **Step 4: Build + test + commit**

```bash
cargo check -p opex-core
cargo clippy -p opex-core --all-targets -- -D warnings
cargo test -p opex-core --bin opex-core agent::providers
```

```bash
git add crates/opex-core/src/agent/providers/
git commit -m "$(cat <<'EOF'
chore(providers): tighten visibility after W1 split

Narrow pub(super) -> private on items only used within their defining
file. Drop pub(super) re-exports in adapter mod.rs files where the
only consumer is mod.rs itself.

This commit is purely visibility tightening — no item moves, no logic
changes. Safe to revert if a downstream consumer surfaces an
unexpected import.
EOF
)"
```

---

## Task 18: Acceptance — full test matrix + module-tree summary

**Files:**

- No source changes
- Update only the commit message

- [ ] **Step 1: Run full test matrix**

```bash
DATABASE_URL=postgres://opex_test:opex_test@127.0.0.1:5434/opex_test \
  cargo test --workspace 2>&1 | grep -E "test result|FAILED" | tail -30
```

Expected: same baseline as Discovery commit (1 failure: `db::outbound::tests::test_outbound_queue_lifecycle` — sqlx VersionMismatch on local DB only).

- [ ] **Step 2: rustls invariant**

```bash
cargo tree --workspace -e normal | grep -E "openssl-sys|native-tls" || echo "rustls invariant holds"
```

Expected: prints `rustls invariant holds`.

- [ ] **Step 3: Per-module final LoC measurement**

```bash
wc -l crates/opex-core/src/agent/providers/anthropic/*.rs \
      crates/opex-core/src/agent/providers/openai/*.rs \
      crates/opex-core/src/agent/providers/google/*.rs
```

Capture the output for the commit body.

- [ ] **Step 4: Verify public surface unchanged**

```bash
diff <(git show HEAD~17:crates/opex-core/src/agent/providers/mod.rs | grep "^pub") \
     <(grep "^pub" crates/opex-core/src/agent/providers/mod.rs)
```

Expected: empty diff (re-exports identical to pre-W1 baseline).

- [ ] **Step 5: Decide on golden-fixture cleanup**

For each `mod golden_fixtures` block (across the 3 adapters), check whether its tests are now redundant — they may overlap with the relocated `mod tests` blocks. If overlap exists, delete the redundant golden-fixture test(s); keep at most distinct regression coverage.

- [ ] **Step 6: Final commit**

```bash
git commit --allow-empty -m "$(cat <<'EOF'
chore(providers): W1 refactor acceptance

Wave 1 of the refactoring roadmap is complete. 17 preceding commits
split three large adapter files into focused sibling modules per
concern without changing any wire-level behaviour.

Per-module final LoC:
[paste output of Step 3 here — engineer fills in]

Public surface (providers/mod.rs re-exports): unchanged from pre-W1.
Test baseline failures: 1 (db::outbound, sqlx local-DB only — same
as before W1).
Rustls invariant: holds (no openssl-sys / native-tls in dep tree).

Follow-up observations (deferred from W1, recorded for future waves):
* StreamingAnthropicUsage and StreamingUsage have similar role —
  candidate for a follow-up unification wave if duplication remains
  visually obvious.
* openai/minimax_xml.rs lives under openai/ as a dialect quirk. If
  MiniMax-specific provider routing emerges later, this is the file
  that promotes to a sibling adapter directory.

Next wave: W3a yaml_tools (per roadmap recommended order).
EOF
)"
```

---

## Plan self-review

**Spec coverage check:**

| Spec requirement | Plan task(s) |
| ---- | ---- |
| Discovery commit | Task 1 |
| Anthropic rename + 5 extracts | Tasks 2-7 |
| OpenAI rename + 5 extracts | Tasks 8-13 |
| Google rename + 2 extracts | Tasks 14-16 |
| Cleanup (visibility tightening) | Task 17 |
| Acceptance (full matrix + summary) | Task 18 |
| Test inventory verification (discovery) | Task 1 Step 1 |
| OpenAI `mod tests` gap closure | Task 1 Step 3 |
| Per-adapter `mod golden_fixtures` (×3) | Task 1 Step 4 |
| LCOV → inline summary in commit message | Task 1 Step 2 + Step 7 |
| Anthropic `#[cfg(test)] *_for_test` helpers travel with stream.rs | Task 5 Step 2 visibility note |
| `mod.rs` re-export surface preserved | Tasks 4, 9, 11, 15, 16 all keep `pub(super) use` |
| Per-module LoC trend record | Task 18 Step 3 + commit body |

All 18 tasks have a corresponding spec section.

**Placeholder scan:**

- "[paste body verbatim]" / "[paste fields]" — these are deliberate references to the exact source code that must be moved without modification, not placeholders. The engineer reads the source file and copy-paste-moves. This is correct usage; no rewrite is allowed during extract per spec.
- "engineer fills in" — only in commit-body templates (LoC numbers, coverage %). These are values that *must* come from the engineer's measurement, not pre-filled.
- No "TBD", no "implement later", no "add appropriate error handling".

**Type consistency:**

- `AnthropicProvider`, `OpenAiCompatibleProvider`, `GoogleProvider` — used consistently across all tasks.
- `extract_minimax_xml_tool_calls` — `pub(crate)` consistently (Task 9 Step 4 keeps it).
- `messages_to_gemini_format` — `pub(super)` re-export consistently (Task 16 Step 4).
- `parse_anthropic_response` — `pub(super)` re-export consistently (Task 4 Step 5).
- `build_request_body` — same signature name in both Anthropic Task 7 and OpenAI Task 13 (`pub(super)`).

## Execution

After saving and committing the plan, the next step is to invoke `superpowers:subagent-driven-development` (recommended) or `superpowers:executing-plans` to run the 18 tasks. Each task is independently committable; failure on one task surfaces a clear regression boundary.
