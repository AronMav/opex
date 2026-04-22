# LLM Retry-on-Timeout with Deadline — Design Spec

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Guarantee the model always completes its response. Per-call timeouts (`stream_inactivity_secs`, `stream_max_duration_secs`) trigger automatic retries instead of session failure. Only `run_max_duration_secs = 0` (infinite by default) can stop retrying; if not set, the session lives until it gets a final answer or the user cancels.

**Architecture:** Additive changes only. New `run_max_duration_secs` field in `TimeoutsConfig`. New `PartialState` enum replaces raw `partial_text: String` in `LlmCallError`. Child cancellation tokens in 3 providers. New `chat_stream_with_deadline_retry` in `llm_call.rs` wraps existing transient retry. `execute.rs` calls the new function, no other pipeline changes.

**Tech Stack:** Rust, tokio, `hydeclaw-core` providers, `llm_call.rs`, `execute.rs`

---

## Background

When Ollama or any slow model takes longer than `stream_inactivity_secs` (default 60s) or `stream_max_duration_secs` (default 600s), the stream is cancelled and the session is marked `failed`. The user sees an error instead of an answer.

The root cause has two parts:
1. **Cancel token scope:** providers receive `self.cancel.clone()` — the session-level token. When the inactivity watchdog fires it, the session is dead. Retry is impossible without rebuilding the provider.
2. **No retry loop for timeouts:** `chat_stream_with_transient_retry` only retries `TransientHttp | Overloaded | RateLimit`. `InactivityTimeout` and `MaxDurationExceeded` are classified as `Unknown` and returned directly to the caller.

---

## Design Decisions

| Question | Decision | Reason |
|----------|----------|--------|
| Partial state representation | `PartialState` enum (`Text(String)` / `ToolUse { discarded }` / `Thinking { discarded }`) | Raw buffer contains tool_use JSON deltas that cannot be used as assistant prefill |
| Routing × retry semantics | Discrete: routing for unavailability, retry for slowness on same route | Orthogonal failure modes. Conflating them wastes failover budget on slow providers. |
| `run_max_duration_secs` location | `TimeoutsConfig` in the provider's `ProviderOptions` | Per-provider tuning is already in `TimeoutsConfig`. Zero = infinite. |

---

## Fix Inventory

| ID | File | Description |
|----|------|-------------|
| R1 | `providers/error.rs` | Add `PartialState` enum; replace `partial_text: String` with `partial_state: PartialState` |
| R2 | `providers/timeouts.rs` | Add `run_max_duration_secs: u64` (0 = infinite, default 0) |
| R3 | `providers_openai.rs` | `self.cancel.child_token()` instead of `self.cancel.clone()` |
| R4 | `providers_anthropic.rs` | Same child token fix |
| R5 | `providers_google.rs` | Same child token fix |
| R6 | `error_classify.rs` | Add `LlmErrorClass::CallTimeout`; map `InactivityTimeout` / `MaxDurationExceeded` |
| R6b | `providers.rs` | Add `fn run_max_duration_secs(&self) -> u64 { 0 }` default to `LlmProvider` trait |
| R7 | `pipeline/llm_call.rs` | New `chat_stream_with_deadline_retry`; new `ResumeContext` helper |
| R8 | `pipeline/execute.rs` | Call `chat_stream_with_deadline_retry` instead of `chat_stream_with_transient_retry` |
| R9 | `session_manager.rs` + WAL | Log `llm_retry` WAL event on each retry attempt |
| R10 | `gateway/mod.rs` + SSE types | Add `StreamEvent::Reconnecting { attempt, delay_ms }` |

---

## Detailed Design

### R1 — `PartialState` enum

**File:** `crates/hydeclaw-core/src/agent/providers/error.rs`

Replace `partial_text: String` fields in `InactivityTimeout`, `MaxDurationExceeded`, `UserCancelled`, `ShutdownDrain` with `partial_state: PartialState`.

```rust
/// Structured partial response state after a timeout.
///
/// Only `Text` can be used for API-level stream resume (Anthropic prefill).
/// `ToolUse` and `Thinking` cannot be partially resumed — the caller must
/// discard them and retry with the original messages unmodified.
#[derive(Debug, Clone)]
pub enum PartialState {
    /// Accumulated text deltas — usable for Anthropic assistant prefill.
    Text(String),
    /// Stream was cut during a tool_use block — cannot resume mid-JSON.
    ToolUse,
    /// Stream was cut during a thinking block — cannot resume.
    Thinking,
    /// Nothing accumulated before the timeout.
    Empty,
}

impl PartialState {
    pub fn is_resumable(&self) -> bool {
        matches!(self, Self::Text(s) if !s.is_empty())
    }

    pub fn text(&self) -> Option<&str> {
        if let Self::Text(s) = self { Some(s) } else { None }
    }
}
```

Update `LlmCallError` variants:

```rust
InactivityTimeout {
    provider: String,
    silent_secs: u64,
    partial_state: PartialState,   // was: partial_text: String
},
MaxDurationExceeded {
    provider: String,
    elapsed_secs: u64,
    partial_state: PartialState,
},
UserCancelled { partial_state: PartialState },
ShutdownDrain  { partial_state: PartialState },
```

Update `partial_text()` method to `partial_state()` returning `Option<&PartialState>`.

**Migration note:** `abort_reason` strings are unchanged (pinned by tests). Any call site reading `.partial_text` must be updated to `.partial_state`.

**`is_failover_worthy` change:**

`InactivityTimeout` changes from `true` to `false`. Rationale: with the new retry loop, inactivity means "provider is slow" — retry same provider. Failover is reserved for "provider is unreachable" (`ConnectTimeout`, `Server5xx`).

```rust
pub fn is_failover_worthy(&self) -> bool {
    use LlmCallError::*;
    match self {
        ConnectTimeout { .. }
        | RequestTimeout { .. }
        | Network(_)
        | Server5xx { .. } => true,

        InactivityTimeout { .. }      // changed: no longer failover-worthy
        | MaxDurationExceeded { .. }
        | UserCancelled { .. }
        | ShutdownDrain { .. }
        | AuthError { .. } => false,

        SchemaError { at_bytes, .. } => *at_bytes == 0,
    }
}
```

---

### R2 — `run_max_duration_secs` in `TimeoutsConfig`

**File:** `crates/hydeclaw-core/src/agent/providers/timeouts.rs`

Add one field:

```rust
/// Maximum total wall-clock duration for one pipeline run (all retry
/// attempts combined). Zero means no limit — the run continues until
/// the model responds or the user cancels.
#[serde(default)]   // default = 0 = infinite
pub run_max_duration_secs: u64,
```

Default remains `0`. Validation: no upper bound (0 = infinite; any positive value is valid).

---

### R3–R5 — Child cancellation tokens

**Files:** `providers_openai.rs:522`, `providers_anthropic.rs:470`, `providers_google.rs:456`

Each provider stores `self.cancel: CancellationToken`. Change all three call sites:

```rust
// Before:
stream_with_cancellation(resp.bytes_stream(), self.cancel.clone(), slot.clone(), self.timeouts)

// After:
stream_with_cancellation(resp.bytes_stream(), self.cancel.child_token(), slot.clone(), self.timeouts)
```

**Why this works:** `child_token()` creates a token that:
- Cancels when inactivity/max_duration watchdog fires (allowing retry on parent)
- Also cancels automatically when the parent token is cancelled (user Stop → child already cancelled → `UserCancelled` branch fires → retry loop sees parent cancelled → stops)

No provider struct changes needed — `self.cancel` stays as `CancellationToken`.

---

### R6b — `LlmProvider` trait: `run_max_duration_secs`

**File:** `crates/hydeclaw-core/src/agent/providers.rs`

Add two default methods to `LlmProvider` trait:

```rust
/// Maximum wall-clock duration for all retry attempts combined. 0 = infinite.
/// Overridden by each concrete provider to return `self.timeouts.run_max_duration_secs`.
fn run_max_duration_secs(&self) -> u64 { 0 }

/// True when the provider supports Anthropic-style assistant prefill (appending
/// a partial assistant message so the model continues from it).
/// Only `AnthropicProvider` overrides this to `true`.
fn supports_prefill(&self) -> bool { false }
```

Implement in `OpenAiProvider`, `GoogleProvider`, `OllamaProvider`:

```rust
fn run_max_duration_secs(&self) -> u64 { self.timeouts.run_max_duration_secs }
// supports_prefill() uses default false
```

Implement in `AnthropicProvider`:

```rust
fn run_max_duration_secs(&self) -> u64 { self.timeouts.run_max_duration_secs }
fn supports_prefill(&self) -> bool { true }
```

`RoutingProvider` returns `0` for both methods — the inner provider manages its own stream timeouts; applying a deadline at routing level would double-count elapsed time across routes.

---

### R6 — `error_classify.rs`

**File:** `crates/hydeclaw-core/src/agent/error_classify.rs`

Add new error class:

```rust
#[derive(Debug, Clone, PartialEq)]
pub enum LlmErrorClass {
    TransientHttp,
    RateLimit,
    Overloaded,
    CallTimeout,  // NEW: inactivity or max_duration
    Unknown,
}
```

Update `classify`:

```rust
if matches!(&e, LlmCallError::InactivityTimeout { .. } | LlmCallError::MaxDurationExceeded { .. }) {
    return LlmErrorClass::CallTimeout;
}
```

`is_retryable` does NOT include `CallTimeout` — that class is handled by the outer deadline loop, not the inner transient loop.

---

### R7 — `chat_stream_with_deadline_retry`

**File:** `crates/hydeclaw-core/src/agent/pipeline/llm_call.rs`

New function signature:

```rust
pub async fn chat_stream_with_deadline_retry(
    provider: &dyn LlmProvider,
    messages: &mut Vec<Message>,
    tools: &[ToolDefinition],
    chunk_tx: mpsc::UnboundedSender<String>,
    compact: &impl Compactor,
    session_cancel: &CancellationToken,     // parent token (session-level)
    run_max_duration_secs: u64,             // 0 = infinite
    session_id: Uuid,
    sm: &SessionManager,
) -> Result<hydeclaw_types::LlmResponse, LlmCallError>
```

**Return contract:** On timeout retry, the function loops internally and returns only when the model succeeds, `run_max_duration_secs` is exceeded, or `session_cancel` fires. It does NOT emit `StreamEvent::Reconnecting` — that is the caller's responsibility (see R8).

**Algorithm:**

```
run_started_at = Instant::now()
attempt = 0
resume_messages = messages.clone()  // saved original for discard-path

loop:
    if run_max_duration_secs > 0 and elapsed > run_max_duration_secs:
        return Err(MaxDurationExceeded { ... })

    if session_cancel.is_cancelled():
        return Err(UserCancelled { partial_state: Empty })

    result = chat_stream_with_transient_retry(provider, messages, tools, chunk_tx.clone(), compact).await

    match result:
        Ok(r) => return Ok(r)

        Err(LlmCallError::UserCancelled | ShutdownDrain) => return Err(e)  // propagate

        Err(e @ InactivityTimeout { partial_state, .. } | e @ MaxDurationExceeded { partial_state, .. }):
            attempt += 1
            log WAL llm_retry event (attempt, reason, partial_resumable)

            delay = min(2^attempt * 2s, 30s)  // 2s, 4s, 8s, 16s, 30s cap

            // Backoff with cancel check
            select! {
                _ = session_cancel.cancelled() => return Err(UserCancelled { partial_state: Empty }),
                _ = sleep(delay) => ()
            }

            // Build resume context
            if partial_state.is_resumable() and provider.supports_prefill():
                // Anthropic: append assistant prefill message
                messages = resume_messages.clone()
                messages.push(Message { role: Assistant, content: partial_state.text() })
                // Model continues from here
            else:
                // Discard partial, retry with original messages
                messages = resume_messages.clone()

        Err(other) => return Err(other)  // ConnectTimeout, AuthError, etc. — propagate to routing
```

**Backoff:** exponential 2s base, 30s cap. `select!` with `session_cancel.cancelled()` so user Stop interrupts backoff immediately.

---

### R8 — `execute.rs` call site

**File:** `crates/hydeclaw-core/src/agent/pipeline/execute.rs`

Replace the call at line ~181:

```rust
// Before:
let llm_fut = crate::agent::pipeline::llm_call::chat_stream_with_transient_retry(
    provider,
    &mut messages,
    &tools,
    chunk_tx,
    engine,
);

// After:
// Note: `sm` is already instantiated earlier in execute() — reuse it.
let run_max = provider.run_max_duration_secs();  // via LlmProvider trait (R6b)
let llm_fut = crate::agent::pipeline::llm_call::chat_stream_with_deadline_retry(
    provider,
    &mut messages,
    &tools,
    chunk_tx,
    engine,
    &cancel,           // already in execute() scope (passed as param)
    run_max,
    session_id,        // already destructured from bootstrap_outcome
    &sm,
);
```

`session_id`, `sm`, `provider`, and `cancel` are all already in scope in `execute()`.

**SSE `Reconnecting` emission** belongs in `execute.rs`, not in `llm_call.rs` (which has no sink access). `chat_stream_with_deadline_retry` is called inside `forward_chunks_into_sink`. Since the deadline retry loops internally and returns only on final success/failure, the sink cannot observe intermediate retry attempts from the outside.

Solution: wrap `forward_chunks_into_sink` in a thin outer loop in `execute.rs` that drives one `chat_stream_with_deadline_retry` call — but since the retry is now internal to `chat_stream_with_deadline_retry`, emit the event via a callback. Pass an optional `on_retry` closure:

```rust
let on_retry = |attempt: u32, delay_ms: u64| {
    let _ = sink.emit(PipelineEvent::Stream(StreamEvent::Reconnecting { attempt, delay_ms }));
};
```

Add `on_retry: Option<&dyn Fn(u32, u64)>` as the last parameter of `chat_stream_with_deadline_retry`. Called after delay is computed, before `sleep`. `execute.rs` passes `Some(&on_retry)`; tests pass `None`.

---

### R9 — WAL `llm_retry` event

**File:** `crates/hydeclaw-core/src/agent/session_manager.rs` (uses existing `log_wal_event`)

In `chat_stream_with_deadline_retry`, log a WAL event on each retry:

```rust
let details = serde_json::json!({
    "attempt": attempt,
    "reason": e.abort_reason().unwrap_or("unknown"),
    "partial_resumable": partial_state.is_resumable(),
    "delay_ms": delay.as_millis(),
});
sm.log_wal_event(session_id, "llm_retry", Some(details)).await.ok();
```

Non-fatal: `.ok()` — retry continues even if WAL write fails.

---

### R10 — SSE `reconnecting` event

**Files:** `crates/hydeclaw-core/src/gateway/mod.rs` (sse_types), `ui/src/stores/sse-events.ts`, `ui/src/stores/chat-store.ts`

Add new `StreamEvent` variant:

```rust
// In sse_types (gateway/mod.rs):
Reconnecting { attempt: u32, delay_ms: u64 },
```

Serialized as `{"type": "reconnecting", "attempt": 1, "delay_ms": 2000}`.

**Frontend (`sse-events.ts`):** add `"reconnecting"` to the union type of known event types.

**Frontend (`chat-store.ts`):** add `isReconnecting: boolean` flag to `AgentState`. Set to `true` on `reconnecting` event, reset to `false` on `text-start`, `finish`, or `error`. The chat UI shows a "Model is retrying..." banner while `isReconnecting` is true — dismiss is automatic on next token arrival.

**`chat_stream_with_deadline_retry` final signature** (incorporating `on_retry` from R8):

```rust
pub async fn chat_stream_with_deadline_retry(
    provider: &dyn LlmProvider,
    messages: &mut Vec<Message>,
    tools: &[ToolDefinition],
    chunk_tx: mpsc::UnboundedSender<String>,
    compact: &impl Compactor,
    session_cancel: &CancellationToken,
    run_max_duration_secs: u64,
    session_id: Uuid,
    sm: &SessionManager,
    on_retry: Option<&dyn Fn(u32, u64)>,  // (attempt, delay_ms) callback
) -> Result<hydeclaw_types::LlmResponse, LlmCallError>
```

---

## Testing

### Unit tests (in `llm_call.rs`)

- `deadline_retry_succeeds_on_second_attempt`: mock provider returns `InactivityTimeout` first, then `Ok` — verify function returns `Ok`
- `deadline_retry_stops_on_user_cancel`: cancel token fired during backoff — verify returns `UserCancelled`
- `deadline_retry_stops_when_run_max_exceeded`: `run_max_duration_secs = 1`, sleep mock — verify returns `MaxDurationExceeded`
- `deadline_retry_propagates_connect_timeout`: non-retryable error — verify returned immediately without retry

### Unit tests (in `error.rs`)

- `partial_state_text_is_resumable`: `PartialState::Text("hello")` → `is_resumable() == true`
- `partial_state_empty_is_not_resumable`: `PartialState::Empty` → `is_resumable() == false`
- `partial_state_tool_use_is_not_resumable`: `PartialState::ToolUse` → `is_resumable() == false`
- `inactivity_is_not_failover_worthy_after_change`: verify `is_failover_worthy() == false`

### Unit tests (in `error_classify.rs`)

- `classify_inactivity_timeout_is_call_timeout`: verify new class mapping

---

## File Summary

| File | Action | Change |
|------|--------|--------|
| `providers/error.rs` | Modify | `PartialState` enum; update variants; `is_failover_worthy` for `InactivityTimeout` → false |
| `providers/timeouts.rs` | Modify | Add `run_max_duration_secs: u64` (default 0) |
| `providers_openai.rs` | Modify | `child_token()` instead of `clone()` |
| `providers_anthropic.rs` | Modify | Same |
| `providers_google.rs` | Modify | Same |
| `error_classify.rs` | Modify | Add `CallTimeout` class; map inactivity/max_duration |
| `pipeline/llm_call.rs` | Modify | Add `chat_stream_with_deadline_retry` |
| `pipeline/execute.rs` | Modify | Call `chat_stream_with_deadline_retry` |
| `session_manager.rs` | Modify | WAL `llm_retry` event (already has `log_wal_event`) |
| `gateway/mod.rs` | Modify | `StreamEvent::Reconnecting` variant |
| `ui/src/stores/sse-events.ts` | Modify | Handle `reconnecting` event |
