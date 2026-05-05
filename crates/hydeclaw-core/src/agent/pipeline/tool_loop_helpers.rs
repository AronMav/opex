//! Shared helpers for the LLM+tools loop.
//!
//! Extracted from `pipeline::execute` and `engine::stream::handle_isolated`
//! so the streaming SSE pipeline and the synchronous RPC path share a single
//! source of truth for:
//!
//! * loop-nudge system message wording (was duplicated, drifted twice in
//!   review history)
//! * intermediate persist scaffolding (UUID + serde encoding of tool_calls
//!   and thinking_blocks for the detached `spawn_persist_assistant_message`
//!   call)
//! * appending the assistant message + tool result rows back into the
//!   in-memory `messages` vec used for LLM context
//! * the loop-broken vs nudge bookkeeping driven by `LoopDetector`
//!
//! Both paths still own their transport-specific concerns (streaming chunks,
//! cancellation token, fallback provider, auto-continue) — those remain
//! divergent intentionally because the contracts differ.

use crate::agent::pipeline::parallel::{BatchOutcome, ToolBatchResult};
use hydeclaw_types::{Message, MessageRole, ThinkingBlock, ToolCall};
use uuid::Uuid;

/// Build the system nudge message injected when a tool-call loop is detected.
///
/// Single source of truth — both `pipeline::execute` and the legacy
/// `handle_isolated` path call this. The wording was duplicated before and
/// drifted twice during review.
pub fn build_loop_nudge_message(reason: Option<&str>) -> String {
    let nudge_desc = reason.unwrap_or("repeating pattern");
    format!(
        "LOOP DETECTED: You have repeated the same sequence of actions ({desc}). \
         Change your approach entirely. If the task is too large for a single session, \
         tell the user and suggest breaking it into smaller steps. Do NOT retry the same approach.",
        desc = nudge_desc
    )
}

/// Push an intermediate assistant message (one carrying `tool_calls`) into the
/// in-memory `messages` vec used for LLM context. Returns the chars added so
/// the caller can keep its `context_chars` budget tracker in sync.
///
/// Both paths previously inlined this with subtly different field orders;
/// extracting it kills a class of "forgot to copy thinking_blocks" bugs.
pub fn append_intermediate_assistant_message(
    messages: &mut Vec<Message>,
    content: String,
    tool_calls: Vec<ToolCall>,
    thinking_blocks: Vec<ThinkingBlock>,
) -> usize {
    let added = content.chars().count();
    messages.push(Message {
        role: MessageRole::Assistant,
        content,
        tool_calls: Some(tool_calls),
        tool_call_id: None,
        thinking_blocks,
        db_id: None,
    });
    added
}

/// Append every tool result from a `BatchOutcome` to the in-memory `messages`
/// vec as `Tool` role messages. Returns the total chars added so the caller
/// can keep its `context_chars` budget tracker in sync.
pub fn append_tool_results_to_messages(
    messages: &mut Vec<Message>,
    results: &[ToolBatchResult],
) -> usize {
    let mut added = 0usize;
    for batch in results {
        added += batch.result.chars().count();
        messages.push(Message {
            role: MessageRole::Tool,
            content: batch.result.clone(),
            tool_calls: None,
            tool_call_id: Some(batch.tool_call_id.clone()),
            thinking_blocks: vec![],
            db_id: None,
        });
    }
    added
}

/// Pre-encode `tool_calls` and `thinking_blocks` into the JSON-Value pair
/// expected by `spawn_persist_assistant_message`. Returns `(tool_calls_json,
/// thinking_blocks_json)` — the second slot is `None` when the response had
/// no thinking blocks (so we don't insert an empty `[]` into the DB).
pub fn encode_intermediate_persist_payload(
    tool_calls: &[ToolCall],
    thinking_blocks: &[ThinkingBlock],
) -> (Option<serde_json::Value>, Option<serde_json::Value>) {
    let tc_json = serde_json::to_value(tool_calls).ok();
    let tb_json = if thinking_blocks.is_empty() {
        None
    } else {
        serde_json::to_value(thinking_blocks).ok()
    };
    (tc_json, tb_json)
}

/// Outcome of feeding a `BatchOutcome` through the loop-nudge bookkeeping.
pub struct LoopNudgeDecision {
    /// `true` when max nudges already injected — the caller should terminate
    /// the turn (Failed for the streaming path, force-final-call for the RPC
    /// path).
    pub loop_broken: bool,
    /// `true` when a nudge system-message was pushed onto `messages` — caller
    /// uses this as a hint for telemetry / notifications.
    pub nudge_appended: bool,
}

/// Apply the loop-nudge / loop-break bookkeeping for one batch outcome.
///
/// * If `outcome.loop_break` is `None` → no-op, `(false, false)`.
/// * If a break is reported and we still have nudge budget → push a system
///   message into `messages`, increment `loop_nudge_count`, return
///   `(false, true)`.
/// * If we're out of nudge budget → return `(true, false)` so the caller can
///   stop the loop.
///
/// The detector reset historically lived only on the legacy path; we keep
/// that behavior here so both paths behave identically.
pub fn apply_loop_nudge(
    messages: &mut Vec<Message>,
    outcome_loop_break: &Option<Option<String>>,
    loop_nudge_count: &mut usize,
    max_loop_nudges: usize,
    detector: &mut crate::agent::tool_loop::LoopDetector,
    agent_name: &str,
) -> LoopNudgeDecision {
    let Some(reason) = outcome_loop_break else {
        return LoopNudgeDecision {
            loop_broken: false,
            nudge_appended: false,
        };
    };

    if *loop_nudge_count < max_loop_nudges {
        messages.push(Message {
            role: MessageRole::System,
            content: build_loop_nudge_message(reason.as_deref()),
            tool_calls: None,
            tool_call_id: None,
            thinking_blocks: vec![],
            db_id: None,
        });
        *loop_nudge_count += 1;
        detector.reset();
        tracing::warn!(
            agent = %agent_name,
            nudge_count = *loop_nudge_count,
            reason = ?reason,
            "loop nudge injected"
        );
        LoopNudgeDecision {
            loop_broken: false,
            nudge_appended: true,
        }
    } else {
        tracing::error!(
            agent = %agent_name,
            nudge_count = *loop_nudge_count,
            "max loop nudges reached, force-stopping agent"
        );
        LoopNudgeDecision {
            loop_broken: true,
            nudge_appended: false,
        }
    }
}

/// Drain a `BatchOutcome` into the in-memory message vec and apply loop-nudge
/// bookkeeping in one shot. Mirrors what both paths did inline before.
///
/// Returns `(chars_added, decision)`. Caller uses `chars_added` to advance
/// its `context_chars` tracker.
pub fn finalize_tool_batch(
    messages: &mut Vec<Message>,
    outcome: BatchOutcome,
    loop_nudge_count: &mut usize,
    max_loop_nudges: usize,
    detector: &mut crate::agent::tool_loop::LoopDetector,
    agent_name: &str,
) -> (usize, LoopNudgeDecision) {
    let chars_added = append_tool_results_to_messages(messages, &outcome.results);
    let decision = apply_loop_nudge(
        messages,
        &outcome.loop_break,
        loop_nudge_count,
        max_loop_nudges,
        detector,
        agent_name,
    );
    (chars_added, decision)
}

/// Mark this iteration's pre-allocated UUID as the one used for both the
/// SSE `step-start` event and the eventual DB row insert. Returns it.
///
/// Trivial wrapper on `Uuid::new_v4()` — the value of factoring it out is
/// the single docstring location for "why per-iteration UUID matters" so
/// future maintainers don't reinvent the heuristic-based dedup we deleted.
pub fn allocate_iteration_message_id() -> Uuid {
    Uuid::new_v4()
}

#[cfg(test)]
mod tests {
    use super::*;
    use hydeclaw_types::ToolCall;

    fn mk_tc(id: &str, name: &str) -> ToolCall {
        ToolCall {
            id: id.to_string(),
            name: name.to_string(),
            arguments: serde_json::Value::Null,
        }
    }

    fn mk_result(id: &str, body: &str) -> ToolBatchResult {
        ToolBatchResult {
            tool_call_id: id.to_string(),
            result: body.to_string(),
            tool_msg_id: None,
        }
    }

    #[test]
    fn loop_nudge_uses_reason_when_provided() {
        let msg = build_loop_nudge_message(Some("calling the same tool repeatedly"));
        assert!(msg.contains("calling the same tool repeatedly"));
        assert!(msg.contains("LOOP DETECTED"));
    }

    #[test]
    fn loop_nudge_falls_back_to_default_reason() {
        let msg = build_loop_nudge_message(None);
        assert!(msg.contains("repeating pattern"));
        assert!(msg.contains("LOOP DETECTED"));
    }

    #[test]
    fn append_intermediate_returns_chars_count() {
        let mut messages: Vec<Message> = vec![];
        let added = append_intermediate_assistant_message(
            &mut messages,
            "hello".to_string(),
            vec![mk_tc("tc1", "name")],
            vec![],
        );
        assert_eq!(added, 5);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, MessageRole::Assistant);
        assert!(messages[0].tool_calls.is_some());
    }

    #[test]
    fn append_tool_results_pushes_one_per_result() {
        let mut messages: Vec<Message> = vec![];
        let results = vec![mk_result("t1", "r1"), mk_result("t2", "r2-longer")];
        let added = append_tool_results_to_messages(&mut messages, &results);
        assert_eq!(added, 2 + 9);
        assert_eq!(messages.len(), 2);
        for m in &messages {
            assert_eq!(m.role, MessageRole::Tool);
        }
        assert_eq!(messages[0].tool_call_id.as_deref(), Some("t1"));
        assert_eq!(messages[1].tool_call_id.as_deref(), Some("t2"));
    }

    #[test]
    fn encode_persist_payload_skips_empty_thinking() {
        let (tc_json, tb_json) =
            encode_intermediate_persist_payload(&[mk_tc("a", "b")], &[]);
        assert!(tc_json.is_some());
        assert!(tb_json.is_none(), "empty thinking_blocks should encode to None");
    }

    #[test]
    fn apply_loop_nudge_no_break_is_noop() {
        let mut messages: Vec<Message> = vec![];
        let mut nudge_count = 0usize;
        let cfg = crate::agent::tool_loop::ToolLoopConfig::default();
        let mut detector = crate::agent::tool_loop::LoopDetector::new(&cfg);
        let decision = apply_loop_nudge(
            &mut messages,
            &None,
            &mut nudge_count,
            3,
            &mut detector,
            "TestAgent",
        );
        assert!(!decision.loop_broken);
        assert!(!decision.nudge_appended);
        assert!(messages.is_empty());
        assert_eq!(nudge_count, 0);
    }

    #[test]
    fn apply_loop_nudge_within_budget_pushes_system_message() {
        let mut messages: Vec<Message> = vec![];
        let mut nudge_count = 0usize;
        let cfg = crate::agent::tool_loop::ToolLoopConfig::default();
        let mut detector = crate::agent::tool_loop::LoopDetector::new(&cfg);
        let decision = apply_loop_nudge(
            &mut messages,
            &Some(Some("repeated_pattern".to_string())),
            &mut nudge_count,
            3,
            &mut detector,
            "TestAgent",
        );
        assert!(!decision.loop_broken);
        assert!(decision.nudge_appended);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, MessageRole::System);
        assert!(messages[0].content.contains("LOOP DETECTED"));
        assert!(messages[0].content.contains("repeated_pattern"));
        assert_eq!(nudge_count, 1);
    }

    #[test]
    fn apply_loop_nudge_over_budget_signals_loop_broken() {
        let mut messages: Vec<Message> = vec![];
        let mut nudge_count = 3usize; // already at max
        let cfg = crate::agent::tool_loop::ToolLoopConfig::default();
        let mut detector = crate::agent::tool_loop::LoopDetector::new(&cfg);
        let decision = apply_loop_nudge(
            &mut messages,
            &Some(None),
            &mut nudge_count,
            3,
            &mut detector,
            "TestAgent",
        );
        assert!(decision.loop_broken);
        assert!(!decision.nudge_appended);
        assert!(messages.is_empty(), "no nudge should be appended over budget");
        assert_eq!(nudge_count, 3, "counter must not advance past max");
    }

    #[test]
    fn finalize_tool_batch_drains_results_and_applies_nudge() {
        let mut messages: Vec<Message> = vec![];
        let mut nudge_count = 0usize;
        let cfg = crate::agent::tool_loop::ToolLoopConfig::default();
        let mut detector = crate::agent::tool_loop::LoopDetector::new(&cfg);
        let outcome = BatchOutcome {
            results: vec![mk_result("t1", "ok")],
            loop_break: Some(Some("dup".to_string())),
        };
        let (chars_added, decision) = finalize_tool_batch(
            &mut messages,
            outcome,
            &mut nudge_count,
            3,
            &mut detector,
            "TestAgent",
        );
        assert_eq!(chars_added, 2);
        assert!(decision.nudge_appended);
        assert!(!decision.loop_broken);
        // Tool result + nudge system message
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, MessageRole::Tool);
        assert_eq!(messages[1].role, MessageRole::System);
    }
}
