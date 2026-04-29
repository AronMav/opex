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
/// tool has authoritative internal timeouts (sync `message` waits up to
/// `MESSAGE_WAIT_FOR_IDLE_TIMEOUT` plus `MESSAGE_RESULT_TIMEOUT`, sync `run`
/// blocks 300s; see `pipeline::agent_tool`) and is wrapped in a strictly
/// larger 600s outer safety net (`AGENT_SAFETY_TIMEOUT`). Under normal
/// conditions the inner caps fire first; the outer wrapper exists as
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
) -> Result<Vec<(String, String)>, LoopBreak> {
    let n = tool_calls.len();
    let mut results: Vec<Option<String>> = vec![None; n];

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

        let futs: Vec<_> = parallel_indices
            .iter()
            .map(|&i| {
                let name = tool_calls[i].name.clone();
                let args = enriched[i].clone();
                async move {
                    // The `agent` tool owns authoritative internal timeouts
                    // (sync run / sync message — see `pipeline::agent_tool`).
                    // The outer wrapper here is a defense-in-depth safety net
                    // sized strictly larger than every inner cap; under normal
                    // conditions the inner timeouts fire first.
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
    }

    // 5. Final reassemble
    Ok(tool_calls
        .iter()
        .enumerate()
        .map(|(i, tc)| (tc.id.clone(), results[i].take().unwrap_or_default()))
        .collect())
}
