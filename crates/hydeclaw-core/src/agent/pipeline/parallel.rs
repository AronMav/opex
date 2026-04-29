//! Pipeline step: parallel — parallel tool execution with WAL.
//!
//! Extracted from `engine_parallel.rs`. All logic lives in free functions;
//! `AgentEngine` methods delegate here.

use crate::agent::tool_loop::{LoopDetector, LoopStatus};
use crate::memory::EmbeddingService;
use crate::tools::semantic_cache::SemanticCache;
use crate::tools::yaml_tools::YamlToolDef;
use hydeclaw_types::ToolCall;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

// ── Public types ─────────────────────────────────────────────────────────────

/// Returned when the loop detector triggers a break mid-batch.
pub struct LoopBreak(pub Option<String>);

/// One persisted tool result.
///
/// `tool_msg_id` is the row id assigned to the tool message. It is generated
/// upfront (before the detached `tokio::spawn` insert fires) so callers can
/// thread `parent_message_id` through the chain without waiting on the
/// detached insert.
pub struct ToolBatchResult {
    pub tool_call_id: String,
    pub result: String,
    pub tool_msg_id: Option<Uuid>,
}

/// Context required for the durable per-tool persistence path inside
/// `execute_tool_calls_partitioned`.
///
/// When `Some(_)` is supplied, each tool result is persisted to the
/// `messages` table immediately after its `tool_end` WAL entry, via a
/// detached `tokio::spawn` so the insert survives parent-task cancellation
/// (e.g. SSE client disconnect → engine task abort). Each tool's row id is
/// pre-generated synchronously and threaded into `parent_message_id` so the
/// chain is deterministic regardless of detached insert ordering.
///
/// When `None`, no DB save is performed and `ToolBatchResult::tool_msg_id`
/// is `None` for every result. Used by transport-less call sites
/// (openai_compat — nil session_id; subagent_runner — no DB writes).
pub struct ToolPersistCtx<'a> {
    pub agent_name: &'a str,
    /// Initial parent — typically the id of the assistant message that
    /// emitted the tool calls.
    pub initial_parent: Option<Uuid>,
}

/// Trait abstracting single-tool execution so the free function doesn't depend
/// on `AgentEngine` directly.
pub trait ToolExecutor: Send + Sync {
    fn execute_tool_call<'a>(
        &'a self,
        name: &'a str,
        arguments: &'a Value,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = String> + Send + 'a>>;

    fn needs_approval(&self, tool_name: &str) -> bool;

    /// Defense-in-depth safety-net duration applied to the `agent` tool's
    /// outer wrapper. Reads from the live `AppConfig.agent_tool.safety_timeout_secs`.
    /// Default implementation returns the hardcoded 600s used before this
    /// became configurable, so existing test stubs continue to compile.
    fn agent_safety_timeout(&self) -> Duration {
        Duration::from_secs(600)
    }
}

// ── Helper predicates ────────────────────────────────────────────────────────

fn is_system_tool_parallel_safe(name: &str) -> bool {
    matches!(
        name,
        "web_fetch"
            | "memory"
            | "workspace_read"
            | "workspace_list"
            | "tool_list"
            | "skill"
            | "session"
            | "canvas"
            | "rich_card"
            | "agent"
    )
}

fn is_tool_cacheable(name: &str) -> bool {
    matches!(
        name,
        "searxng_search" | "brave_search" | "browser_render" | "web_search"
    )
}

// ── Arg enrichment ───────────────────────────────────────────────────────────

/// Enrich tool arguments with `_context` (message context + `session_id`).
/// Uses `insert` (not `or_insert`) intentionally — LLM must not be able to
/// forge `_context` (e.g., spoofing `chat_id` for channel actions).
pub fn enrich_tool_args(
    args: &Value,
    context: &Value,
    session_id: Uuid,
    channel: &str,
) -> Value {
    let mut args = args.clone();
    if let Some(obj) = args.as_object_mut() {
        let mut ctx = if context.is_null() {
            serde_json::json!({})
        } else {
            context.clone()
        };
        if let Some(ctx_obj) = ctx.as_object_mut() {
            ctx_obj.insert(
                "session_id".to_string(),
                serde_json::json!(session_id.to_string()),
            );
            ctx_obj.insert("_channel".to_string(), serde_json::json!(channel));
        }
        obj.insert("_context".to_string(), ctx);
    }
    args
}

// ── Main execution function ──────────────────────────────────────────────────

/// Execute a batch of tool calls, partitioning into parallel and sequential
/// groups. Returns `(tool_call_id, result)` pairs in the original order.
///
/// # Parameters
/// - `executor`: implements [`ToolExecutor`] (typically `&AgentEngine`)
/// - `yaml_tools`: pre-loaded YAML tool definitions
/// - `model`: model name (for `truncate_tool_result`)
///
/// # Timeouts
/// Non-`agent` tool calls are wrapped in a 120s outer timeout. The `agent`
/// tool has authoritative internal timeouts (`ask` waits up to
/// `message_wait_for_idle_secs` for idle plus `message_result_secs` for the
/// result; see `pipeline::agent_tool`) and is wrapped in a strictly larger
/// outer safety net read from `agent_safety_timeout()` (default 600s). Under
/// normal conditions the inner caps fire first; the outer wrapper exists as
/// defense-in-depth so that a future sync action which bypasses the
/// deadline-enforced waits cannot hang the engine indefinitely.
#[allow(clippy::too_many_arguments)]
pub async fn execute_tool_calls_partitioned(
    tool_calls: &[ToolCall],
    context: &Value,
    session_id: Uuid,
    channel: &str,
    model: &str,
    current_context_chars: usize,
    detector: &mut LoopDetector,
    detect_loops: bool,
    db: &sqlx::PgPool,
    embedder: &Arc<dyn EmbeddingService>,
    yaml_tools: &HashMap<String, YamlToolDef>,
    executor: &(dyn ToolExecutor + '_),
    persist_ctx: Option<&ToolPersistCtx<'_>>,
) -> Result<Vec<ToolBatchResult>, LoopBreak> {
    let n = tool_calls.len();
    let mut results: Vec<Option<String>> = vec![None; n];
    // Pre-generated row ids for each tool's persisted message — assigned only
    // when persist_ctx is Some(_). Threaded into parent_message_id so the
    // chain stays deterministic even though inserts run in detached spawns.
    let mut persisted_ids: Vec<Option<Uuid>> = vec![None; n];
    // Walking parent — starts at initial_parent and advances to the last
    // tool's pre-generated id as we visit them in original order. Used as
    // parent for the next persisted tool message.
    let mut chain_parent: Option<Uuid> = persist_ctx.and_then(|p| p.initial_parent);

    // 1. Enrich args
    let enriched: Vec<Value> = tool_calls
        .iter()
        .map(|tc| enrich_tool_args(&tc.arguments, context, session_id, channel))
        .collect();

    // 2. Semantic cache check
    for (i, tc) in tool_calls.iter().enumerate() {
        if is_tool_cacheable(&tc.name) && embedder.is_available() {
            let query_text = tc
                .arguments
                .get("query")
                .or_else(|| tc.arguments.get("url"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if !query_text.is_empty()
                && let Ok(Some(cached_res)) =
                    SemanticCache::check(db, embedder, &tc.name, query_text, 0.95).await
            {
                tracing::info!(tool = %tc.name, query = %query_text, "semantic cache hit");
                results[i] = Some(cached_res);
            }
        }
    }

    // 3. Partition (only those NOT found in cache)
    let mut parallel_indices = Vec::new();
    let mut sequential_indices = Vec::new();
    for (i, tc) in tool_calls.iter().enumerate() {
        if results[i].is_some() {
            continue;
        }
        let is_parallel = if is_system_tool_parallel_safe(&tc.name) {
            true
        } else if executor.needs_approval(&tc.name) {
            false
        } else if let Some(tool) = yaml_tools.get(&tc.name) {
            tool.parallel && tool.channel_action.is_none()
        } else {
            false
        };
        if is_parallel {
            parallel_indices.push(i);
        } else {
            sequential_indices.push(i);
        }
    }

    // 4. Execute
    let default_timeout = Duration::from_secs(120);
    let agent_safety_timeout = executor.agent_safety_timeout();

    let start_payload = |tc: &ToolCall| -> Value {
        serde_json::json!({
            "tool_call_id": tc.id,
            "tool_name": tc.name,
            "args_hash": format!("{:x}", LoopDetector::hash_call_raw(&tc.name, &tc.arguments))
        })
    };
    let end_payload = |tc: &ToolCall, res: &str| -> Value {
        let success =
            !res.to_lowercase().contains("error") && !res.to_lowercase().contains("failed");
        serde_json::json!({
            "tool_call_id": tc.id,
            "tool_name": tc.name,
            "success": success
        })
    };

    // 4a. Parallel batch
    if !parallel_indices.is_empty() {
        for &i in &parallel_indices {
            let _ = crate::db::session_wal::log_event(
                db,
                session_id,
                "tool_start",
                Some(&start_payload(&tool_calls[i])),
            )
            .await;
        }

        // Pre-allocate row ids and parent links in ORIGINAL index order so the
        // chain is deterministic regardless of completion order. We can then
        // spawn the persist insert immediately when each tool finishes — no
        // second post-join_all loop, which previously left a micro-window where
        // `tool_end` was logged but the persist hadn't been spawned yet.
        let parallel_persist_meta: Vec<Option<(Uuid, Option<Uuid>)>> = if persist_ctx.is_some() {
            let mut out: Vec<Option<(Uuid, Option<Uuid>)>> = vec![None; n];
            for &i in &parallel_indices {
                let new_id = Uuid::new_v4();
                let parent_for_this = chain_parent;
                out[i] = Some((new_id, parent_for_this));
                persisted_ids[i] = Some(new_id);
                chain_parent = Some(new_id);
            }
            out
        } else {
            Vec::new()
        };

        let futs: Vec<_> = parallel_indices
            .iter()
            .map(|&i| {
                let name = tool_calls[i].name.clone();
                let args = enriched[i].clone();
                async move {
                    // The `agent` tool owns authoritative internal timeouts
                    // (ask = wait_for_idle + wait_for_result; see
                    // `pipeline::agent_tool`). The outer wrapper here is a
                    // defense-in-depth safety net sized strictly larger than
                    // every inner cap; under normal conditions the inner
                    // timeouts fire first.
                    let timeout = if name == "agent" {
                        agent_safety_timeout
                    } else {
                        default_timeout
                    };
                    let result = match tokio::time::timeout(
                        timeout,
                        executor.execute_tool_call(&name, &args),
                    )
                    .await
                    {
                        Ok(r) => r,
                        Err(_) => format!(
                            "Tool '{}' timed out after {}s",
                            name,
                            timeout.as_secs()
                        ),
                    };
                    (
                        i,
                        super::context::truncate_tool_result(
                            model,
                            &result,
                            current_context_chars,
                        ),
                    )
                }
            })
            .collect();

        for (i, result) in futures_util::future::join_all(futs).await {
            if detect_loops {
                if let LoopStatus::Break(reason) =
                    detector.check_limits(&tool_calls[i].name, &tool_calls[i].arguments)
                {
                    tracing::error!(tool = %tool_calls[i].name, reason = %reason, "tool loop broken (parallel post-check)");
                    return Err(LoopBreak(Some(reason)));
                }
                let success = !result.starts_with("Error:")
                    && !result.starts_with("tool error:")
                    && !result.contains("timed out");
                detector.record_execution(
                    &tool_calls[i].name,
                    &tool_calls[i].arguments,
                    success,
                );
            }

            // Store in semantic cache if successful
            if is_tool_cacheable(&tool_calls[i].name)
                && !result.starts_with("Error:")
                && !result.starts_with("tool error:")
            {
                let query_text = tool_calls[i]
                    .arguments
                    .get("query")
                    .or_else(|| tool_calls[i].arguments.get("url"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if !query_text.is_empty() {
                    let _ = SemanticCache::store(
                        db,
                        embedder,
                        &tool_calls[i].name,
                        query_text,
                        &result,
                        3600,
                    )
                    .await;
                }
            }

            results[i] = Some(result.clone());
            let _ = crate::db::session_wal::log_event(
                db,
                session_id,
                "tool_end",
                Some(&end_payload(&tool_calls[i], &result)),
            )
            .await;

            // Durable persist for THIS tool — spawned immediately after its
            // `tool_end` WAL so we don't leave a window where the WAL says
            // "ended" but the row isn't queued for insert. Detached so the
            // insert survives parent-task cancellation between here and
            // `execute()` returning. Row id and parent link were pre-allocated
            // in ORIGINAL index order above, so the chain is deterministic
            // regardless of `join_all` completion order.
            if let Some(pctx) = persist_ctx
                && let Some((new_id, parent_for_this)) = parallel_persist_meta[i]
            {
                spawn_persist_tool_message(
                    db,
                    new_id,
                    session_id,
                    pctx.agent_name,
                    &tool_calls[i].id,
                    &result,
                    parent_for_this,
                );
            }
        }
    }

    // 4b. Sequential
    for &i in &sequential_indices {
        if detect_loops
            && let LoopStatus::Break(reason) =
                detector.check_limits(&tool_calls[i].name, &tool_calls[i].arguments)
        {
            tracing::error!(tool = %tool_calls[i].name, reason = %reason, "tool loop broken (pre-check)");
            return Err(LoopBreak(Some(reason)));
        }
        let _ = crate::db::session_wal::log_event(
            db,
            session_id,
            "tool_start",
            Some(&start_payload(&tool_calls[i])),
        )
        .await;
        // See note on the parallel branch: the `agent` tool owns its own
        // longer sync timeouts. The outer wrapper here is a defense-in-depth
        // safety net — strictly larger than every inner cap so the inner
        // timeouts fire first under normal conditions.
        let timeout = if tool_calls[i].name == "agent" {
            agent_safety_timeout
        } else {
            default_timeout
        };
        let raw = match tokio::time::timeout(
            timeout,
            executor.execute_tool_call(&tool_calls[i].name, &enriched[i]),
        )
        .await
        {
            Ok(r) => r,
            Err(_) => format!(
                "Tool '{}' timed out after {}s",
                tool_calls[i].name,
                timeout.as_secs()
            ),
        };
        let res = super::context::truncate_tool_result(model, &raw, current_context_chars);
        if detect_loops {
            let success = !res.starts_with("Error:")
                && !res.starts_with("tool error:")
                && !res.contains("timed out");
            detector.record_execution(&tool_calls[i].name, &tool_calls[i].arguments, success);
        }

        // Store in semantic cache if successful
        if is_tool_cacheable(&tool_calls[i].name)
            && !res.starts_with("Error:")
            && !res.starts_with("tool error:")
        {
            let query_text = tool_calls[i]
                .arguments
                .get("query")
                .or_else(|| tool_calls[i].arguments.get("url"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if !query_text.is_empty() {
                let _ = SemanticCache::store(
                    db,
                    embedder,
                    &tool_calls[i].name,
                    query_text,
                    &res,
                    3600,
                )
                .await;
            }
        }

        results[i] = Some(res.clone());
        let _ = crate::db::session_wal::log_event(
            db,
            session_id,
            "tool_end",
            Some(&end_payload(&tool_calls[i], &res)),
        )
        .await;

        // Durable persist for this sequential tool. Detached so it survives
        // parent-task cancellation between here and `execute()` returning.
        if let Some(pctx) = persist_ctx {
            let new_id = Uuid::new_v4();
            persisted_ids[i] = Some(new_id);
            spawn_persist_tool_message(
                db,
                new_id,
                session_id,
                pctx.agent_name,
                &tool_calls[i].id,
                &res,
                chain_parent,
            );
            chain_parent = Some(new_id);
        }
    }

    // 5. Final reassemble
    Ok(tool_calls
        .iter()
        .enumerate()
        .map(|(i, tc)| ToolBatchResult {
            tool_call_id: tc.id.clone(),
            result: results[i].take().unwrap_or_default(),
            tool_msg_id: persisted_ids[i],
        })
        .collect())
}

// ── Detached persistence helper ──────────────────────────────────────────────

/// Spawn a fire-and-forget tokio task that persists a single tool result row
/// to the `messages` table. Detached so it survives parent-task cancellation
/// (e.g. SSE client disconnect → engine task abort).
///
/// The id is supplied by the caller (pre-generated synchronously) so the
/// `parent_message_id` chain is deterministic regardless of insert ordering.
/// Idempotent against retry: `save_message_ex_with_id` uses
/// `ON CONFLICT (id) DO NOTHING`.
///
/// NOTE: Uses bare `tokio::spawn` rather than `bg_tasks.spawn(...)` because
/// `parallel.rs` is reachable from call sites without a `TaskTracker`
/// (`openai_compat`, `subagent_runner`). The tradeoff: persist tasks aren't
/// awaited by graceful-shutdown drain. Acceptable because the insert is short
/// (single SQL `INSERT ... ON CONFLICT DO NOTHING`) and worst-case loss on
/// SIGTERM mid-flight is one tool row, which the user can reproduce by
/// re-asking. Threading `bg_tasks` through every persist-aware call site is a
/// larger refactor and not the scope of this gap-fix.
#[allow(clippy::too_many_arguments)]
fn spawn_persist_tool_message(
    db: &sqlx::PgPool,
    id: Uuid,
    session_id: Uuid,
    agent_name: &str,
    tool_call_id: &str,
    content: &str,
    parent_id: Option<Uuid>,
) {
    let db = db.clone();
    let agent_name = agent_name.to_string();
    let tool_call_id = tool_call_id.to_string();
    let content = content.to_string();
    tokio::spawn(async move {
        if let Err(e) = crate::db::sessions::save_message_ex_with_id(
            &db,
            id,
            session_id,
            "tool",
            &content,
            None,
            Some(&tool_call_id),
            Some(&agent_name),
            None,
            parent_id,
        )
        .await
        {
            tracing::warn!(
                error = %e,
                session_id = %session_id,
                tool_call_id = %tool_call_id,
                msg_id = %id,
                "failed to persist tool message (detached)"
            );
        }
    });
}

/// Spawn a fire-and-forget tokio task that persists an INTERMEDIATE assistant
/// message (the one that holds `tool_calls`) to the `messages` table.
///
/// Mirrors [`spawn_persist_tool_message`] for the assistant-with-tool-calls
/// case in `pipeline::execute`. The synchronous-await variant
/// (`SessionManager::save_message_ex(...).await`) leaves a cancellation gap:
/// if the engine task is aborted during the await (e.g. SSE client disconnect),
/// the row is never written and subsequent tool messages have no parent
/// assistant — the chain is broken on reload.
///
/// Detached spawn closes that gap. The id is supplied by the caller
/// (pre-generated synchronously) so the `parent_message_id` chain is
/// deterministic regardless of insert ordering. Idempotent: `ON CONFLICT (id)
/// DO NOTHING`.
///
/// See note in [`spawn_persist_tool_message`] re: bare `tokio::spawn` vs.
/// `TaskTracker`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn_persist_assistant_message(
    db: &sqlx::PgPool,
    id: Uuid,
    session_id: Uuid,
    agent_name: &str,
    content: &str,
    tool_calls_json: Option<&serde_json::Value>,
    thinking_blocks_json: Option<&serde_json::Value>,
    parent_id: Option<Uuid>,
) {
    let db = db.clone();
    let agent_name = agent_name.to_string();
    let content = content.to_string();
    let tool_calls_owned = tool_calls_json.cloned();
    let thinking_owned = thinking_blocks_json.cloned();
    tokio::spawn(async move {
        if let Err(e) = crate::db::sessions::save_message_ex_with_id(
            &db,
            id,
            session_id,
            "assistant",
            &content,
            tool_calls_owned.as_ref(),
            None,
            Some(&agent_name),
            thinking_owned.as_ref(),
            parent_id,
        )
        .await
        {
            tracing::warn!(
                error = %e,
                session_id = %session_id,
                msg_id = %id,
                "failed to persist intermediate assistant message (detached)"
            );
        }
    });
}
