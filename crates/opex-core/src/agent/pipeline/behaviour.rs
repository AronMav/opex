//! Composable behaviour layers for `pipeline::execute`.
//!
//! Five orthogonal opt-in policies — fallback provider, auto-continue,
//! session-corruption recovery, tool-policy override, forced final call —
//! expressed as **data-only** policy structs bundled into `BehaviourLayers`,
//! which `pipeline::execute` consults at well-defined insertion points.
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

/// Tool result injected when the interrupted-verify guard blocks a batch on an
/// autonomous re-drive. Worded as an `Error:`-style result the model reads and
/// acts on (verify the prior effect, then decide whether to repeat).
pub const INTERRUPTED_VERIFY_BLOCK_RESULT: &str =
    "Error: blocked by the interrupted-verify guard. The previous turn was \
     interrupted before this tool's result was recorded, so this action may have \
     ALREADY taken effect. Do NOT blindly repeat it — first verify the current \
     state with a read-only check (read the file, list the directory/process, \
     inspect the result), then decide whether the action still needs to run.";

/// System tools whose side-effects are NOT idempotent: re-running them after a
/// crash-interrupted turn risks double-applying (run code twice, delete/rename a
/// path again, start a second process). Gated by the interrupted-verify guard.
/// Committed tool results are already replay-safe via the cache; this list only
/// matters for the narrow window where a result was lost before persistence.
pub const NON_IDEMPOTENT_TOOLS: &[&str] =
    &["code_exec", "process", "workspace_delete", "workspace_rename"];

/// Whether `name` is a non-idempotent system tool — see [`NON_IDEMPOTENT_TOOLS`].
pub fn is_non_idempotent_tool(name: &str) -> bool {
    NON_IDEMPOTENT_TOOLS.contains(&name)
}

/// True when the most recent tool result in the context is an un-cleared
/// `[interrupted:verify]` marker — a prior tool call whose outcome is unknown
/// because the turn crashed before its result was recorded. "Cleared" simply
/// means a later real tool result superseded it (so only the LAST tool result
/// is examined).
pub fn last_tool_result_is_interrupted_verify(messages: &[opex_types::Message]) -> bool {
    messages
        .iter()
        .rev()
        .find(|m| m.role == opex_types::MessageRole::Tool)
        .is_some_and(|m| m.content.starts_with(crate::db::sessions::INTERRUPTED_VERIFY_TAG))
}

/// The interrupted-verify guard decision: block this batch when the last tool
/// outcome is an un-cleared `[interrupted:verify]` marker AND the batch includes
/// a non-idempotent tool. (Layer presence is checked separately by the caller.)
pub fn should_block_interrupted_batch(
    messages: &[opex_types::Message],
    tool_calls: &[opex_types::ToolCall],
) -> bool {
    last_tool_result_is_interrupted_verify(messages)
        && tool_calls.iter().any(|tc| is_non_idempotent_tool(&tc.name))
}

// ── Individual layer policies ────────────────────────────────────────────────

/// Switch the live LLM provider to a fallback after N consecutive failures.
///
/// **Trigger.** LLM call returns `Err(_)` and either the error is
/// failover-worthy (`is_failover_worthy()` — transport/Unknown errors swap
/// immediately, on the first failure) OR the consecutive-failure counter has
/// crossed `consecutive_failure_threshold`.
///
/// **Action.** Lazily construct the fallback via the supplied builder
/// (typically `engine.create_fallback_provider(chain_idx).await`, walking
/// the profile's `text` chain one reserve at a time via `chain_idx`). If
/// construction returns `Some(_)`, the engine swaps the live provider for the
/// fallback for all subsequent iterations of this turn. The
/// consecutive-failure counter resets to 0 on the swap and on every
/// subsequent successful call (whether on primary or fallback).
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

/// Guard the autonomous re-drive path from blindly repeating a non-idempotent
/// tool whose prior outcome is unknown.
///
/// **Trigger.** Engaged only on autonomous (cron / goal re-drive) turns
/// (`for_cron`). [`should_block_interrupted_batch`] returns true: the most recent
/// tool result is an un-cleared `[interrupted:verify]` marker AND the model's
/// current batch includes a tool in [`NON_IDEMPOTENT_TOOLS`].
///
/// **Action.** `execute()` refuses to dispatch the batch; it injects
/// [`INTERRUPTED_VERIFY_BLOCK_RESULT`] as the result for every call in the batch
/// and continues, forcing the model to verify before repeating. Bounded to a
/// single checkpoint: the injected result is no longer an `[interrupted:verify]`
/// marker, so the guard does not fire again on the next iteration.
///
/// **Why a layer.** Defense-in-depth ON TOP of committed-result cache-replay
/// (a persisted `role='tool'` is never re-executed). It covers only the narrow
/// window where a non-idempotent tool's result was lost before persistence. The
/// interactive path leaves it `None` — a human is present to judge.
#[derive(Clone)]
pub struct InterruptedVerifyGuardPolicy;

// ── Composite ────────────────────────────────────────────────────────────────

/// Bundle of opt-in behaviour layers passed to `pipeline::execute`.
///
/// `default()` (and the SSE caller) leaves every layer `None` — the loop
/// runs with vanilla semantics, no fallback, no auto-continue, no session
/// recovery. The cron caller calls `for_cron(...)` to populate the layers
/// that RPC-style callers need (see `handle_isolated_via_pipeline`).
///
/// Construction is cheap (clones of small structs); the runtime cost
/// inside `execute()` is one `if let Some(_)` check per layer per
/// iteration — localised to well-defined insertion points.
#[derive(Clone, Default)]
pub struct BehaviourLayers {
    pub fallback_provider: Option<FallbackPolicy>,
    pub auto_continue: Option<AutoContinuePolicy>,
    pub session_recovery: Option<SessionRecoveryPolicy>,
    pub tool_policy_override: Option<ToolPolicyOverride>,
    pub forced_final_call: Option<ForcedFinalCallPolicy>,
    pub interrupted_verify_guard: Option<InterruptedVerifyGuardPolicy>,
}

impl BehaviourLayers {
    /// All layers off. Equivalent to `default()`. Interactive paths now use
    /// [`Self::for_interactive`] (fallback + session-recovery) and cron uses
    /// [`Self::for_cron`], so this "everything off" constructor is only
    /// referenced by tests — hence `#[cfg(test)]` to keep `-D warnings` clean.
    #[cfg(test)]
    pub fn none() -> Self {
        Self::default()
    }

    /// Layers for interactive paths (SSE web chat, channel adapters, plain
    /// streaming). Enables the layers that stop a recoverable
    /// provider/transport hiccup — or an empty upstream response — from
    /// killing a live user session:
    ///
    ///   * `fallback_provider` — fail over to the configured fallback after N
    ///     consecutive LLM errors, exactly as cron does. No-op when the agent
    ///     has no `fallback_provider` configured (`create_fallback_provider`
    ///     returns `None`), so this is behaviour-preserving by default.
    ///   * `session_recovery` — rebuild the message list once on a
    ///     `SessionCorruption` error ("roles must alternate", orphan tool_use)
    ///     instead of failing the turn (and forking a fresh session).
    ///   * `auto_continue` — engaged in EMPTY-RETRY-ONLY mode
    ///     (`max_continues: 0` so the nudge path never fires): an empty LLM
    ///     response — including a normalized upstream `(Empty response: …)`
    ///     garbage blob — is retried once instead of ending the turn empty
    ///     (and, in voice mode, being read aloud).
    ///
    /// Deliberately leaves the nudge path, `forced_final_call`, and
    /// `tool_policy_override` OFF — those alter interactive UX (the user is
    /// meant to see the iteration-limit Finish and raw turn boundaries) and
    /// remain cron-only. Before this, interactive callers used
    /// `BehaviourLayers::none()`, so a single non-retryable LLM error or a
    /// primary-provider outage failed the web/channel turn that an identical
    /// cron run would have survived.
    pub fn for_interactive(
        loop_config: &crate::agent::tool_loop::ToolLoopConfig,
        user_text: String,
    ) -> Self {
        Self {
            fallback_provider: Some(FallbackPolicy::from_loop_config(loop_config)),
            // Only empty-retry (max_continues=0 disables the nudge path): an
            // empty LLM response on the web (including normalized upstream
            // garbage) is retried once, instead of ending the turn with an
            // empty message.
            auto_continue: Some(AutoContinuePolicy {
                max_continues: 0,
                retry_on_empty: true,
            }),
            session_recovery: Some(SessionRecoveryPolicy {
                original_user_text: user_text,
            }),
            tool_policy_override: None,
            forced_final_call: None,
            // A human is present on interactive turns — no auto-block needed.
            interrupted_verify_guard: None,
        }
    }

    /// Populates the layer set used by `handle_isolated_via_pipeline`
    /// (cron jobs and other RPC-style callers).
    ///
    /// `tool_policy_override` is sourced from `msg.tool_policy_override`
    /// (the optional JSON blob carried by the IncomingMessage); if the
    /// blob is absent or malformed, the layer stays `None` so behaviour
    /// matches the "no override applied" path.
    pub fn for_cron(
        loop_config: &crate::agent::tool_loop::ToolLoopConfig,
        msg: &opex_types::IncomingMessage,
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
            // Autonomous re-drive: block a blind repeat of a non-idempotent tool
            // whose prior outcome was lost to a crash (defense-in-depth).
            interrupted_verify_guard: Some(InterruptedVerifyGuardPolicy),
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
    /// Position in the profile's `text` reserve chain reached so far. Starts at
    /// 0; each failover-worthy swap resolves `text[1 + fallback_chain_idx]` then
    /// advances. When `create_fallback_provider` returns `None`, the chain is
    /// exhausted and the turn falls through to the error path. Per-turn state —
    /// the next run starts fresh at the primary.
    pub fallback_chain_idx: usize,
    pub did_reset_session: bool,
    pub auto_continue_count: u8,
    pub empty_retry_count: u8,
}

impl LayerRuntimeState {
    /// Adopt reserve provider `p` after a failover-worthy error: make it the
    /// live provider for the rest of the turn, advance the reserve-chain
    /// cursor so the *next* failover resolves the following `text[…]` entry,
    /// and reset the failure counter. Encapsulates the multi-hop swap so the
    /// `execute()` loop body stays a straight-line branch and the advance
    /// semantics are unit-testable without a live engine.
    pub(crate) fn adopt_fallback(
        &mut self,
        p: Arc<dyn crate::agent::providers::LlmProvider>,
    ) {
        self.fallback_provider = Some(p);
        self.using_fallback = true;
        self.fallback_chain_idx += 1;
        self.consecutive_failures = 0;
    }
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
        assert!(a.interrupted_verify_guard.is_none() && b.interrupted_verify_guard.is_none());
    }

    /// A1 regression: the real tool is `process` (action=start), NOT the
    /// phantom `process_start`. The interrupted-verify guard matches on the
    /// tool-call name, so it must treat `process` as non-idempotent — otherwise
    /// a crash-interrupted `process(action="start")` can double-start.
    #[test]
    fn process_tool_is_non_idempotent() {
        assert!(is_non_idempotent_tool("process"));
        assert!(is_non_idempotent_tool("code_exec"));
        assert!(is_non_idempotent_tool("workspace_rename"));
        // The old phantom name is not a real tool and must not match.
        assert!(!is_non_idempotent_tool("process_start"));
        assert!(!is_non_idempotent_tool("workspace_read"));
    }

    /// `LayerRuntimeState::default()` is the zero state every iteration
    /// loop starts from. Confirm no surprising non-zero defaults.
    #[test]
    fn runtime_state_defaults_are_zero() {
        let s = LayerRuntimeState::default();
        assert_eq!(s.consecutive_failures, 0);
        assert!(!s.using_fallback);
        assert!(s.fallback_provider.is_none());
        assert_eq!(s.fallback_chain_idx, 0);
        assert!(!s.did_reset_session);
        assert_eq!(s.auto_continue_count, 0);
        assert_eq!(s.empty_retry_count, 0);
    }

    // ── Fallback reserve-chain walk ──
    //
    // Models the multi-hop failover `execute()` performs: the profile's `text`
    // slot chain is `[primary, reserve1, reserve2]`; `chain_idx=k` resolves
    // `text[1 + k]` (the engine wrapper's indexing). Each failover-worthy error
    // adopts the next reserve via `LayerRuntimeState::adopt_fallback`, advancing
    // the cursor. Reuses the fake-provider pattern from the sibling `llm_call`
    // tests — a trivial `LlmProvider` whose only job is to carry a name — instead
    // of a live engine (which needs a DB).

    struct FakeReserve(&'static str);
    #[async_trait::async_trait]
    impl crate::agent::providers::LlmProvider for FakeReserve {
        async fn chat(
            &self,
            _m: &[opex_types::Message],
            _t: &[opex_types::ToolDefinition],
            _o: crate::agent::providers::CallOptions,
        ) -> anyhow::Result<opex_types::LlmResponse> {
            anyhow::bail!("fake reserve never called for chat")
        }
        async fn chat_stream(
            &self,
            _m: &[opex_types::Message],
            _t: &[opex_types::ToolDefinition],
            _tx: tokio::sync::mpsc::Sender<String>,
            _o: crate::agent::providers::CallOptions,
        ) -> anyhow::Result<opex_types::LlmResponse> {
            anyhow::bail!("fake reserve never called for chat_stream")
        }
        fn name(&self) -> &str { self.0 }
    }

    /// Primary + reserve #1 both fail with transport errors; the state must
    /// advance through the `text` chain to reserve #2 (two swaps), landing at
    /// `fallback_chain_idx == 2`, then report the chain exhausted at index 2.
    #[test]
    fn fallback_walks_text_chain_to_second_reserve() {
        use crate::db::profiles::{SlotEntry, Slots};

        // Profile `text` slot: index 0 is the live primary; 1 and 2 are reserves.
        let mut slots: Slots = Slots::new();
        slots.insert(
            "text".to_string(),
            vec![
                SlotEntry { provider: "primary".into(),  model: None, voice: None },
                SlotEntry { provider: "reserve1".into(), model: None, voice: None },
                SlotEntry { provider: "reserve2".into(), model: None, voice: None },
            ],
        );

        // Mirror the engine wrapper: chain_idx=k → text[1 + k].
        let resolve = |chain_idx: usize| -> Option<Arc<dyn crate::agent::providers::LlmProvider>> {
            let entry = slots.get("text")?.get(1 + chain_idx)?;
            let name: &'static str = match entry.provider.as_str() {
                "reserve1" => "reserve1",
                "reserve2" => "reserve2",
                _ => "other",
            };
            Some(Arc::new(FakeReserve(name)))
        };

        let mut st = LayerRuntimeState::default();

        // Primary (text[0]) failed → resolve reserve #1 (text[1]) and adopt.
        let r1 = resolve(st.fallback_chain_idx).expect("reserve #1 present");
        assert_eq!(r1.name(), "reserve1");
        st.adopt_fallback(r1);
        assert_eq!(st.fallback_chain_idx, 1);
        assert!(st.using_fallback);
        assert_eq!(st.consecutive_failures, 0);

        // Reserve #1 also failed → resolve reserve #2 (text[2]) and adopt.
        let r2 = resolve(st.fallback_chain_idx).expect("reserve #2 present");
        assert_eq!(r2.name(), "reserve2");
        st.adopt_fallback(r2);
        assert_eq!(st.fallback_chain_idx, 2, "advanced to reserve #2 after second swap");
        assert_eq!(st.fallback_provider.as_ref().unwrap().name(), "reserve2");

        // Reserve #2 also fails → chain exhausted (text[3] does not exist),
        // so `execute()` falls through to the error path.
        assert!(resolve(st.fallback_chain_idx).is_none(), "chain exhausted at index 2");
    }

    /// Session Resilience Task 4 (WS4) — brief step 5: chain `[P0, P1, P2]`,
    /// `P0` (primary) cooled → the turn-start self-heal check resolves `P1`
    /// first; `P1` fails failover-worthy → records `P1`'s cooldown →
    /// resolves `P2`; after `P0`'s cooldown expires, `P0` is selected again.
    /// Exercises `provider_cooldown::{ProviderCooldowns, resolve_next_uncooled}`
    /// against the same `text`-chain shape as `fallback_walks_text_chain_to_second_reserve`
    /// above, without touching a live engine or DB (pure — mirrors that test's style).
    #[test]
    fn cooled_primary_skips_to_first_uncooled_reserve_and_self_heals() {
        use crate::agent::error_classify::LlmErrorClass;
        use crate::agent::provider_cooldown::{resolve_next_uncooled, ProviderCooldowns};
        use crate::db::profiles::{SlotEntry, Slots};
        use std::time::Duration;

        let mut slots: Slots = Slots::new();
        slots.insert(
            "text".to_string(),
            vec![
                SlotEntry { provider: "P0".into(), model: None, voice: None },
                SlotEntry { provider: "P1".into(), model: None, voice: None },
                SlotEntry { provider: "P2".into(), model: None, voice: None },
            ],
        );
        let chain = slots.get("text").unwrap();
        let cooldowns = ProviderCooldowns::new();

        // Short-lived cooldown so the test doesn't sleep through the real
        // 60s RateLimit window.
        cooldowns.record_failure_for("P0", Duration::from_millis(30));
        assert!(cooldowns.is_cooled("P0"), "P0 must be cooled before the turn starts");

        // Turn-start check (execute.rs step 4b mirror): primary cooled →
        // resolve the reserve chain from chain_idx=0 → first uncooled entry
        // is P1 (nothing to skip).
        let (entry, idx) =
            resolve_next_uncooled(chain, &cooldowns, 0).expect("P1 must be available");
        assert_eq!(entry.provider, "P1", "turn resolves P1 first, not P0");
        assert_eq!(idx, 0);

        let mut st = LayerRuntimeState { fallback_chain_idx: idx, ..Default::default() };
        st.adopt_fallback(Arc::new(FakeReserve("P1")));
        assert_eq!(st.fallback_chain_idx, 1);
        assert!(st.using_fallback);

        // P1 fails failover-worthy mid-turn → cooldown recorded against P1,
        // walking the chain from fallback_chain_idx=1 must land on P2.
        cooldowns.record_failure("P1", &LlmErrorClass::RateLimit);
        assert!(cooldowns.is_cooled("P1"));
        let (entry, idx) = resolve_next_uncooled(chain, &cooldowns, st.fallback_chain_idx)
            .expect("P2 must be available");
        assert_eq!(entry.provider, "P2", "cooled P1 must be skipped in favor of P2");
        st.fallback_chain_idx = idx;
        st.adopt_fallback(Arc::new(FakeReserve("P2")));
        assert_eq!(st.fallback_provider.as_ref().unwrap().name(), "P2");

        // P0's cooldown expires → the next turn's self-heal check sees the
        // primary as usable again, with zero timers or background sweeps.
        std::thread::sleep(Duration::from_millis(60));
        assert!(!cooldowns.is_cooled("P0"), "primary must self-heal once its cooldown lapses");
    }

    /// Build a minimal `IncomingMessage` for layer-construction tests.
    /// Centralises the boilerplate so future field additions touch one
    /// place; the layer code only reads `text` and `tool_policy_override`.
    fn mk_msg(
        text: Option<&str>,
        tool_policy_override: Option<serde_json::Value>,
    ) -> opex_types::IncomingMessage {
        opex_types::IncomingMessage {
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
    /// values; mirrors what `handle_isolated_via_pipeline` enables.
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
        assert!(layers.interrupted_verify_guard.is_some(), "autonomous turns arm the verify guard");
    }

    #[test]
    fn for_interactive_leaves_verify_guard_off() {
        let loop_config = crate::agent::tool_loop::ToolLoopConfig::default();
        let layers = BehaviourLayers::for_interactive(&loop_config, "hi".to_string());
        assert!(layers.interrupted_verify_guard.is_none(), "a human is present — no auto-block");
    }

    // ── interrupted-verify guard logic ──

    use opex_types::{Message, MessageRole, ToolCall};

    fn tool_msg(content: &str) -> Message {
        Message {
            role: MessageRole::Tool,
            content: content.to_string(),
            tool_calls: None,
            tool_call_id: Some(opex_types::ids::ToolCallId::new("c1".to_string())),
            thinking_blocks: vec![],
            db_id: None,
        }
    }

    fn call(name: &str) -> ToolCall {
        ToolCall {
            id: opex_types::ids::ToolCallId::new(format!("call_{name}")),
            name: name.to_string(),
            arguments: serde_json::json!({}),
            thought_signature: None,
        }
    }

    #[test]
    fn is_non_idempotent_tool_matches_the_set() {
        for t in NON_IDEMPOTENT_TOOLS {
            assert!(is_non_idempotent_tool(t), "{t} should be non-idempotent");
        }
        for t in ["workspace_write", "workspace_read", "memory", "agent", "search_web"] {
            assert!(!is_non_idempotent_tool(t), "{t} should NOT be gated");
        }
    }

    #[test]
    fn last_tool_result_interrupted_verify_examines_only_the_last_result() {
        let marker = crate::db::sessions::INTERRUPTED_TOOL_RESULT;
        // No tool messages → false.
        assert!(!last_tool_result_is_interrupted_verify(&[]));
        // Last tool result IS the marker → true.
        assert!(last_tool_result_is_interrupted_verify(&[tool_msg(marker)]));
        // Last tool result is a normal result → false.
        assert!(!last_tool_result_is_interrupted_verify(&[tool_msg("ok, done")]));
        // Marker SUPERSEDED by a later real result → cleared → false.
        assert!(!last_tool_result_is_interrupted_verify(&[tool_msg(marker), tool_msg("verified: file absent")]));
    }

    #[test]
    fn should_block_only_when_marker_present_and_batch_non_idempotent() {
        let marker = crate::db::sessions::INTERRUPTED_TOOL_RESULT;
        // Marker present + non-idempotent tool → block.
        assert!(should_block_interrupted_batch(&[tool_msg(marker)], &[call("code_exec")]));
        // Marker present but batch is all idempotent → no block.
        assert!(!should_block_interrupted_batch(&[tool_msg(marker)], &[call("workspace_read")]));
        // No marker (normal last result) even with a non-idempotent tool → no block.
        assert!(!should_block_interrupted_batch(&[tool_msg("ok")], &[call("code_exec")]));
        // Mixed batch with at least one non-idempotent tool + marker → block.
        assert!(should_block_interrupted_batch(&[tool_msg(marker)], &[call("workspace_read"), call("workspace_delete")]));
    }

    /// When `tool_policy_override` is the wrong shape (e.g. a JSON
    /// array instead of an object), the layer falls through to None
    /// instead of failing — matches the "best-effort" handling
    /// in `handle_isolated_via_pipeline`.
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
