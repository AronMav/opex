# Extended Thinking + Base Agent Scaffold Refactor

**Date:** 2026-04-30
**Status:** Approved

## Overview

Three independent work items:

1. **Scaffold refactor** — Slim `scaffold/base/SOUL.md` from ~6 KB to ~2.5 KB by moving technical reference content into a new `scaffold/base/MEMORY.md`. Reduces system prompt token cost on every request.
2. **`CallOptions` in `LlmProvider` trait** — Thread `thinking_level` from `AgentState` down to the provider layer via an explicit `CallOptions` struct.
3. **Anthropic extended thinking** — Implement extended thinking support: model-aware thinking config generation, temperature enforcement, streaming `thinking_delta` handling, signature accumulation for multi-turn.

---

## Block A — Scaffold Refactor

### Problem

`scaffold/base/SOUL.md` is ~6 KB. It contains the System Architecture diagram, Core API Reference table, a full tool inventory (available + denied), and a detailed Methodology section. This content is consumed as system prompt tokens on every LLM call for base-agent sessions, adding ~875–1 000 tokens of static reference overhead.

### Solution

Split into two files:

**`SOUL.md` keeps (~2.5 KB):**
- Identity section (who this agent is, what it does)
- Inter-agent security rules (`HARD RULE` block — behavioral, must be inline)
- Maintenance / heartbeat summary line
- Skills list (names only)
- Core operational principles (brief bullets)
- Forbidden list

**`MEMORY.md` receives (~3.5 KB):**
- System Architecture diagram (`Core → channels → toolgate → ...` with paths)
- Core API Reference table (`/api/providers`, `/api/agents`, etc.)
- Available tools reference (with `action` parameter enumerations)
- Denied tools list
- Methodology details (Goal-Backward, Discovery Classification, Verification Mindset, Error Recovery)

### Why MEMORY.md works here

`MEMORY.md` is a workspace file read on session entry, not injected into every LLM call as a system prompt. The base agent accesses it via `workspace_read` when it needs architectural reference. Behavioral rules (security, identity) stay inline in SOUL.md because they must be active every call without a read step.

---

## Block B — `CallOptions` in `LlmProvider` Trait

### Current state

`LlmProvider::chat` and `LlmProvider::chat_stream` signatures carry only `messages` and `tools`. There is no mechanism to pass per-call LLM parameters (thinking level, seed, etc.) without modifying the provider struct itself.

`thinking_level` lives in `AgentState` as `AtomicU8`, changed at runtime via `/think N`. It currently only controls response-side stripping of `<think>` blocks; it has no effect on outgoing API requests.

### Design

```rust
// crates/hydeclaw-core/src/agent/providers.rs

#[derive(Default, Clone, Copy)]
pub struct CallOptions {
    pub thinking_level: u8,
}
```

Trait signatures change:

```rust
pub trait LlmProvider: Send + Sync {
    async fn chat(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        opts: CallOptions,
    ) -> Result<LlmResponse>;

    async fn chat_stream(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        chunk_tx: mpsc::UnboundedSender<String>,
        opts: CallOptions,
    ) -> Result<LlmResponse>;
}
```

### Call chain

```
AgentState.thinking_level.load(Ordering::Relaxed)
    ↓  execute.rs — creates CallOptions before each LLM call
CallOptions { thinking_level }
    ↓  chat_stream_with_deadline_retry(provider, messages, tools, chunk_tx, opts, ...)
    ↓  deadline_retry_inner(provider, messages, tools, chunk_tx, opts, ...)
provider.chat_stream(messages, tools, chunk_tx, opts)
    ↓  AnthropicProvider uses opts.thinking_level
    ↓  All other providers: _opts (ignored)
```

### Affected files

| File | Change |
|------|--------|
| `src/agent/providers.rs` | Add `CallOptions`, update trait |
| `src/agent/providers_anthropic.rs` | Use `opts.thinking_level` |
| `src/agent/providers_openai.rs` | Accept `_opts`, ignore |
| `src/agent/providers_google.rs` | Accept `_opts`, ignore |
| `src/agent/providers_http.rs` | Accept `_opts`, ignore |
| `src/agent/providers_claude_cli.rs` | Accept `_opts`, ignore |
| `src/agent/providers.rs` (`RoutingProvider`) | Thread `opts` through to inner provider |
| `src/agent/pipeline/llm_call.rs` | Thread `opts` through `chat_stream_with_deadline_retry` |
| `src/agent/pipeline/execute.rs` | Read `engine.state().thinking_level`, construct `CallOptions` |
| `src/agent/pipeline/subagent_runner.rs` | Pass `CallOptions::default()` |
| `src/agent/pipeline/openai_compat.rs` | Pass `CallOptions::default()` |
| Test mock impls (~5 files) | Add `_opts: CallOptions` to mock trait impls |

---

## Block C — Anthropic Extended Thinking

### Model detection

Anthropic has two thinking modes; model name determines which applies:

```rust
fn thinking_mode(model: &str) -> ThinkingMode {
    // Opus 4.7+ and Mythos: adaptive only (manual → 400 error)
    if model.contains("claude-opus-4-7") || model.contains("claude-mythos") {
        return ThinkingMode::AdaptiveOnly;
    }
    // Opus 4.6, Sonnet 4.6: adaptive recommended, manual deprecated but functional
    if model.contains("claude-opus-4-6") || model.contains("claude-sonnet-4-6") {
        return ThinkingMode::Adaptive;
    }
    // All others: manual budget_tokens
    ThinkingMode::Manual
}

enum ThinkingMode { AdaptiveOnly, Adaptive, Manual }
```

### Level → thinking config mapping

**Adaptive models** (Opus 4.7, Mythos, Opus 4.6, Sonnet 4.6):

`display: "summarized"` is included explicitly on all levels > 0. Opus 4.7 defaults to `"omitted"`, which suppresses `thinking_delta` SSE events — without this field the streaming state machine receives no thinking content.

| Level | Config |
|-------|--------|
| 0 | no `thinking` field |
| 1–2 | `{"type":"adaptive","effort":"low","display":"summarized"}` |
| 3 | `{"type":"adaptive","effort":"medium","display":"summarized"}` |
| 4–5 | `{"type":"adaptive","effort":"high","display":"summarized"}` |

**Manual models** (Sonnet 3.7, Haiku 4.5, Opus 4.5, etc.):

`display: "summarized"` is included on all levels > 0 for consistency and future-safety.

| Level | Config |
|-------|--------|
| 0 | none |
| 1 | `{"type":"enabled","budget_tokens":1024,"display":"summarized"}` |
| 2 | `{"type":"enabled","budget_tokens":4096,"display":"summarized"}` |
| 3 | `{"type":"enabled","budget_tokens":10000,"display":"summarized"}` |
| 4 | `{"type":"enabled","budget_tokens":20000,"display":"summarized"}` |
| 5 | `{"type":"enabled","budget_tokens":32000,"display":"summarized"}` |

Constraint enforcement: `budget_tokens = budget.min(effective_max_tokens.saturating_sub(1_000))`, where `effective_max_tokens = self.max_tokens.unwrap_or(8_192)`. The subtraction of 1 000 ensures `budget_tokens < max_tokens` per Anthropic's strict requirement. If the clamped result < 1 024, thinking is silently skipped (budget too tight for the configured `max_tokens`). The clamping formula does not apply to adaptive models since they do not use `budget_tokens`.

### Temperature enforcement

Anthropic rejects `temperature < 1.0` when thinking is enabled:

```rust
let temperature = if opts.thinking_level > 0 {
    self.temperature.max(1.0)
} else {
    self.temperature
};
```

### Request body construction

`build_request_body` gains a third parameter: `fn build_request_body(&self, messages, tools, opts: CallOptions)`.

`thinking_config` is a private free function in `providers_anthropic.rs` with signature:
```rust
fn thinking_config(level: u8, model: &str, effective_max_tokens: u32) -> Option<serde_json::Value>
```
It returns `None` for level 0 or when the clamped budget falls below 1 024. The returned `Value` already includes `"display": "summarized"`.

In `build_request_body`, when `thinking_level > 0`:

```rust
if let Some(thinking_json) = thinking_config(opts.thinking_level, model, effective_max_tokens) {
    body["thinking"] = thinking_json;
    body["temperature"] = serde_json::json!(temperature); // forced ≥ 1.0
}
```

### Streaming: parsing new delta types

Current code at `providers_anthropic.rs:494` only reads `delta.text`. With extended thinking, Anthropic sends three delta types per turn:

| SSE delta type | Field | Current handling | New handling |
|---------------|-------|-----------------|--------------|
| `text_delta` | `text` | ✅ → ThinkingFilter → chunk_tx | unchanged |
| `thinking_delta` | `thinking` | ignored | wrap in `<thinking>…</thinking>` → chunk_tx (bypass ThinkingFilter) |
| `signature_delta` | `signature` | ignored | accumulate into local `current_signature: String` |

The current streaming loop only matches `content_block_delta` events. Three additional event types need to be parsed:

| SSE event type | When | Action |
|---------------|------|--------|
| `content_block_start` | start of a block | if `content_block.type == "thinking"`: set `in_thinking_block = true`, emit `"<thinking>"` to `chunk_tx` |
| `content_block_stop` | end of a block | if `in_thinking_block`: emit `"</thinking>"`, push to `thinking_blocks`, reset state |
| `content_block_delta` | existing | add handling for `thinking_delta` and `signature_delta` alongside existing `text_delta` |

State accumulated across one thinking block:

```rust
let mut thinking_content = String::new();
let mut current_signature = String::new();
let mut in_thinking_block = false;
let mut thinking_blocks: Vec<ThinkingBlock> = vec![];

// content_block_delta, delta.type == "thinking_delta":
//   thinking_content.push_str(delta.thinking);
//   chunk_tx.send(delta.thinking)  // raw chunk, bypasses ThinkingFilter

// content_block_delta, delta.type == "signature_delta":
//   current_signature.push_str(delta.signature);

// content_block_stop when in_thinking_block:
//   chunk_tx.send("</thinking>")
//   thinking_blocks.push(ThinkingBlock { thinking: mem::take(&mut thinking_content),
//                                        signature: mem::take(&mut current_signature) });
//   in_thinking_block = false
```

Thinking chunks bypass `ThinkingFilter` because they are emitted directly, not via the filter. `ThinkingFilter` continues to process `text_delta` chunks (handles inline `<think>` tags from models like Qwen that embed them in text).

### UI: no changes required

The frontend already has the full rendering pipeline for thinking content:

- `IncrementalParser` (`message-parser.ts`) — parses `<thinking>…</thinking>` tags in streaming chunks → `ReasoningPart` objects
- `ReasoningPart.tsx` — renders reasoning with styled box
- `MessageItem.tsx` — dispatches `case "reasoning"` to `ReasoningPart`
- `parseContentParts` (`message-parser.ts`) — handles `<thinking>` tags in loaded history

Thinking content wrapped in `<thinking>` tags flows through `chunk_tx` → SSE `text-delta` events → `IncrementalParser` → renders live. It also gets included in `partial` → stored in `message.content` → parsed on history load.

### Multi-turn correctness

`LlmResponse.thinking_blocks` is already used when building subsequent messages in `build_request_body`:

```rust
// Already implemented (providers_anthropic.rs:156-161)
for tb in &msg.thinking_blocks {
    content.push(json!({
        "type": "thinking",
        "thinking": tb.thinking,
        "signature": tb.signature,
    }));
}
```

After this change, the streaming path will populate `LlmResponse.thinking_blocks` (currently returns empty `vec![]`), ensuring the signature is carried forward in tool-use loops.

---

## Testing

### Block A
- Visual diff of SOUL.md before/after (char count, key sections present)
- MEMORY.md exists and contains Architecture + API Reference sections

### Block B
- `make check` — trait signature consistency across all impls
- `cargo test` — existing provider tests pass with added `opts` parameter

### Block C
- Unit test: `thinking_config(0, "claude-opus-4-7", 8192)` → `None`
- Unit test: `thinking_config(3, "claude-opus-4-7", 8192)` → `{"type":"adaptive","effort":"medium"}`
- Unit test: `thinking_config(3, "claude-sonnet-3-7", 8192)` → `{"type":"enabled","budget_tokens":7192}` (10 000 clamped to 8192 − 1000)
- Unit test: `thinking_config(5, "claude-haiku-4-5", 2000)` → `None` (budget 32k clamped to 1000 < 1024)
- Unit test: temperature enforcement — `level > 0, temperature = 0.7` → body `temperature = 1.0`
- Integration: streaming loop correctly emits `<thinking>…</thinking>` chunks and populates `thinking_blocks`

---

## Implementation Order

1. **Block A** — scaffold files (no Rust changes, independent)
2. **Block B** — `CallOptions` trait change (mechanical, no logic)
3. **Block C** — Anthropic logic on top of Block B

Blocks B and C are sequential (C depends on B). Block A is independent and can be done in parallel or first.
