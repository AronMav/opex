//! Composable behaviour layers for `pipeline::execute`.
//!
//! Five orthogonal opt-in policies — fallback provider, auto-continue,
//! session-corruption recovery, tool-policy override, forced final call —
//! that today live only inside `engine::stream::handle_isolated`. This
//! module re-expresses them as **data-only** policy structs bundled into
//! `BehaviourLayers`, which `pipeline::execute` consults at well-defined
//! insertion points.
//!
//! See `docs/architecture/2026-05-06-llm-loop-unification-plan.md` for the
//! full divergent-feature map (Appendix A) and the phased delivery plan
//! that this module is part of.
//!
//! ## Design choice — data, not traits
//!
//! Each layer is a tiny configuration struct, not a trait object. Two
//! reasons:
//!
//! 1. **One implementation per layer ever.** Trait-object indirection
//!    would buy us nothing — there's no second strategy waiting to plug
//!    in alongside the cron-style fallback policy.
//! 2. **Span tree clarity.** `dyn Trait` calls add a `span!` boundary
//!    that doesn't correspond to any architecturally-meaningful
//!    boundary. The `if let Some(_) = layers.fallback_provider` guards
//!    in `execute()` keep the span tree flat.
//!
//! Behaviour layers are **read-only policy** consulted by `execute()`.
//! All mutable state (`consecutive_failures`, `auto_continue_count`,
//! `did_reset_session`, …) stays as `let mut` locals inside `execute()`
//! and is gated by the relevant layer's presence. No layer carries
//! state across iterations.

use crate::config::AgentToolPolicy;
use std::sync::Arc;

// ── Constants ────────────────────────────────────────────────────────────────

/// User message injected by the auto-continue layer when the LLM ends a
/// turn describing remaining work but doesn't actually execute it. Lives
/// here (next to `AutoContinuePolicy`) rather than in any caller because
/// the layer owns the definition of what "auto-continue" means.
pub const AUTO_CONTINUE_NUDGE: &str =
    "[system] You described remaining steps but didn't execute them. \
     Continue and complete the task using tools.";

// ── Individual layer policies ────────────────────────────────────────────────

/// Switch the live LLM provider to a fallback after N consecutive failures.
///
/// **Trigger.** LLM call returns `Err(_)` and the consecutive-failure
/// counter has crossed `consecutive_failure_threshold`.
///
/// **Action.** Lazily construct the fallback via the supplied builder
/// (typically `engine.create_fallback_provider().await`). If construction
/// returns `Some(_)`, the engine swaps the live provider for the fallback
/// for all subsequent iterations of this turn. The consecutive-failure
/// counter resets to 0 on the swap and on every subsequent successful
/// call (whether on primary or fallback).
///
/// **Why a closure-style builder, not a pre-resolved `Arc`.** Construction
/// is async and can fail; the engine should only do that work when the
/// threshold actually trips. Carrying a builder lets us defer.
#[derive(Clone)]
pub struct FallbackPolicy {
    /// Failures-in-a-row threshold above which the layer attempts to
    /// switch. Mirrors the legacy `loop_config.max_consecutive_failures`
    /// so the cron path's behaviour is preserved exactly.
    pub consecutive_failure_threshold: usize,
}

impl FallbackPolicy {
    pub fn from_loop_config(loop_config: &crate::agent::tool_loop::ToolLoopConfig) -> Self {
        Self {
            consecutive_failure_threshold: loop_config.max_consecutive_failures,
        }
    }
}

/// Nudge the LLM to continue when a no-tool-calls turn produced text that
/// looks incomplete (the classic "I'll do X next" without actually doing
/// it).
///
/// **Trigger.** No tool calls, response text non-empty, `looks_incomplete`
/// returns true, and we're under the per-turn nudge cap.
///
/// **Action.** Push a constant nudge user message into the context
/// (`AUTO_CONTINUE_NUDGE`), increment the counter, `continue` the loop.
/// On nudge, the layer also fires an `auto_continue` notification to
/// `ui_event_tx` if one is wired.
///
/// **`retry_on_empty`** folds in the legacy `empty_retry_count` behaviour
/// — when an LLM returns no text at all (rare, some providers do this on
/// 429s), retry the turn once before reporting an empty response.
#[derive(Clone)]
pub struct AutoContinuePolicy {
    /// Maximum nudges per turn. Mirrors the legacy
    /// `loop_config.max_auto_continues`.
    pub max_continues: u8,
    /// Retry once on an empty response before breaking.
    pub retry_on_empty: bool,
}

impl AutoContinuePolicy {
    pub fn from_loop_config(loop_config: &crate::agent::tool_loop::ToolLoopConfig) -> Self {
        Self {
            max_continues: loop_config.max_auto_continues,
            retry_on_empty: true,
        }
    }
}

/// Rebuild the message list when the provider reports a corrupted
/// session ("messages list is in an invalid order", malformed thinking
/// blocks, …). One-shot per turn.
///
/// **Trigger.** LLM call returns `Err(e)`,
/// `error_classify::classify(&e) == LlmErrorClass::SessionCorruption`,
/// and the per-turn flag hasn't fired yet.
///
/// **Action.** Retain only `MessageRole::System` messages, push a fresh
/// user message with the original prompt text, recompute `context_chars`,
/// `continue`. The next iteration retries on the cleaned context with the
/// same provider.
///
/// **Order.** Must be checked **before** the fallback layer — a
/// SessionCorruption error shouldn't increment the consecutive-failure
/// counter that drives fallback.
#[derive(Clone)]
pub struct SessionRecoveryPolicy {
    /// The original user text to re-seed after the reset. We carry this
    /// in the policy so the layer doesn't have to dig it out of `messages`
    /// after the reset (which would race with the system-only retain).
    pub original_user_text: String,
}

/// Override the agent's normal tool allowlist for this turn.
///
/// **Trigger.** Bootstrap-time only — applied once before the loop starts.
///
/// **Action.** Bootstrap calls
/// `engine.apply_tool_policy_override(tools, &policy)` to filter the
/// available tools. Used by cron jobs to narrow blast radius for
/// scheduled runs.
///
/// **Architectural note.** Lives in `BehaviourLayers` so the cron caller
/// can express its policy in one place; bootstrap consumes it. The
/// loop body itself never reads this field — by the time `execute()`
/// runs, `available_tools` already reflects the override.
#[derive(Clone)]
pub struct ToolPolicyOverride {
    pub policy: AgentToolPolicy,
}

/// On loop break / iteration limit, perform one extra LLM call with an
/// empty tools list to coax a final natural-language summary.
///
/// **Trigger.** `loop_broken || iteration == max - 1`.
///
/// **Action.** `provider.chat(&messages, &[], CallOptions::default())`
/// once. The response replaces `final_response`. If the call itself
/// errors, `final_response` is set to a graceful user-facing error
/// instead of bubbling the error up.
///
/// **Why a layer, not a default.** The SSE path doesn't want a bonus
/// LLM call on iteration limit — it returns a `Finish { reason:
/// "turn_limit" }` and lets the user see it. The cron path has nothing
/// rendering the SSE finish event, so it needs the extra call to
/// produce a final string for the channel response.
#[derive(Clone)]
pub struct ForcedFinalCallPolicy;

// ── Composite ────────────────────────────────────────────────────────────────

/// Bundle of opt-in behaviour layers passed to `pipeline::execute`.
///
/// `default()` (and the SSE caller) leaves every layer `None` — the loop
/// runs with vanilla semantics, no fallback, no auto-continue, no session
/// recovery. The cron caller calls `for_cron(...)` to populate the same
/// set of layers `handle_isolated` enables today.
///
/// Construction is cheap (clones of small structs); the runtime cost
/// inside `execute()` is one `if let Some(_)` check per layer per
/// iteration — the same shape as today's branching, just localised to
/// well-defined insertion points.
#[derive(Clone, Default)]
pub struct BehaviourLayers {
    pub fallback_provider: Option<FallbackPolicy>,
    pub auto_continue: Option<AutoContinuePolicy>,
    pub session_recovery: Option<SessionRecoveryPolicy>,
    pub tool_policy_override: Option<ToolPolicyOverride>,
    pub forced_final_call: Option<ForcedFinalCallPolicy>,
}

impl BehaviourLayers {
    /// All layers off. Used by the SSE chat path.
    pub fn none() -> Self {
        Self::default()
    }

    /// The same set of layers `engine::stream::handle_isolated` enables
    /// today, for use by cron jobs and other RPC-style callers.
    ///
    /// `tool_policy_override` is sourced from `msg.tool_policy_override`
    /// (the optional JSON blob carried by the IncomingMessage); if the
    /// blob is absent or malformed, the layer stays `None` so behaviour
    /// matches today's "no override applied" path.
    pub fn for_cron(
        loop_config: &crate::agent::tool_loop::ToolLoopConfig,
        msg: &hydeclaw_types::IncomingMessage,
    ) -> Self {
        let user_text = msg.text.clone().unwrap_or_default();

        let tool_policy_override = msg.tool_policy_override.as_ref().and_then(|json| {
            serde_json::from_value::<AgentToolPolicy>(json.clone()).ok()
                .map(|policy| ToolPolicyOverride { policy })
        });

        Self {
            fallback_provider: Some(FallbackPolicy::from_loop_config(loop_config)),
            auto_continue: Some(AutoContinuePolicy::from_loop_config(loop_config)),
            session_recovery: Some(SessionRecoveryPolicy {
                original_user_text: user_text,
            }),
            tool_policy_override,
            forced_final_call: Some(ForcedFinalCallPolicy),
        }
    }
}

// ── Runtime layer state ──────────────────────────────────────────────────────
//
// Each layer's mutable counters live here as a single struct rather than
// being scattered across `let mut` bindings in `execute()`. Keeps the
// loop body's local scope readable without forcing the layers themselves
// to carry mutable state.

/// Per-turn mutable state owned by the layers. Initialised at the top of
/// the iteration loop, read+written inside the loop body, dropped when
/// the turn ends. Never seen by callers.
///
/// `Default` initialises all counters to 0 / false; the actual `mut` is
/// in `execute()` so the borrow checker treats this as a normal local.
#[derive(Default)]
pub struct LayerRuntimeState {
    pub consecutive_failures: usize,
    pub using_fallback: bool,
    pub fallback_provider: Option<Arc<dyn crate::agent::providers::LlmProvider>>,
    pub did_reset_session: bool,
    pub auto_continue_count: u8,
    pub empty_retry_count: u8,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `BehaviourLayers::none()` matches `default()` — the SSE caller
    /// uses `none()` for clarity but functionally they're identical.
    #[test]
    fn none_equals_default() {
        let a = BehaviourLayers::none();
        let b = BehaviourLayers::default();
        assert!(a.fallback_provider.is_none() && b.fallback_provider.is_none());
        assert!(a.auto_continue.is_none() && b.auto_continue.is_none());
        assert!(a.session_recovery.is_none() && b.session_recovery.is_none());
        assert!(a.tool_policy_override.is_none() && b.tool_policy_override.is_none());
        assert!(a.forced_final_call.is_none() && b.forced_final_call.is_none());
    }

    /// `LayerRuntimeState::default()` is the zero state every iteration
    /// loop starts from. Confirm no surprising non-zero defaults.
    #[test]
    fn runtime_state_defaults_are_zero() {
        let s = LayerRuntimeState::default();
        assert_eq!(s.consecutive_failures, 0);
        assert!(!s.using_fallback);
        assert!(s.fallback_provider.is_none());
        assert!(!s.did_reset_session);
        assert_eq!(s.auto_continue_count, 0);
        assert_eq!(s.empty_retry_count, 0);
    }

    /// Build a minimal `IncomingMessage` for layer-construction tests.
    /// Centralises the boilerplate so future field additions touch one
    /// place; the layer code only reads `text` and `tool_policy_override`.
    fn mk_msg(
        text: Option<&str>,
        tool_policy_override: Option<serde_json::Value>,
    ) -> hydeclaw_types::IncomingMessage {
        hydeclaw_types::IncomingMessage {
            user_id: "user1".to_string(),
            context: serde_json::Value::Null,
            text: text.map(|s| s.to_string()),
            attachments: vec![],
            agent_id: "agent1".to_string(),
            channel: "ui".to_string(),
            timestamp: chrono::Utc::now(),
            formatting_prompt: None,
            tool_policy_override,
            leaf_message_id: None,
            user_message_id: None,
        }
    }

    /// `for_cron` populates all five layers with the loop_config-derived
    /// values; mirrors what `handle_isolated` enables today.
    #[test]
    fn for_cron_populates_all_five_layers() {
        let loop_config = crate::agent::tool_loop::ToolLoopConfig {
            max_consecutive_failures: 3,
            max_auto_continues: 2,
            ..Default::default()
        };
        let msg = mk_msg(Some("hello"), None);
        let layers = BehaviourLayers::for_cron(&loop_config, &msg);

        let fb = layers.fallback_provider.expect("fallback layer present");
        assert_eq!(fb.consecutive_failure_threshold, 3);

        let ac = layers.auto_continue.expect("auto-continue layer present");
        assert_eq!(ac.max_continues, 2);
        assert!(ac.retry_on_empty);

        let sr = layers.session_recovery.expect("session-recovery layer present");
        assert_eq!(sr.original_user_text, "hello");

        // No JSON override on the message → layer stays None.
        assert!(layers.tool_policy_override.is_none());

        assert!(layers.forced_final_call.is_some());
    }

    /// When `tool_policy_override` is the wrong shape (e.g. a JSON
    /// array instead of an object), the layer falls through to None
    /// instead of failing — matches the legacy "best-effort" handling
    /// in handle_isolated.
    ///
    /// Note: every field on `AgentToolPolicy` is `#[serde(default)]`,
    /// so an empty object `{}` or any object with unknown keys
    /// deserializes successfully into the default policy. Only
    /// non-object JSON (arrays, scalars) actually fails the parse.
    #[test]
    fn for_cron_swallows_invalid_tool_policy_override_json() {
        let loop_config = crate::agent::tool_loop::ToolLoopConfig::default();
        let msg = mk_msg(
            None,
            // JSON array — wrong shape, can't deserialize as an object.
            Some(serde_json::json!(["not", "a", "policy"])),
        );
        let layers = BehaviourLayers::for_cron(&loop_config, &msg);
        assert!(layers.tool_policy_override.is_none());
    }

    /// A well-formed `AgentToolPolicy` JSON object lands in the layer.
    /// Pinning the happy path so a future regression in `for_cron`'s
    /// JSON wiring would surface as a failed test instead of silently
    /// dropping the override on production cron jobs.
    #[test]
    fn for_cron_accepts_valid_tool_policy_override_json() {
        let loop_config = crate::agent::tool_loop::ToolLoopConfig::default();
        let msg = mk_msg(
            None,
            Some(serde_json::json!({
                "deny": ["code_exec"],
                "deny_all_others": false,
            })),
        );
        let layers = BehaviourLayers::for_cron(&loop_config, &msg);
        let override_layer = layers.tool_policy_override
            .expect("valid JSON object should populate the layer");
        assert_eq!(override_layer.policy.deny, vec!["code_exec"]);
    }
}
