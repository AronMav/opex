//! Background goal-driver loop: run an autonomous turn, deliver it, judge it, decide.

use std::sync::Arc;

use opex_types::{Message, MessageRole, ToolDefinition};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::decompose::{self, ChunkVerdict, DecomposeAction, CHUNK_MAX_CHARS, MAX_CHUNKS};
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
    let mut decompose_failed = false;
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

        let is_decompose =
            row.origin == "initiative" && engine.cfg().agent.initiative.decompose && !decompose_failed;
        if is_decompose {
            // Lazy decompose on first entry.
            if row.subgoals.is_empty() {
                let chunks = clean_chunks(
                    llm_json_list(&engine, decompose::decompose_prompt(&row.goal_text), "chunks").await,
                );
                if chunks.is_empty() {
                    tracing::warn!(session = %session_id, "decompose failed/empty; falling back to flat loop");
                    decompose_failed = true;
                    continue;
                }
                let _ = crate::db::session_goals::set_subgoals(&db, session_id, &chunks).await;
                let _ = crate::db::session_goals::set_current_chunk(&db, session_id, 0).await;
                continue; // reload on next iteration
            }
            let current = row.current_chunk.max(0) as usize;
            let cur_text = row.subgoals.get(current).cloned().unwrap_or_default();
            let lock = super::pool::goal_lock(&locks, session_id);
            let text = {
                let _guard = lock.lock().await;
                if cancel.is_cancelled() {
                    break;
                }
                let prompt = decompose::chunk_continuation_prompt(&row.goal_text, &row.subgoals, current);
                match engine.run_goal_turn(session_id, &prompt, cancel.clone()).await {
                    Ok(t) => t,
                    Err(e) => {
                        tracing::warn!(session = %session_id, error = %e, "chunk turn failed; continue");
                        String::new()
                    }
                }
            };
            if cancel.is_cancelled() {
                break;
            }
            let _ = crate::db::session_goals::bump_turn(&db, session_id).await;
            if !text.trim().is_empty() {
                deliver(&engine, &target, session_id, &text).await;
            }
            let verdict = chunk_judge(&engine, &row.goal_text, &cur_text, &text).await;
            let _ = crate::db::session_goals::record_verdict(
                &db,
                session_id,
                if verdict.chunk_done { "chunk_done" } else { "continue" },
                !verdict.parse_ok,
            )
            .await;
            let fresh = crate::db::session_goals::get(&db, session_id).await.ok().flatten().unwrap_or_else(|| row.clone());
            match decompose::advance_decision(&fresh, verdict, fresh.subgoals.len()) {
                DecomposeAction::Continue => {}
                DecomposeAction::Advance => {
                    let _ = crate::db::session_goals::set_current_chunk(&db, session_id, fresh.current_chunk + 1).await;
                }
                DecomposeAction::AdvanceAndReplan => {
                    let done: Vec<String> = fresh.subgoals.iter().take(current + 1).cloned().collect();
                    let remaining: Vec<String> = fresh.subgoals.iter().skip(current + 1).cloned().collect();
                    let new_remaining = clean_chunks(
                        llm_json_list(
                            &engine,
                            decompose::replan_prompt(&fresh.goal_text, &done, &remaining, &text),
                            "remaining",
                        )
                        .await,
                    );
                    if !new_remaining.is_empty() {
                        let mut merged = done.clone();
                        merged.extend(new_remaining);
                        let _ = crate::db::session_goals::set_subgoals(&db, session_id, &merged).await;
                        tracing::info!(session = %session_id, "initiative goal replanned remaining chunks");
                    }
                    let _ = crate::db::session_goals::set_current_chunk(&db, session_id, fresh.current_chunk + 1).await;
                }
                DecomposeAction::Done => {
                    let _ = crate::db::session_goals::set_status(&db, session_id, "done").await;
                    deliver(&engine, &target, session_id, "✅ Goal complete.").await;
                    break;
                }
                DecomposeAction::Pause(reason) => {
                    let _ = crate::db::session_goals::set_status(&db, session_id, "paused").await;
                    let m = if reason == "judge" {
                        "⏸ Goal paused (judge unreliable). /goal resume to retry."
                    } else {
                        "⏸ Goal paused (turn budget). /goal resume to continue."
                    };
                    deliver(&engine, &target, session_id, m).await;
                    break;
                }
            }
            continue; // decompose branch handled this iteration
        }

        // Serialize against user turns for the duration of the autonomous turn.
        let lock = super::pool::goal_lock(&locks, session_id);
        let text = {
            let _guard = lock.lock().await;
            if cancel.is_cancelled() {
                break;
            }
            let flat_subgoals: Vec<String> = if row.origin == "initiative" && row.current_chunk > 0 {
                row.subgoals.iter().skip(row.current_chunk as usize).cloned().collect()
            } else {
                row.subgoals.clone()
            };
            let prompt = continuation_prompt(&row.goal_text, &flat_subgoals);
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

/// Sanitize + cap LLM-produced chunk strings before persistence (H1). Drops
/// injection-tripping entries; empty result signals decompose/replan failure.
fn clean_chunks(raw: Vec<String>) -> Vec<String> {
    raw.into_iter()
        .filter_map(|c| crate::agent::soul::sanitize::sanitize_soul_text(&c, CHUNK_MAX_CHARS))
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty())
        .take(MAX_CHUNKS)
        .collect()
}

/// Call the aux/compaction model with `prompt`, repair the JSON reply, and pull
/// the string array at `key`. Fail-soft: any provider/parse error → `vec![]`.
async fn llm_json_list(engine: &AgentEngine, prompt: String, key: &str) -> Vec<String> {
    let provider = engine.cfg().compaction_provider.clone().unwrap_or_else(|| engine.provider_arc());
    let Ok(raw) = crate::agent::soul::reflection::llm_text(&provider, prompt).await else {
        return vec![];
    };
    let Ok(v) = crate::agent::json_repair::repair_json(&raw) else {
        return vec![];
    };
    v.get(key)
        .and_then(|a| a.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default()
}

/// Ask the aux/compaction model whether the current chunk is done and whether
/// the remaining plan needs replanning. Fail-soft: provider/parse error → not-done, not-ok.
async fn chunk_judge(engine: &AgentEngine, goal: &str, current_chunk: &str, last: &str) -> ChunkVerdict {
    let provider = engine.cfg().compaction_provider.clone().unwrap_or_else(|| engine.provider_arc());
    match crate::agent::soul::reflection::llm_text(&provider, decompose::chunk_judge_prompt(goal, current_chunk, last))
        .await
    {
        Ok(raw) => decompose::parse_chunk_verdict(&raw),
        Err(_) => ChunkVerdict { chunk_done: false, replan: false, parse_ok: false },
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
