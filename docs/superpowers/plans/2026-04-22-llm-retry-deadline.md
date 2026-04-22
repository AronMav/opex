# LLM Retry-on-Timeout with Deadline — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Guarantee the model always completes its response — per-call timeouts trigger automatic retries instead of session failure.

**Architecture:** `PartialState` enum replaces raw `partial_text: String` in `LlmCallError`. Child cancellation tokens in 3 providers isolate per-call timeouts from the session token. New `chat_stream_with_deadline_retry` in `llm_call.rs` wraps existing transient retry with an outer timeout-retry loop. `execute.rs` calls the new function; `StreamEvent::Reconnecting` signals the UI.

**Tech Stack:** Rust, tokio, `hydeclaw-core` providers, `pipeline/llm_call.rs`, `pipeline/execute.rs`, Next.js frontend

---

## File Map

| File | Action | Responsibility |
|------|--------|----------------|
| `crates/hydeclaw-core/src/agent/providers/error.rs` | Modify | `PartialState` enum; replace `partial_text` in 4 variants; update tests |
| `crates/hydeclaw-core/src/agent/providers_openai.rs` | Modify | `child_token()` + `PartialState` construction at cancel exit |
| `crates/hydeclaw-core/src/agent/providers_anthropic.rs` | Modify | Same |
| `crates/hydeclaw-core/src/agent/providers_google.rs` | Modify | Same |
| `crates/hydeclaw-core/src/agent/providers_http.rs` | Modify | `PartialState` construction in `parse_sse_stream` (compile fix) |
| `crates/hydeclaw-core/src/agent/providers/timeouts.rs` | Modify | Add `run_max_duration_secs: u64` (0 = infinite) |
| `crates/hydeclaw-core/src/agent/providers.rs` | Modify | `LlmProvider` trait: `run_max_duration_secs()` + `supports_prefill()` |
| `crates/hydeclaw-core/src/agent/error_classify.rs` | Modify | `LlmErrorClass::CallTimeout`; downcast check in `classify` |
| `crates/hydeclaw-core/src/agent/pipeline/llm_call.rs` | Modify | New `chat_stream_with_deadline_retry` + WAL event logging |
| `crates/hydeclaw-core/src/agent/pipeline/execute.rs` | Modify | Call new function; `emit_chunk` handles `__reconnecting__:` prefix |
| `crates/hydeclaw-core/src/agent/stream_event.rs` | Modify | `StreamEvent::Reconnecting { attempt, delay_ms }` |
| `crates/hydeclaw-core/src/gateway/mod.rs` | Modify | `sse_types::RECONNECTING` constant |
| `crates/hydeclaw-core/src/gateway/handlers/chat.rs` | Modify | Handle `StreamEvent::Reconnecting` in SSE converter |
| `ui/src/stores/sse-events.ts` | Modify | `"reconnecting"` variant + parse case |
| `ui/src/stores/chat-types.ts` | Modify | `isLlmReconnecting: boolean` in `AgentState` |

---

## Task 1: `PartialState` enum (R1)

**Files:**
- Modify: `crates/hydeclaw-core/src/agent/providers/error.rs`

- [ ] **Step 1.1: Write the failing tests**

Add after the existing `abort_reason_strings_are_stable` test:

```rust
#[test]
fn partial_state_text_is_resumable() {
    use super::PartialState;
    assert!(PartialState::Text("hello".into()).is_resumable());
    assert!(!PartialState::Text(String::new()).is_resumable());
}

#[test]
fn partial_state_non_text_is_not_resumable() {
    use super::PartialState;
    assert!(!PartialState::ToolUse.is_resumable());
    assert!(!PartialState::Thinking.is_resumable());
    assert!(!PartialState::Empty.is_resumable());
}

#[test]
fn inactivity_is_not_failover_worthy_after_r1() {
    let e = LlmCallError::InactivityTimeout {
        provider: "p".into(),
        silent_secs: 60,
        partial_state: PartialState::Empty,
    };
    assert!(!e.is_failover_worthy(), "InactivityTimeout must NOT be failover-worthy after R1");
}
```

- [ ] **Step 1.2: Run failing tests**

```
cargo test -p hydeclaw-core providers::error -- --nocapture 2>&1 | tail -20
```
Expected: compile error `PartialState` not found.

- [ ] **Step 1.3: Add `PartialState` enum before `LlmCallError`**

Insert after `pub enum CancelReason { ... }` block (before line 36):

```rust
/// Structured partial response state captured on stream timeout.
///
/// Only `Text` can be used for Anthropic-style assistant prefill.
/// `ToolUse` and `Thinking` cannot be partially resumed.
#[derive(Debug, Clone)]
pub enum PartialState {
    /// Accumulated text deltas — usable for Anthropic assistant prefill.
    Text(String),
    /// Stream cut during a tool_use block — cannot resume mid-JSON.
    ToolUse,
    /// Stream cut during a thinking block — cannot resume.
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

- [ ] **Step 1.4: Replace `partial_text: String` with `partial_state: PartialState` in `LlmCallError` variants**

Change `InactivityTimeout`, `MaxDurationExceeded`, `UserCancelled`, `ShutdownDrain`:

```rust
#[error("{provider}: provider stopped sending data for {silent_secs}s")]
InactivityTimeout {
    provider: String,
    silent_secs: u64,
    partial_state: PartialState,
},

#[error("{provider}: stream exceeded max duration {elapsed_secs}s")]
MaxDurationExceeded {
    provider: String,
    elapsed_secs: u64,
    partial_state: PartialState,
},

#[error("stopped by user")]
UserCancelled { partial_state: PartialState },

#[error("interrupted by shutdown drain")]
ShutdownDrain { partial_state: PartialState },
```

- [ ] **Step 1.5: Update `is_failover_worthy` — `InactivityTimeout` → false**

```rust
pub fn is_failover_worthy(&self) -> bool {
    use LlmCallError::*;
    match self {
        ConnectTimeout { .. }
        | RequestTimeout { .. }
        | Network(_)
        | Server5xx { .. } => true,

        InactivityTimeout { .. }      // changed: no longer failover-worthy (retry same provider)
        | MaxDurationExceeded { .. }
        | UserCancelled { .. }
        | ShutdownDrain { .. }
        | AuthError { .. } => false,

        SchemaError { at_bytes, .. } => *at_bytes == 0,
    }
}
```

- [ ] **Step 1.6: Replace `partial_text()` method with `partial_state()`**

```rust
/// Returns the partial state if this variant carries one.
pub fn partial_state(&self) -> Option<&PartialState> {
    use LlmCallError::*;
    match self {
        InactivityTimeout { partial_state, .. }
        | MaxDurationExceeded { partial_state, .. }
        | UserCancelled { partial_state }
        | ShutdownDrain { partial_state } => Some(partial_state),
        _ => None,
    }
}
```

- [ ] **Step 1.7: Update existing tests to use `partial_state: PartialState::Empty`**

Fix these tests:
- `is_failover_worthy_inactivity_timeout`: change assertion from `assert!` to `assert!(!...)` and rename to `inactivity_not_failover_worthy_after_r1` (or just update in-place, but the new test in step 1.1 already covers it — delete the old one that asserted `true`)
- `not_failover_worthy_max_duration`: `partial_text: String::new()` → `partial_state: PartialState::Empty`
- `not_failover_worthy_user_cancelled`: `partial_text: String::new()` → `partial_state: PartialState::Empty`
- `not_failover_worthy_shutdown_drain`: same
- `variants_carrying_partial_text_can_return_it`: rename to `variants_carrying_partial_state_can_return_it`, update to use `partial_state: PartialState::Text("hello".into())` and check via `.partial_state()` returning `Some(PartialState::Text(s))` where `s == "hello"`
- `abort_reason_strings_are_stable`: replace `partial_text: "".into()` with `partial_state: PartialState::Empty` in all 4 variants

Full updated `variants_carrying_partial_state_can_return_it`:
```rust
#[test]
fn variants_carrying_partial_state_can_return_it() {
    let e = LlmCallError::UserCancelled { partial_state: PartialState::Text("hello".into()) };
    match e.partial_state() {
        Some(PartialState::Text(s)) => assert_eq!(s, "hello"),
        other => panic!("expected Some(Text), got {other:?}"),
    }

    let e2 = LlmCallError::ConnectTimeout { provider: "p".into(), elapsed_secs: 5 };
    assert!(e2.partial_state().is_none());
}
```

Updated `abort_reason_strings_are_stable`:
```rust
#[test]
fn abort_reason_strings_are_stable() {
    use LlmCallError::*;
    assert_eq!(ConnectTimeout { provider: "p".into(), elapsed_secs: 1 }.abort_reason(), Some("connect_timeout"));
    assert_eq!(InactivityTimeout { provider: "p".into(), silent_secs: 1, partial_state: PartialState::Empty }.abort_reason(), Some("inactivity"));
    assert_eq!(RequestTimeout { provider: "p".into(), elapsed_secs: 1 }.abort_reason(), Some("request_timeout"));
    assert_eq!(MaxDurationExceeded { provider: "p".into(), elapsed_secs: 1, partial_state: PartialState::Empty }.abort_reason(), Some("max_duration"));
    assert_eq!(UserCancelled { partial_state: PartialState::Empty }.abort_reason(), Some("user_cancelled"));
    assert_eq!(ShutdownDrain { partial_state: PartialState::Empty }.abort_reason(), Some("shutdown_drain"));
}
```

- [ ] **Step 1.8: Run tests (will fail until providers are updated in Task 2)**

```
cargo check -p hydeclaw-core 2>&1 | head -40
```
Expected: compile errors in providers_openai.rs, providers_anthropic.rs, providers_google.rs, providers_http.rs — they still use `partial_text`. These will be fixed in Task 2.

- [ ] **Step 1.9: Commit when Task 2 also compiles (defer commit to end of Task 2)**

---

## Task 2: Provider migrations — child token + PartialState (R1b, R3–R5)

**Files:**
- Modify: `crates/hydeclaw-core/src/agent/providers_openai.rs:520-524`
- Modify: `crates/hydeclaw-core/src/agent/providers_openai.rs:633-654`
- Modify: `crates/hydeclaw-core/src/agent/providers_anthropic.rs:~470`
- Modify: `crates/hydeclaw-core/src/agent/providers_google.rs:~456`
- Modify: `crates/hydeclaw-core/src/agent/providers_http.rs:187-245`

- [ ] **Step 2.1: OpenAI — child token**

In `providers_openai.rs`, find the `stream_with_cancellation` call (line ~520). Change:
```rust
// Before:
self.cancel.clone(),
// After:
self.cancel.child_token(),
```

- [ ] **Step 2.2: OpenAI — PartialState construction**

In `providers_openai.rs`, in the cancel exit block (line ~633), replace `partial_text: full_content.clone()` with `partial_state` in all 4 match arms. OpenAI tracks both text and tool calls, so:

```rust
if let Some(reason) = slot.get() {
    use crate::agent::providers::error::{CancelReason, PartialState};
    let partial_state = if !tool_call_parts.is_empty() {
        PartialState::ToolUse
    } else if !full_content.is_empty() {
        PartialState::Text(full_content.clone())
    } else {
        PartialState::Empty
    };
    let err = match reason {
        CancelReason::InactivityTimeout { silent_secs } => LlmCallError::InactivityTimeout {
            provider: self.name().to_string(),
            silent_secs,
            partial_state,
        },
        CancelReason::MaxDurationExceeded { elapsed_secs } => LlmCallError::MaxDurationExceeded {
            provider: self.name().to_string(),
            elapsed_secs,
            partial_state,
        },
        CancelReason::UserCancelled => LlmCallError::UserCancelled { partial_state },
        CancelReason::ShutdownDrain => LlmCallError::ShutdownDrain { partial_state },
    };
    return Err(anyhow::Error::new(err));
}
```

- [ ] **Step 2.3: Anthropic — child token + PartialState**

In `providers_anthropic.rs`, find the `stream_with_cancellation` call (line ~470). Change:
```rust
self.cancel.clone()  →  self.cancel.child_token()
```

Find the cancel exit block (similar pattern to OpenAI). Anthropic is text-only (no tool_call_parts during streaming), so:

```rust
if let Some(reason) = slot.get() {
    use crate::agent::providers::error::{CancelReason, PartialState};
    let partial_state = if !full_content.is_empty() {
        PartialState::Text(full_content.clone())
    } else {
        PartialState::Empty
    };
    let err = match reason {
        CancelReason::InactivityTimeout { silent_secs } => LlmCallError::InactivityTimeout {
            provider: self.name().to_string(),
            silent_secs,
            partial_state,
        },
        CancelReason::MaxDurationExceeded { elapsed_secs } => LlmCallError::MaxDurationExceeded {
            provider: self.name().to_string(),
            elapsed_secs,
            partial_state,
        },
        CancelReason::UserCancelled => LlmCallError::UserCancelled { partial_state },
        CancelReason::ShutdownDrain => LlmCallError::ShutdownDrain { partial_state },
    };
    return Err(anyhow::Error::new(err));
}
```

- [ ] **Step 2.4: Google — child token + PartialState**

Same as Anthropic: `self.cancel.child_token()` at line ~456. Same text-only `PartialState` logic.

- [ ] **Step 2.5: providers_http.rs — PartialState in `parse_sse_stream`**

In `parse_sse_stream` (line ~225), the local variable is `partial_text: String`. Replace the cancel exit block:

```rust
if let Some(reason) = slot.get() {
    use crate::agent::providers::error::{CancelReason, PartialState};
    let partial_state = if !partial_text.is_empty() {
        PartialState::Text(partial_text.clone())
    } else {
        PartialState::Empty
    };
    let err = match reason {
        CancelReason::InactivityTimeout { silent_secs } => LlmCallError::InactivityTimeout {
            provider: provider_name.to_string(),
            silent_secs,
            partial_state,
        },
        CancelReason::MaxDurationExceeded { elapsed_secs } => LlmCallError::MaxDurationExceeded {
            provider: provider_name.to_string(),
            elapsed_secs,
            partial_state,
        },
        CancelReason::UserCancelled => LlmCallError::UserCancelled { partial_state },
        CancelReason::ShutdownDrain => LlmCallError::ShutdownDrain { partial_state },
    };
    return Err(anyhow::Error::new(err));
}
```

- [ ] **Step 2.6: Compile check**

```
cargo check -p hydeclaw-core 2>&1 | head -40
```
Expected: no errors.

- [ ] **Step 2.7: Run error.rs tests**

```
cargo test -p hydeclaw-core providers::error -- --nocapture
```
Expected: all pass including the new `partial_state_*` and `inactivity_not_failover_worthy_after_r1` tests.

- [ ] **Step 2.8: Commit Task 1 + Task 2 together**

```bash
git add crates/hydeclaw-core/src/agent/providers/error.rs \
        crates/hydeclaw-core/src/agent/providers_openai.rs \
        crates/hydeclaw-core/src/agent/providers_anthropic.rs \
        crates/hydeclaw-core/src/agent/providers_google.rs \
        crates/hydeclaw-core/src/agent/providers_http.rs
git commit -m "feat(providers): PartialState enum + child_token isolation for timeout retry (R1, R3-R5)"
```

---

## Task 3: `run_max_duration_secs` + `LlmProvider` trait methods (R2, R6b)

**Files:**
- Modify: `crates/hydeclaw-core/src/agent/providers/timeouts.rs`
- Modify: `crates/hydeclaw-core/src/agent/providers.rs`
- Modify: `crates/hydeclaw-core/src/agent/providers_openai.rs`
- Modify: `crates/hydeclaw-core/src/agent/providers_anthropic.rs`
- Modify: `crates/hydeclaw-core/src/agent/providers_google.rs`

- [ ] **Step 3.1: Write failing tests**

Add to `timeouts.rs` test module:
```rust
#[test]
fn run_max_duration_secs_defaults_to_zero() {
    assert_eq!(TimeoutsConfig::default().run_max_duration_secs, 0);
}

#[test]
fn run_max_duration_secs_round_trips_json() {
    let input = r#"{"run_max_duration_secs": 3600}"#;
    let cfg: TimeoutsConfig = serde_json::from_str(input).unwrap();
    assert_eq!(cfg.run_max_duration_secs, 3600);
}

#[test]
fn run_max_duration_secs_zero_is_infinite_is_valid() {
    let cfg = TimeoutsConfig { run_max_duration_secs: 0, ..Default::default() };
    assert!(cfg.validate().is_ok());
}
```

- [ ] **Step 3.2: Run — fail**

```
cargo test -p hydeclaw-core providers::timeouts -- --nocapture 2>&1 | tail -10
```
Expected: `run_max_duration_secs` field not found.

- [ ] **Step 3.3: Add field to `TimeoutsConfig`**

In `timeouts.rs`, add to the struct after `stream_max_duration_secs`:

```rust
/// Maximum wall-clock duration for ALL retry attempts combined (seconds).
/// Zero = no limit; the session runs until the model responds or user cancels.
#[serde(default)]
pub run_max_duration_secs: u64,
```

Update `Default` impl to include the new field:
```rust
impl Default for TimeoutsConfig {
    fn default() -> Self {
        Self {
            connect_secs: default_connect_secs(),
            request_secs: default_request_secs(),
            stream_inactivity_secs: default_stream_inactivity_secs(),
            stream_max_duration_secs: default_stream_max_duration_secs(),
            run_max_duration_secs: 0,
        }
    }
}
```

The `validate` method needs no change — 0 is valid, any positive value is valid (no upper bound on run deadline).

- [ ] **Step 3.4: Add default methods to `LlmProvider` trait**

In `providers.rs`, add two default methods to the `LlmProvider` trait (after `current_model`):

```rust
/// Maximum wall-clock duration for all retry attempts combined. 0 = infinite.
fn run_max_duration_secs(&self) -> u64 { 0 }

/// True when the provider supports Anthropic-style assistant prefill
/// (appending a partial assistant message so the model continues from it).
fn supports_prefill(&self) -> bool { false }
```

- [ ] **Step 3.5: Implement in concrete providers**

In `providers_openai.rs`, add to the `LlmProvider` impl for `OpenAiCompatibleProvider`:
```rust
fn run_max_duration_secs(&self) -> u64 { self.timeouts.run_max_duration_secs }
```

In `providers_anthropic.rs`, add to the `LlmProvider` impl:
```rust
fn run_max_duration_secs(&self) -> u64 { self.timeouts.run_max_duration_secs }
fn supports_prefill(&self) -> bool { true }
```

In `providers_google.rs`, add to the `LlmProvider` impl:
```rust
fn run_max_duration_secs(&self) -> u64 { self.timeouts.run_max_duration_secs }
```

`RoutingProvider` (in `providers.rs`) does NOT override these — its default returns 0 for both. The inner routes manage their own timeouts.

`UnconfiguredProvider` does NOT need overrides — the default 0 is fine (it always fails immediately).

- [ ] **Step 3.6: Run tests**

```
cargo test -p hydeclaw-core providers::timeouts -- --nocapture
```
Expected: all pass.

- [ ] **Step 3.7: Compile check**

```
cargo check -p hydeclaw-core 2>&1 | head -20
```

- [ ] **Step 3.8: Commit**

```bash
git add crates/hydeclaw-core/src/agent/providers/timeouts.rs \
        crates/hydeclaw-core/src/agent/providers.rs \
        crates/hydeclaw-core/src/agent/providers_openai.rs \
        crates/hydeclaw-core/src/agent/providers_anthropic.rs \
        crates/hydeclaw-core/src/agent/providers_google.rs
git commit -m "feat(providers): run_max_duration_secs in TimeoutsConfig + LlmProvider trait methods (R2, R6b)"
```

---

## Task 4: `LlmErrorClass::CallTimeout` (R6)

**Files:**
- Modify: `crates/hydeclaw-core/src/agent/error_classify.rs`
- Modify: `crates/hydeclaw-core/src/agent/localization.rs`

- [ ] **Step 4.1: Write failing test**

Add to `error_classify.rs` tests:
```rust
#[test]
fn classify_inactivity_timeout_is_call_timeout() {
    use crate::agent::providers::error::{LlmCallError, PartialState};
    let e = anyhow::Error::new(LlmCallError::InactivityTimeout {
        provider: "p".into(),
        silent_secs: 60,
        partial_state: PartialState::Empty,
    });
    assert_eq!(classify(&e), LlmErrorClass::CallTimeout);
}

#[test]
fn classify_max_duration_exceeded_is_call_timeout() {
    use crate::agent::providers::error::{LlmCallError, PartialState};
    let e = anyhow::Error::new(LlmCallError::MaxDurationExceeded {
        provider: "p".into(),
        elapsed_secs: 600,
        partial_state: PartialState::Empty,
    });
    assert_eq!(classify(&e), LlmErrorClass::CallTimeout);
}

#[test]
fn call_timeout_is_not_retryable() {
    assert!(!is_retryable(&LlmErrorClass::CallTimeout));
}
```

- [ ] **Step 4.2: Run — fail**

```
cargo test -p hydeclaw-core agent::error_classify -- --nocapture 2>&1 | tail -10
```
Expected: `CallTimeout` variant not found.

- [ ] **Step 4.3: Add `CallTimeout` to `LlmErrorClass`**

In `error_classify.rs`, add to the enum:
```rust
/// Stream inactivity or max-duration timeout — handled by outer deadline retry loop.
/// NOT retryable by the inner transient loop.
CallTimeout,
```

- [ ] **Step 4.4: Update `classify` to downcast first**

Replace the existing `classify` function:
```rust
pub fn classify(error: &anyhow::Error) -> LlmErrorClass {
    // Fast path: typed dispatch for LlmCallError variants.
    if let Some(llm_err) = error.downcast_ref::<crate::agent::providers::error::LlmCallError>() {
        use crate::agent::providers::error::LlmCallError::*;
        match llm_err {
            InactivityTimeout { .. } | MaxDurationExceeded { .. } => return LlmErrorClass::CallTimeout,
            _ => {}
        }
    }
    let msg = error.to_string();
    classify_str(&msg)
}
```

- [ ] **Step 4.5: Update exhaustive matches for `CallTimeout`**

In `cooldown_duration`, add:
```rust
LlmErrorClass::CallTimeout => Duration::ZERO,
```

In `user_message_lang`, add:
```rust
LlmErrorClass::CallTimeout => e.unknown,  // handled by retry UI, not error message
```

In `user_messages_not_empty` test, add `LlmErrorClass::CallTimeout` to the classes array:
```rust
let classes = [
    LlmErrorClass::ContextOverflow, LlmErrorClass::SessionCorruption,
    LlmErrorClass::TransientHttp, LlmErrorClass::RateLimit,
    LlmErrorClass::AuthPermanent, LlmErrorClass::Billing,
    LlmErrorClass::Overloaded, LlmErrorClass::CallTimeout,
    LlmErrorClass::Unknown,
];
```

Also update `retryable_check` test to assert `CallTimeout` is NOT retryable:
```rust
assert!(!is_retryable(&LlmErrorClass::CallTimeout));
```

- [ ] **Step 4.6: Run tests**

```
cargo test -p hydeclaw-core agent::error_classify -- --nocapture
```
Expected: all pass.

- [ ] **Step 4.7: Commit**

```bash
git add crates/hydeclaw-core/src/agent/error_classify.rs
git commit -m "feat(classify): add LlmErrorClass::CallTimeout for inactivity/max_duration timeouts (R6)"
```

---

## Task 5: `chat_stream_with_deadline_retry` (R7, R9)

**Files:**
- Modify: `crates/hydeclaw-core/src/agent/pipeline/llm_call.rs`

The new function wraps `chat_stream_with_transient_retry` in an outer loop that retries on `InactivityTimeout` / `MaxDurationExceeded`. Uses a special `__reconnecting__:` prefix on `chunk_tx` to signal the UI (signals are picked up by `forward_chunks_into_sink` in Task 6).

- [ ] **Step 5.1: Write failing unit tests**

Add after the `Compactor` trait definition in `llm_call.rs`:

```rust
#[cfg(test)]
mod deadline_retry_tests {
    use super::*;
    use crate::agent::providers::error::{LlmCallError, PartialState};
    use hydeclaw_types::{LlmResponse, ToolCall};
    use tokio_util::sync::CancellationToken;

    // Minimal no-op SessionManager substitute for tests — we test WAL path
    // via tracing/log inspection, not DB assertions.
    struct NoopCompact;
    #[async_trait::async_trait]
    impl Compactor for NoopCompact {
        async fn compact(&self, _messages: &mut Vec<hydeclaw_types::Message>) {}
    }

    fn ok_response() -> LlmResponse {
        LlmResponse {
            content: "done".into(),
            tool_calls: vec![],
            usage: None,
            finish_reason: None,
            model: None,
            provider: None,
            fallback_notice: None,
            tools_used: vec![],
            iterations: 0,
            thinking_blocks: vec![],
        }
    }

    /// Provider that fails on first call with InactivityTimeout, succeeds on second.
    struct RetryOnceProvider {
        calls: std::sync::atomic::AtomicU32,
    }

    impl RetryOnceProvider {
        fn new() -> Self { Self { calls: std::sync::atomic::AtomicU32::new(0) } }
    }

    #[async_trait::async_trait]
    impl crate::agent::providers::LlmProvider for RetryOnceProvider {
        async fn chat(&self, _m: &[hydeclaw_types::Message], _t: &[hydeclaw_types::ToolDefinition]) -> anyhow::Result<LlmResponse> {
            Ok(ok_response())
        }
        async fn chat_stream(&self, _m: &[hydeclaw_types::Message], _t: &[hydeclaw_types::ToolDefinition], tx: tokio::sync::mpsc::UnboundedSender<String>) -> anyhow::Result<LlmResponse> {
            let n = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if n == 0 {
                return Err(anyhow::Error::new(LlmCallError::InactivityTimeout {
                    provider: "test".into(),
                    silent_secs: 60,
                    partial_state: PartialState::Empty,
                }));
            }
            tx.send("done".into()).ok();
            Ok(ok_response())
        }
        fn name(&self) -> &str { "retry-once" }
    }

    #[tokio::test]
    async fn deadline_retry_succeeds_on_second_attempt() {
        let provider = RetryOnceProvider::new();
        let (chunk_tx, mut chunk_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let cancel = CancellationToken::new();
        let mut messages = vec![];
        let compact = NoopCompact;
        let session_id = uuid::Uuid::new_v4();

        // Use a mock SM — no DB in unit tests, WAL errors are non-fatal
        // We just need the function to not panic. Supply a disconnected pool.
        // Instead, test the function without WAL by checking return value.
        let result = chat_stream_with_deadline_retry_no_wal(
            &provider,
            &mut messages,
            &[],
            chunk_tx,
            &compact,
            &cancel,
            0, // infinite
        ).await;
        assert!(result.is_ok(), "expected Ok on second attempt, got {result:?}");

        // Drain chunks: first chunk should be __reconnecting__ signal
        let mut chunks = vec![];
        while let Ok(c) = chunk_rx.try_recv() { chunks.push(c); }
        assert!(chunks.iter().any(|c| c.starts_with("__reconnecting__:")),
            "expected __reconnecting__ chunk, got: {chunks:?}");
    }

    #[tokio::test]
    async fn deadline_retry_stops_on_user_cancel() {
        struct AlwaysInactiveProvider;
        #[async_trait::async_trait]
        impl crate::agent::providers::LlmProvider for AlwaysInactiveProvider {
            async fn chat(&self, _m: &[hydeclaw_types::Message], _t: &[hydeclaw_types::ToolDefinition]) -> anyhow::Result<LlmResponse> { Ok(ok_response()) }
            async fn chat_stream(&self, _m: &[hydeclaw_types::Message], _t: &[hydeclaw_types::ToolDefinition], _tx: tokio::sync::mpsc::UnboundedSender<String>) -> anyhow::Result<LlmResponse> {
                Err(anyhow::Error::new(LlmCallError::InactivityTimeout {
                    provider: "test".into(), silent_secs: 60, partial_state: PartialState::Empty,
                }))
            }
            fn name(&self) -> &str { "always-inactive" }
        }

        let cancel = CancellationToken::new();
        let (chunk_tx, _) = tokio::sync::mpsc::unbounded_channel::<String>();
        let mut messages = vec![];

        // Cancel immediately during the first backoff sleep
        let cancel_clone = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            cancel_clone.cancel();
        });

        let result = chat_stream_with_deadline_retry_no_wal(
            &AlwaysInactiveProvider,
            &mut messages,
            &[],
            chunk_tx,
            &NoopCompact,
            &cancel,
            0,
        ).await;

        let err = result.unwrap_err();
        assert!(err.downcast_ref::<LlmCallError>().map(|e| matches!(e, LlmCallError::UserCancelled { .. })).unwrap_or(false),
            "expected UserCancelled, got: {err}");
    }

    #[tokio::test]
    async fn deadline_retry_propagates_connect_timeout() {
        struct ConnectFailProvider;
        #[async_trait::async_trait]
        impl crate::agent::providers::LlmProvider for ConnectFailProvider {
            async fn chat(&self, _m: &[hydeclaw_types::Message], _t: &[hydeclaw_types::ToolDefinition]) -> anyhow::Result<LlmResponse> { Ok(ok_response()) }
            async fn chat_stream(&self, _m: &[hydeclaw_types::Message], _t: &[hydeclaw_types::ToolDefinition], _tx: tokio::sync::mpsc::UnboundedSender<String>) -> anyhow::Result<LlmResponse> {
                Err(anyhow::Error::new(LlmCallError::ConnectTimeout { provider: "test".into(), elapsed_secs: 10 }))
            }
            fn name(&self) -> &str { "connect-fail" }
        }

        let cancel = CancellationToken::new();
        let (chunk_tx, _) = tokio::sync::mpsc::unbounded_channel::<String>();
        let mut messages = vec![];

        let result = chat_stream_with_deadline_retry_no_wal(
            &ConnectFailProvider,
            &mut messages,
            &[],
            chunk_tx,
            &NoopCompact,
            &cancel,
            0,
        ).await;

        let err = result.unwrap_err();
        assert!(err.downcast_ref::<LlmCallError>().map(|e| matches!(e, LlmCallError::ConnectTimeout { .. })).unwrap_or(false),
            "ConnectTimeout must propagate without retry: {err}");
    }
}
```

Note: the tests use `chat_stream_with_deadline_retry_no_wal` (a test-only variant without `SessionManager`). The production function `chat_stream_with_deadline_retry` adds `sm: &SessionManager` for WAL events. This avoids a live DB in unit tests while testing all retry logic.

- [ ] **Step 5.2: Run — fail**

```
cargo test -p hydeclaw-core pipeline::llm_call::deadline_retry_tests -- --nocapture 2>&1 | tail -20
```
Expected: function not found.

- [ ] **Step 5.3: Add `RECONNECTING_PREFIX` constant and test helper**

At the top of `llm_call.rs`, add:
```rust
/// Special prefix sent on chunk_tx when the deadline retry loop retries.
/// Handled by `execute::forward_chunks_into_sink` to emit `StreamEvent::Reconnecting`.
pub(crate) const RECONNECTING_PREFIX: &str = "__reconnecting__:";
```

- [ ] **Step 5.4: Add `chat_stream_with_deadline_retry_no_wal` (test helper)**

Add before the `Compactor` trait or at the end of the file (inside a `#[cfg(test)]` block if you prefer, but placing it as a module-private free function is cleaner since the full function calls it):

```rust
/// Inner timeout-retry loop without WAL logging — extracted for unit testability.
/// Production callers use `chat_stream_with_deadline_retry` which wraps this.
async fn deadline_retry_inner(
    provider: &dyn LlmProvider,
    messages: &mut Vec<hydeclaw_types::Message>,
    tools: &[hydeclaw_types::ToolDefinition],
    chunk_tx: mpsc::UnboundedSender<String>,
    compact: &impl Compactor,
    session_cancel: &tokio_util::sync::CancellationToken,
    run_max_duration_secs: u64,
    on_retry: impl Fn(u32, u64),  // (attempt, delay_ms) — called before backoff sleep
) -> anyhow::Result<hydeclaw_types::LlmResponse> {
    use crate::agent::providers::error::{LlmCallError, PartialState};
    use hydeclaw_types::{Message, MessageRole};

    let base_messages = messages.clone();
    let mut attempt: u32 = 0;
    let run_started = std::time::Instant::now();

    loop {
        // Check run deadline
        if run_max_duration_secs > 0 {
            let elapsed = run_started.elapsed().as_secs();
            if elapsed >= run_max_duration_secs {
                return Err(anyhow::Error::new(LlmCallError::MaxDurationExceeded {
                    provider: provider.name().to_string(),
                    elapsed_secs: elapsed,
                    partial_state: PartialState::Empty,
                }));
            }
        }

        // Check cancellation before each attempt
        if session_cancel.is_cancelled() {
            return Err(anyhow::Error::new(LlmCallError::UserCancelled {
                partial_state: PartialState::Empty,
            }));
        }

        // Restore messages to base state for this attempt
        *messages = base_messages.clone();

        let result = chat_stream_with_transient_retry(
            provider,
            messages,
            tools,
            chunk_tx.clone(),
            compact,
        ).await;

        match result {
            Ok(r) => return Ok(r),
            Err(e) => {
                let call_err = e.downcast_ref::<LlmCallError>().cloned();
                match call_err {
                    // Propagate user/shutdown cancellations immediately
                    Some(LlmCallError::UserCancelled { .. }) | Some(LlmCallError::ShutdownDrain { .. }) => {
                        return Err(e);
                    }
                    // Timeout → retry
                    Some(ref te @ (LlmCallError::InactivityTimeout { .. } | LlmCallError::MaxDurationExceeded { .. })) => {
                        let partial_state = te.partial_state().cloned();
                        let is_resumable = partial_state.as_ref().map(|p| p.is_resumable()).unwrap_or(false);

                        attempt += 1;
                        // Exponential backoff: 2s, 4s, 8s, 16s, 30s cap
                        let delay_ms = (2u64.pow(attempt) * 1000).min(30_000);

                        on_retry(attempt, delay_ms);

                        // Signal the UI via chunk_tx (handled by forward_chunks_into_sink)
                        let signal = format!("{}{attempt}:{delay_ms}", RECONNECTING_PREFIX);
                        let _ = chunk_tx.send(signal);

                        tracing::warn!(
                            attempt,
                            delay_ms,
                            is_resumable,
                            reason = te.abort_reason().unwrap_or("unknown"),
                            "LLM call timed out, scheduling retry"
                        );

                        // Anthropic prefill: if partial text is available, inject as assistant
                        // message so the model continues from where it left off
                        if is_resumable && provider.supports_prefill() {
                            if let Some(PartialState::Text(ref partial)) = partial_state {
                                *messages = base_messages.clone();
                                messages.push(Message {
                                    role: MessageRole::Assistant,
                                    content: partial.clone(),
                                    tool_calls: None,
                                    tool_call_id: None,
                                    thinking_blocks: vec![],
                                });
                            }
                        }
                        // else: messages already restored to base_messages above

                        // Backoff with cancel check so user Stop interrupts immediately
                        tokio::select! {
                            biased;
                            _ = session_cancel.cancelled() => {
                                return Err(anyhow::Error::new(LlmCallError::UserCancelled {
                                    partial_state: PartialState::Empty,
                                }));
                            }
                            _ = tokio::time::sleep(std::time::Duration::from_millis(delay_ms)) => {}
                        }

                        continue;
                    }
                    // Other errors (ConnectTimeout, AuthError, etc.) propagate to routing
                    _ => return Err(e),
                }
            }
        }
    }
}
```

- [ ] **Step 5.5: Add test-only wrapper `chat_stream_with_deadline_retry_no_wal`**

```rust
#[cfg(test)]
pub(crate) async fn chat_stream_with_deadline_retry_no_wal(
    provider: &dyn LlmProvider,
    messages: &mut Vec<hydeclaw_types::Message>,
    tools: &[hydeclaw_types::ToolDefinition],
    chunk_tx: mpsc::UnboundedSender<String>,
    compact: &impl Compactor,
    session_cancel: &tokio_util::sync::CancellationToken,
    run_max_duration_secs: u64,
) -> anyhow::Result<hydeclaw_types::LlmResponse> {
    deadline_retry_inner(
        provider, messages, tools, chunk_tx, compact, session_cancel, run_max_duration_secs,
        |_attempt, _delay_ms| {},  // no-op on_retry for tests
    ).await
}
```

- [ ] **Step 5.6: Add production `chat_stream_with_deadline_retry`**

```rust
/// Streaming LLM call with deadline retry for timeout errors.
///
/// On `InactivityTimeout` or `MaxDurationExceeded`, retries with exponential
/// backoff (2s base, 30s cap). Logs a WAL `llm_retry` event on each retry.
/// Stops when the model succeeds, `run_max_duration_secs` is exceeded, or
/// `session_cancel` fires (user Stop).
///
/// Non-timeout errors (ConnectTimeout, AuthError, etc.) are returned immediately
/// so the routing layer can fail over.
pub async fn chat_stream_with_deadline_retry(
    provider: &dyn LlmProvider,
    messages: &mut Vec<hydeclaw_types::Message>,
    tools: &[hydeclaw_types::ToolDefinition],
    chunk_tx: mpsc::UnboundedSender<String>,
    compact: &impl Compactor,
    session_cancel: &tokio_util::sync::CancellationToken,
    run_max_duration_secs: u64,
    session_id: uuid::Uuid,
    sm: &crate::agent::session_manager::SessionManager,
) -> anyhow::Result<hydeclaw_types::LlmResponse> {
    deadline_retry_inner(
        provider, messages, tools, chunk_tx, compact, session_cancel, run_max_duration_secs,
        |attempt, delay_ms| {
            // WAL event is async but we're in a sync callback — fire-and-forget via a
            // detached tokio task. Non-fatal: WAL failure must not interrupt retry.
            // We clone the necessary data for the spawned task.
            let sm_db = sm.db().clone();
            tokio::spawn(async move {
                let details = serde_json::json!({
                    "attempt": attempt,
                    "delay_ms": delay_ms,
                });
                let sm2 = crate::agent::session_manager::SessionManager::new(sm_db);
                sm2.log_wal_event(session_id, "llm_retry", Some(details)).await.ok();
            });
        },
    ).await
}
```

Note: `sm.db()` needs to exist. If `SessionManager` exposes its pool, use `sm.db()`. Otherwise, the WAL spawn can be removed from the closure and handled via a channel. See step 5.7 for alternative if `sm.db()` is not accessible.

- [ ] **Step 5.7: Check SessionManager API — ensure `db()` accessor exists**

```
grep -n "pub fn db\|pub.*db:" crates/hydeclaw-core/src/agent/session_manager.rs | head -10
```

If `sm.db()` is not exposed, add it:
```rust
pub fn db(&self) -> &sqlx::PgPool { &self.db }
```

Or alternatively, change the WAL spawn in step 5.6 to pass `session_id` and `sm` directly if async is available. Since `deadline_retry_inner` is async itself, the `on_retry` callback could be an `async` closure — but Rust doesn't support `async Fn` traits cleanly yet. The fire-and-forget spawn is the pragmatic solution.

- [ ] **Step 5.8: Run deadline retry unit tests**

```
cargo test -p hydeclaw-core pipeline::llm_call::deadline_retry_tests -- --nocapture
```
Expected: all 3 tests pass.

- [ ] **Step 5.9: Run all llm_call tests**

```
cargo test -p hydeclaw-core pipeline::llm_call -- --nocapture
```
Expected: all pass.

- [ ] **Step 5.10: Commit**

```bash
git add crates/hydeclaw-core/src/agent/pipeline/llm_call.rs
git commit -m "feat(llm_call): chat_stream_with_deadline_retry + WAL llm_retry event (R7, R9)"
```

---

## Task 6: `execute.rs` call site + `StreamEvent::Reconnecting` (R8, R10 Rust)

**Files:**
- Modify: `crates/hydeclaw-core/src/agent/stream_event.rs`
- Modify: `crates/hydeclaw-core/src/gateway/mod.rs`
- Modify: `crates/hydeclaw-core/src/gateway/handlers/chat.rs`
- Modify: `crates/hydeclaw-core/src/agent/pipeline/execute.rs`

- [ ] **Step 6.1: Add `StreamEvent::Reconnecting` variant**

In `stream_event.rs`, add after `Error(String)`:
```rust
/// LLM deadline retry: model timed out and is being retried after `delay_ms`.
Reconnecting { attempt: u32, delay_ms: u64 },
```

- [ ] **Step 6.2: Add `RECONNECTING` constant to `sse_types`**

In `gateway/mod.rs`, inside the `sse_types` module (after `APPROVAL_RESOLVED`):
```rust
pub const RECONNECTING: &str = "reconnecting";
```

- [ ] **Step 6.3: Handle `StreamEvent::Reconnecting` in SSE converter**

In `gateway/handlers/chat.rs`, add a match arm in the main SSE converter loop. Find the `StreamEvent::Error` arm and add before it:

```rust
StreamEvent::Reconnecting { attempt, delay_ms } => {
    let data = json!({
        "type": sse_types::RECONNECTING,
        "attempt": attempt,
        "delay_ms": delay_ms,
    }).to_string();
    let _ = send_and_buffer!(data);
    continue;
}
```

- [ ] **Step 6.4: Update `emit_chunk` in `execute.rs` to handle `__reconnecting__:` prefix**

In `execute.rs`, inside `forward_chunks_into_sink`, find the `emit_chunk` inner function and update it:

```rust
async fn emit_chunk<S: EventSink>(
    sink: &mut S,
    chunk: String,
    partial: &mut String,
    first_err: &mut Option<anyhow::Error>,
) {
    // Handle reconnecting signal injected by chat_stream_with_deadline_retry.
    // Format: "__reconnecting__:{attempt}:{delay_ms}"
    if let Some(rest) = chunk.strip_prefix(crate::agent::pipeline::llm_call::RECONNECTING_PREFIX) {
        let mut parts = rest.splitn(2, ':');
        let attempt = parts.next().and_then(|s| s.parse::<u32>().ok()).unwrap_or(1);
        let delay_ms = parts.next().and_then(|s| s.parse::<u64>().ok()).unwrap_or(2000);
        match sink.emit(PipelineEvent::Stream(StreamEvent::Reconnecting { attempt, delay_ms })).await {
            Ok(()) | Err(SinkError::Closed) => {}
            Err(other) if first_err.is_none() => {
                *first_err = Some(anyhow::Error::new(other));
            }
            Err(_) => {}
        }
        return; // Do NOT add to partial text accumulator
    }
    partial.push_str(&chunk);
    match sink.emit(PipelineEvent::Stream(StreamEvent::TextDelta(chunk))).await {
        Ok(()) | Err(SinkError::Closed) => {}
        Err(other) if first_err.is_none() => {
            *first_err = Some(anyhow::Error::new(other));
        }
        Err(_) => {}
    }
}
```

- [ ] **Step 6.5: Update `execute.rs` call site**

In `execute.rs`, find the LLM call block around line 179-191. Replace:

```rust
let (chunk_tx, chunk_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
let provider = engine.cfg().provider.as_ref();
let llm_fut = crate::agent::pipeline::llm_call::chat_stream_with_transient_retry(
    provider,
    &mut messages,
    &tools,
    chunk_tx,
    engine,
);
```

With:

```rust
let (chunk_tx, chunk_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
let provider = engine.cfg().provider.as_ref();
let run_max = provider.run_max_duration_secs();
let llm_fut = crate::agent::pipeline::llm_call::chat_stream_with_deadline_retry(
    provider,
    &mut messages,
    &tools,
    chunk_tx,
    engine,
    &cancel,
    run_max,
    session_id,
    &sm,
);
```

`session_id`, `sm`, `provider`, and `cancel` are all already in scope via `BootstrapOutcome` destructuring and the existing `sm` / `cancel` variables.

- [ ] **Step 6.6: Compile check**

```
cargo check -p hydeclaw-core 2>&1 | head -40
```
Expected: no errors.

- [ ] **Step 6.7: Add test for `__reconnecting__:` prefix handling in `forward_chunks_into_sink`**

Add to the `tests` module in `execute.rs`:
```rust
/// Test C: __reconnecting__: chunks emitted by deadline_retry are forwarded as
/// Reconnecting events to the sink, NOT as TextDelta, and do NOT update partial.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reconnecting_prefix_emits_reconnecting_event_not_text_delta() {
    let (chunk_tx, chunk_rx) = mpsc::unbounded_channel::<String>();
    let llm_fut = async move {
        chunk_tx.send("__reconnecting__:1:2000".to_string()).unwrap();
        chunk_tx.send("Hello".to_string()).unwrap();
        drop(chunk_tx);
        Ok::<LlmResponse, anyhow::Error>(mk_response(vec![]))
    };

    let mut sink = MockSink::new();
    let (result, partial, err) = forward_chunks_into_sink(llm_fut, chunk_rx, &mut sink).await;
    assert!(err.is_none());
    assert!(result.is_ok());
    assert_eq!(partial, "Hello", "partial must not include the reconnecting signal");

    let reconnecting_events: Vec<_> = sink.events.iter().filter(|e| matches!(
        e, PipelineEvent::Stream(StreamEvent::Reconnecting { .. })
    )).collect();
    assert_eq!(reconnecting_events.len(), 1, "expected 1 Reconnecting event");

    let text_deltas = text_deltas(&sink);
    assert_eq!(text_deltas, vec!["Hello"], "TextDelta must only contain real text");
}
```

- [ ] **Step 6.8: Run execute.rs tests**

```
cargo test -p hydeclaw-core pipeline::execute -- --nocapture
```
Expected: all 3 tests pass (including the new reconnecting test).

- [ ] **Step 6.9: Full compile + test**

```
cargo test -p hydeclaw-core 2>&1 | tail -20
```
Expected: all tests pass.

- [ ] **Step 6.10: Commit**

```bash
git add crates/hydeclaw-core/src/agent/stream_event.rs \
        crates/hydeclaw-core/src/gateway/mod.rs \
        crates/hydeclaw-core/src/gateway/handlers/chat.rs \
        crates/hydeclaw-core/src/agent/pipeline/execute.rs
git commit -m "feat(execute): wire chat_stream_with_deadline_retry + StreamEvent::Reconnecting (R8, R10)"
```

---

## Task 7: Frontend — `reconnecting` SSE event (R10 Frontend)

**Files:**
- Modify: `ui/src/stores/sse-events.ts`
- Modify: `ui/src/stores/chat-types.ts`

- [ ] **Step 7.1: Write failing frontend test**

In `ui/src/stores/sse-events.test.ts` (create if not exists, or add to existing):
```typescript
import { parseSseEvent } from "./sse-events";

it("parseSseEvent handles reconnecting event", () => {
  const result = parseSseEvent(JSON.stringify({ type: "reconnecting", attempt: 2, delay_ms: 4000 }));
  expect(result).toEqual({ type: "reconnecting", attempt: 2, delay_ms: 4000 });
});

it("parseSseEvent returns null for unknown type", () => {
  const result = parseSseEvent(JSON.stringify({ type: "unknown-type" }));
  expect(result).toBeNull();
});
```

Check where sse-events tests live:
```
find ui/src -name "*.test.ts" | grep -i sse | head -5
```

- [ ] **Step 7.2: Run — fail**

```
cd ui && npm test -- --run src/stores/sse-events 2>&1 | tail -20
```
Expected: `reconnecting` not handled, test fails.

- [ ] **Step 7.3: Add `reconnecting` to `SseEvent` union in `sse-events.ts`**

Add to the `SseEvent` type (after the `error` variant):
```typescript
| { type: "reconnecting"; attempt: number; delay_ms: number }
```

- [ ] **Step 7.4: Add `"reconnecting"` case to `parseSseEvent`**

Add before the `default:` case:
```typescript
case "reconnecting":
  return {
    type,
    attempt: typeof e.attempt === "number" ? e.attempt : 1,
    delay_ms: typeof e.delay_ms === "number" ? e.delay_ms : 2000,
  };
```

- [ ] **Step 7.5: Run sse-events test — pass**

```
cd ui && npm test -- --run src/stores/sse-events 2>&1 | tail -10
```
Expected: pass.

- [ ] **Step 7.6: Add `isLlmReconnecting: boolean` to `AgentState` in `chat-types.ts`**

In `AgentState` interface (after `reconnectAttempt`):
```typescript
/** True while the LLM deadline retry loop is backing off before next attempt. */
isLlmReconnecting: boolean;
```

Update `emptyAgentState()`:
```typescript
isLlmReconnecting: false,
```

- [ ] **Step 7.7: Handle `reconnecting` event in chat-store.ts**

Find the SSE event handler in `chat-store.ts` (in `streaming-renderer.ts` or the store's `startStream` method). In the `switch(event.type)` block, add:

```typescript
case "reconnecting":
  set(produce((s: ChatStore) => {
    const agent = s.agents[agentName];
    if (agent) agent.isLlmReconnecting = true;
  }));
  break;
```

Reset `isLlmReconnecting` to `false` on `text-start`, `finish`, and `error` events (add to existing cases):
```typescript
case "text-start":
  set(produce((s: ChatStore) => {
    const agent = s.agents[agentName];
    if (agent) {
      agent.isLlmReconnecting = false;
      // ... existing text-start handling
    }
  }));
  break;
```

Do the same for `finish` and `error` cases — clear `isLlmReconnecting = false`.

- [ ] **Step 7.8: TypeScript build check**

```
cd ui && npm run build 2>&1 | tail -20
```
Expected: no TypeScript errors.

- [ ] **Step 7.9: Frontend tests**

```
cd ui && npm test 2>&1 | tail -20
```
Expected: all pass.

- [ ] **Step 7.10: Commit**

```bash
git add ui/src/stores/sse-events.ts ui/src/stores/chat-types.ts ui/src/stores/chat-store.ts
git commit -m "feat(ui): handle reconnecting SSE event + isLlmReconnecting flag in AgentState (R10)"
```

---

## Final Verification

- [ ] **Full Rust test suite**

```
cargo test 2>&1 | tail -30
```
Expected: all pass.

- [ ] **Full lint**

```
make lint 2>&1 | tail -20
```
Expected: no warnings.

- [ ] **Full UI build**

```
cd ui && npm run build 2>&1 | tail -10
```
Expected: clean build.

---

## Spec Coverage Check

| Spec ID | Task | Covered |
|---------|------|---------|
| R1 — `PartialState` enum | Task 1 | ✓ |
| R1b — Update 4 providers | Task 2 | ✓ (openai, anthropic, google, http) |
| R2 — `run_max_duration_secs` | Task 3 | ✓ |
| R3–R5 — child tokens | Task 2 | ✓ |
| R6 — `CallTimeout` class | Task 4 | ✓ |
| R6b — `LlmProvider` trait methods | Task 3 | ✓ |
| R7 — `chat_stream_with_deadline_retry` | Task 5 | ✓ |
| R8 — `execute.rs` call site + reconnecting signal | Task 6 | ✓ |
| R9 — WAL `llm_retry` event | Task 5 | ✓ (fire-and-forget spawn) |
| R10 — `StreamEvent::Reconnecting` + frontend | Tasks 6–7 | ✓ |
