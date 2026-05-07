//! Shared helpers for the LLM+tools loop.
//!
//! Extracted from `pipeline::execute` so the streaming SSE pipeline has a
//! single source of truth for:
//!
//! * loop-nudge system message wording (was duplicated, drifted twice in
//!   review history)
//! * intermediate persist scaffolding (UUID + serde encoding of tool_calls
//!   and thinking_blocks for the detached `spawn_persist_assistant_message`
//!   call)
//! * the loop-broken vs nudge bookkeeping driven by `LoopDetector`

use hydeclaw_types::{Message, MessageRole, ThinkingBlock, ToolCall};

/// Build the system nudge message injected when a tool-call loop is detected.
pub fn build_loop_nudge_message(reason: Option<&str>) -> String {
    let nudge_desc = reason.unwrap_or("repeating pattern");
    format!(
        "LOOP DETECTED: You have repeated the same sequence of actions ({desc}). \
         Change your approach entirely. If the task is too large for a single session, \
         tell the user and suggest breaking it into smaller steps. Do NOT retry the same approach.",
        desc = nudge_desc
    )
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
    /// the turn.
    pub loop_broken: bool,
}

/// Apply the loop-nudge / loop-break bookkeeping for one batch outcome.
///
/// * If `outcome.loop_break` is `None` → no-op, returns `loop_broken: false`.
/// * If a break is reported and we still have nudge budget → push a system
///   message into `messages`, increment `loop_nudge_count`, return
///   `loop_broken: false`.
/// * If we're out of nudge budget → returns `loop_broken: true` so the caller
///   can stop the loop.
pub fn apply_loop_nudge(
    messages: &mut Vec<Message>,
    outcome_loop_break: &Option<Option<String>>,
    loop_nudge_count: &mut usize,
    max_loop_nudges: usize,
    detector: &mut crate::agent::tool_loop::LoopDetector,
    agent_name: &str,
) -> LoopNudgeDecision {
    let Some(reason) = outcome_loop_break else {
        return LoopNudgeDecision { loop_broken: false };
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
        LoopNudgeDecision { loop_broken: false }
    } else {
        tracing::error!(
            agent = %agent_name,
            nudge_count = *loop_nudge_count,
            "max loop nudges reached, force-stopping agent"
        );
        LoopNudgeDecision { loop_broken: true }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hydeclaw_types::ToolCall;

    fn mk_tc(id: &str, name: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            name: name.to_string(),
            arguments: serde_json::Value::Null,
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
        assert!(messages.is_empty(), "no nudge should be appended over budget");
        assert_eq!(nudge_count, 3, "counter must not advance past max");
    }
}
