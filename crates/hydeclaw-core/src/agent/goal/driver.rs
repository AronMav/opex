//! Background goal-driver loop: run an autonomous turn, deliver it, judge it, decide.

use std::sync::Arc;

use hydeclaw_types::{Message, MessageRole, ToolDefinition};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::pool::{GoalDriverHandle, GoalTarget};
use super::{continuation_prompt, next_action, parse_judge_verdict, DriverAction, Verdict};
use crate::agent::engine::AgentEngine;

/// Spawn the per-session goal driver (mirror of `session_agent_pool::spawn_live_agent`).
pub fn spawn_goal_driver(engine: Arc<AgentEngine>, session_id: Uuid, target: GoalTarget) -> GoalDriverHandle {
    let cancel = CancellationToken::new();
    let join = tokio::spawn(run_goal_driver(engine, session_id, target, cancel.clone()));
    GoalDriverHandle { cancel, join }
}

async fn run_goal_driver(engine: Arc<AgentEngine>, session_id: Uuid, target: GoalTarget, cancel: CancellationToken) {
    let db = engine.cfg().db.clone();
    let Some(locks) = engine.cfg().goal_locks.clone() else {
        return;
    };
    loop {
        if cancel.is_cancelled() {
            break;
        }
        let Ok(Some(row)) = crate::db::session_goals::get(&db, session_id).await else {
            break;
        };
        if !row.is_running() {
            break;
        }
        if !row.budget_left() {
            let _ = crate::db::session_goals::set_status(&db, session_id, "paused").await;
            deliver(&engine, &target, session_id,
                &format!("⏸ Goal hit the turn budget ({}). /goal resume to continue.", row.max_turns)).await;
            break;
        }

        // Serialize against user turns for the duration of the autonomous turn.
        let lock = super::pool::goal_lock(&locks, session_id);
        let text = {
            let _guard = lock.lock().await;
            if cancel.is_cancelled() {
                break;
            }
            let prompt = continuation_prompt(&row.goal_text, &row.subgoals);
            // Pass the driver's cancel token so `/goal stop` breaks a long
            // in-flight turn cooperatively (execute() observes it) instead of
            // the turn being hard-aborted by pool::stop and guard-dropped.
            match engine.run_goal_turn(session_id, &prompt, cancel.clone()).await {
                Ok(t) => t,
                Err(e) => {
                    tracing::warn!(session = %session_id, error = %e, "goal turn failed; fail-open continue");
                    String::new()
                }
            }
        };
        // After the turn, stop promptly if a cancel arrived during it — avoids
        // a wasted judge/deliver cycle on a goal the user just stopped.
        if cancel.is_cancelled() {
            break;
        }
        let _ = crate::db::session_goals::bump_turn(&db, session_id).await;
        if !text.trim().is_empty() {
            deliver(&engine, &target, session_id, &text).await;
        }

        let verdict = judge(&engine, &row.goal_text, &row.subgoals, &text).await;
        let fresh = crate::db::session_goals::get(&db, session_id).await.ok().flatten().unwrap_or_else(|| row.clone());
        let parse_failed = verdict == Verdict::ParseFail;
        let _ = crate::db::session_goals::record_verdict(
            &db,
            session_id,
            if verdict == Verdict::Done { "done" } else { "continue" },
            parse_failed,
        )
        .await;

        match next_action(&fresh, verdict) {
            DriverAction::Done => {
                let _ = crate::db::session_goals::set_status(&db, session_id, "done").await;
                deliver(&engine, &target, session_id, "✅ Goal complete.").await;
                break;
            }
            DriverAction::Pause(reason) => {
                let _ = crate::db::session_goals::set_status(&db, session_id, "paused").await;
                let m = if reason == "judge" {
                    "⏸ Goal paused (judge unreliable). /goal resume to retry."
                } else {
                    "⏸ Goal paused (turn budget). /goal resume to continue."
                };
                deliver(&engine, &target, session_id, m).await;
                break;
            }
            DriverAction::Continue => {}
        }
    }
    if let Some(pool) = engine.cfg().goal_pool.clone() {
        pool.remove(&session_id);
    }
}

/// Ask the aux/compaction model (fallback: main provider) whether the goal is done.
/// Fail-open: any provider error → `Continue`.
async fn judge(engine: &AgentEngine, goal: &str, subgoals: &[String], last: &str) -> Verdict {
    let provider = engine.cfg().compaction_provider.clone().unwrap_or_else(|| engine.provider_arc());
    let subgoal_block = if subgoals.is_empty() {
        String::new()
    } else {
        let lines: Vec<String> = subgoals.iter().enumerate().map(|(i, s)| format!("{}. {s}", i + 1)).collect();
        format!("\nRanked criteria:\n{}", lines.join("\n"))
    };
    let last_slice: String = last.chars().take(4000).collect();
    let prompt = format!(
        "You are a strict judge deciding whether an autonomous agent has FULLY achieved its goal.\n\n\
         Goal: {goal}{subgoal_block}\n\n\
         The agent's latest reply:\n{last_slice}\n\n\
         Respond with ONE line of JSON: {{\"done\": <true|false>, \"reason\": \"<one sentence>\"}}. \
         Require concrete evidence; if unsure, return done=false."
    );
    let messages = vec![Message {
        role: MessageRole::User,
        content: prompt,
        tool_calls: None,
        tool_call_id: None,
        thinking_blocks: vec![],
        db_id: None,
    }];
    let empty: Vec<ToolDefinition> = vec![];
    match provider.chat(&messages, &empty, crate::agent::providers::CallOptions::default()).await {
        Ok(resp) => parse_judge_verdict(&resp.content),
        Err(e) => {
            tracing::warn!(error = %e, "goal judge call failed; fail-open continue");
            Verdict::Continue
        }
    }
}

/// Deliver a turn's text: channel session → `send_message` via the router; web → ui_event.
async fn deliver(engine: &AgentEngine, target: &GoalTarget, session_id: Uuid, text: &str) {
    match target {
        Some((channel, chat_id)) => {
            let ctx = crate::agent::pipeline::CommandContext {
                cfg: engine.cfg(),
                state: engine.state(),
                tex: engine.tex(),
                subagent_depth: 0,
            };
            if let Err(e) = crate::agent::pipeline::channel_actions::send_channel_message(&ctx, channel, *chat_id, text).await {
                tracing::warn!(session = %session_id, error = %e, "goal delivery to channel failed");
            }
        }
        None => {
            if let Some(tx) = engine.state().ui_event_tx.as_ref() {
                let ev = serde_json::json!({ "type": "goal-turn", "sessionId": session_id.to_string() }).to_string();
                let _ = tx.send(ev);
            }
        }
    }
}
