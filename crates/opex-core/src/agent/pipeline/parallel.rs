//! Pipeline step: parallel — parallel tool execution with timeline.
//!
//! Extracted from `engine_parallel.rs`. All logic lives in free functions;
//! `AgentEngine` methods delegate here.

use crate::agent::tool_loop::{LoopDetector, LoopStatus};
use crate::memory::EmbeddingService;
use crate::tools::semantic_cache::SemanticCache;
use crate::tools::yaml_tools::YamlToolDef;
use opex_types::ToolCall;
use opex_types::ids::ParallelBatchId;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

// ── Public types ─────────────────────────────────────────────────────────────

/// Outcome of a tool batch — always carries every completed tool's result so
/// the caller can emit `ToolResult` SSE events for them, even if the loop
/// detector stopped further iterations afterwards. Without this, a parallel
/// batch that completed all its tools could still leave the SSE stream
/// without `tool-output-available` for those tools whenever the loop detector
/// raised LoopBreak right after `join_all`. Frontend would render a perpetual
/// "in flight" spinner for tools that actually finished.
pub struct BatchOutcome {
    pub results: Vec<ToolBatchResult>,
    /// When `Some`, the loop-break reason that should terminate the turn.
    /// Caller still emits ToolResult for `results` first, then handles break.
    pub loop_break: Option<Option<String>>,
}

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
/// `messages` table immediately after its `tool_end` timeline entry, via a
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

    /// Outer wrapper timeout for a non-`agent` tool call. Reads
    /// `AppConfig.agent_tool` (`default_tool_timeout_secs` + per-tool
    /// `tool_timeout_overrides`) at call time so config hot-reload takes effect
    /// on the next tool batch. The default impl returns the historical
    /// hardcoded 120s so test stubs keep compiling.
    fn tool_timeout(&self, _tool_name: &str) -> Duration {
        Duration::from_secs(120)
    }

    /// Per-tool semantic-cache config (None = tool is not cacheable). Default: not cacheable.
    fn semantic_cache_config(&self, _tool: &str) -> Option<crate::config::SemanticCacheToolConfig> {
        None
    }
}

// ── Helper predicates ────────────────────────────────────────────────────────

/// Derive the LoopDetector key for a tool call. For `tool_use` calls that
/// are not rewritten (search/describe), include the action so the detector
/// distinguishes legitimate `search → describe → call` sequences from
/// pathological loops on a single action.
fn loop_detector_key(tc: &opex_types::ToolCall) -> String {
    if tc.name != "tool_use" {
        return tc.name.clone();
    }
    let action = tc
        .arguments
        .get("action")
        .and_then(|v| v.as_str())
        .unwrap_or("?");
    format!("tool_use:{action}")
}

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

/// Wrap a completed tool result in the `<untrusted_tool_output>` provenance
/// delimiter (defense-in-depth against indirect prompt injection from
/// external content — web pages, browser automation, MCP servers, search
/// results) when `name` is classified untrusted.
///
/// For YAML tools (present in `yaml_tools`), classification uses
/// [`crate::agent::provenance::is_untrusted_yaml_tool`], which combines the
/// name-hint check with the tool's configured HTTP endpoint — this closes
/// the under-wrap gap for YAML tools that call an external third-party API
/// but whose name carries no `web`/`browser`/`search`/`its` hint (e.g.
/// `urban_dictionary` → `api.urbandictionary.com`). System/MCP tools (not in
/// `yaml_tools`) fall back to the name/MCP-only
/// [`crate::agent::provenance::is_untrusted_tool`].
///
/// Skips the wrap in two cases where the string reaching this point is NOT
/// externally-fetched content, even for an otherwise-untrusted tool name:
///
/// 1. `result` carries an inline media marker (`__file__:` / `__rich_card__:`,
///    see `agent::engine::{FILE_PREFIX, RICH_CARD_PREFIX}`) — extracted
///    verbatim downstream by `pipeline::execute::extract_tool_result_events`
///    to drive the SSE File/RichCard events, so wrapping the whole string
///    would garble the binary upload URL / rich-card JSON with surrounding
///    XML-ish delimiter text.
/// 2. `name` is a YAML tool with `channel_action` configured (TTS/imagegen/
///    `screenshot_web`, …) — its non-inline-media branch returns a trusted,
///    OPEX-authored `"[SYSTEM] … dispatched in background …"` instruction
///    (see `pipeline::media_background::MediaKind::system_message`), not
///    fetched external content. Wrapping that instruction as "untrusted"
///    would be a false positive that could make the model discount an
///    OPEX-authored directive.
///
/// This means media-producing untrusted tools (e.g. `screenshot_web`) ship
/// both of their result shapes (inline marker and background instruction)
/// unwrapped, same as before this hardening — only the plain-text result
/// path (e.g. `web`, `browser`, search tools) gains the provenance boundary.
///
/// Called AFTER `truncate_tool_result` in both the parallel and sequential
/// branches so the wrapper tags themselves are never truncated.
async fn maybe_wrap_untrusted_result(
    name: &str,
    result: String,
    mcp: Option<&crate::mcp::McpRegistry>,
    yaml_tools: &HashMap<String, YamlToolDef>,
) -> String {
    use crate::agent::engine::{FILE_PREFIX, RICH_CARD_PREFIX, TOOL_CALL_PREFIX};
    if result.contains(FILE_PREFIX) || result.starts_with(RICH_CARD_PREFIX) || result.contains(TOOL_CALL_PREFIX) {
        return result;
    }
    if yaml_tools
        .get(name)
        .is_some_and(|t| t.channel_action.is_some())
    {
        return result;
    }
    if let Some(yaml_tool) = yaml_tools.get(name) {
        return if crate::agent::provenance::is_untrusted_yaml_tool(name, &yaml_tool.endpoint) {
            crate::agent::provenance::wrap_untrusted_tool_output(name, &result)
        } else {
            result
        };
    }
    let is_mcp = match mcp {
        Some(reg) => reg.find_mcp_for_tool(name).await.is_some(),
        None => false,
    };
    if crate::agent::provenance::is_untrusted_tool(name, is_mcp) {
        crate::agent::provenance::wrap_untrusted_tool_output(name, &result)
    } else {
        result
    }
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
#[tracing::instrument(
    name = "pipeline.execute_tools",
    skip_all,
    fields(
        session_id = %session_id,
        tool_count = tool_calls.len(),
        loop_break = tracing::field::Empty,
    )
)]
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
    policy: Option<&crate::config::AgentToolPolicy>,
    // `extra_deny`: subagent isolation list applied at dispatcher rewrite.
    // Subagent callers pass runtime_subagent_denylist(&parent.delegation)
    // so tool_use(action=call, name=X) cannot reach a tool blocked at the
    // delegation layer. Non-subagent callers pass &[].
    extra_deny: &[String],
    mcp: Option<&crate::mcp::McpRegistry>,
    parallel_batch_id: Option<ParallelBatchId>,
) -> BatchOutcome {
    // ── Dispatcher rewrite ───────────────────────────────────────────────────
    //
    // For every `tool_use(action="call", name=X, arguments=Y)` call, rewrite
    // to a synthetic ToolCall { name: X, arguments: Y } so dispatch below sees
    // the underlying tool. Runtime deny-gate runs inside `rewrite_tool_use_calls`
    // — a denied call is replaced with a synthesized tool result and never reaches dispatch.

    let known_tools: std::collections::HashSet<String> = {
        let mut s = std::collections::HashSet::new();
        for n in crate::agent::pipeline::tool_defs::all_system_tool_names() {
            s.insert((*n).to_string());
        }
        for name in yaml_tools.keys() {
            s.insert(name.clone());
        }
        // MCP tools — without this, the rewrite step rejects MCP calls as
        // "not found" so MCP becomes uncallable when the dispatcher is on
        // (since tool_use is the only entry point for them).
        if let Some(reg) = mcp {
            for d in reg.all_tool_definitions().await {
                s.insert(d.name);
            }
        }
        s
    };

    let rewritten = crate::agent::dispatcher::rewrite_tool_use_calls(
        tool_calls, policy, &known_tools, extra_deny,
    );

    let mut direct_calls: Vec<ToolCall> = Vec::with_capacity(rewritten.len());
    let mut denied_results: Vec<(String, String)> = Vec::new();

    for r in rewritten.into_iter() {
        match r {
            crate::agent::dispatcher::RewriteResult::Direct(rewritten_tc) => {
                direct_calls.push(rewritten_tc);
            }
            crate::agent::dispatcher::RewriteResult::Denied { id, reason } => {
                denied_results.push((id, reason));
            }
        }
    }

    // T3: ParallelBatchId is provided by the caller (`execute.rs`) and
    // threaded into `messages.parallel_batch_id` for tools in the parallel
    // join_all. See spec
    // `docs/superpowers/specs/2026-05-07-s2-identity-first-stream-objects-design.md`
    // (T3) for NULL semantics.

    // Hold onto the original input slice — the final BatchOutcome.results must
    // be ordered by the original `tool_calls` input (denied + dispatched, by
    // tool_call_id). The dispatch loop below operates on `direct_calls` (the
    // post-rewrite batch).
    let original_calls: &[ToolCall] = tool_calls;
    let tool_calls: &[ToolCall] = &direct_calls;

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
    let mut enriched: Vec<Value> = tool_calls
        .iter()
        .map(|tc| enrich_tool_args(&tc.arguments, context, session_id, channel))
        .collect();

    // 2. Semantic cache check
    for (i, tc) in tool_calls.iter().enumerate() {
        if executor.semantic_cache_config(&tc.name).is_some() && embedder.is_available() {
            let query_text = tc
                .arguments
                .get("query")
                .or_else(|| tc.arguments.get("url"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let threshold = executor
                .semantic_cache_config(&tc.name)
                .map(|c| c.threshold)
                .unwrap_or(0.95);
            if !query_text.is_empty()
                && let Ok(Some(cached_res)) =
                    SemanticCache::check(db, embedder, &tc.name, query_text, threshold).await
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
    let agent_safety_timeout = executor.agent_safety_timeout();

    // NOTE: `args_hash` MUST hash `loop_detector_key(tc)` (NOT `tc.name`) so the
    // persisted+replayed hash matches the LIVE detector, which keys on
    // `loop_detector_key` (e.g. `"tool_use:{action}"` on the dispatcher path).
    // Hashing `tc.name` here would produce `hash("tool_use")` while the live
    // `check_limits` compares `hash("tool_use:search")` — never matching, silently
    // defeating LoopDetector warm-up across restart for the common `tool_use` loop.
    let start_payload = |tc: &ToolCall| -> Value {
        serde_json::json!({
            "tool_call_id": tc.id,
            "tool_name": tc.name,
            "args_hash": format!("{:x}", LoopDetector::hash_call_raw(&loop_detector_key(tc), &tc.arguments))
        })
    };
    let end_payload = |tc: &ToolCall, res: &str| -> Value {
        let success =
            !res.to_lowercase().contains("error") && !res.to_lowercase().contains("failed");
        serde_json::json!({
            "tool_call_id": tc.id,
            "tool_name": tc.name,
            "success": success,
            "args_hash": format!("{:x}", LoopDetector::hash_call_raw(&loop_detector_key(tc), &tc.arguments))
        })
    };

    // 4a. Parallel batch
    //
    // T3: stamp every tool in this join_all with the caller-supplied
    // `parallel_batch_id` IF parallel_indices has ≥2 tools. Single-parallel
    // batches stay None on the persisted row (a batch of one is not a batch).
    let active_batch_id: Option<ParallelBatchId> = if parallel_indices.len() >= 2 {
        parallel_batch_id
    } else {
        None
    };
    if !parallel_indices.is_empty() {
        for &i in &parallel_indices {
            let _ = crate::db::session_timeline::log_event(
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
        //
        // All parallel tool messages share the SAME parent (the assistant message
        // that emitted the calls). Chaining them to each other was a workaround
        // that made resolveActivePath traversal work but produced a semantically
        // wrong tree — parallel results are siblings, not a linear chain.
        // chain_parent advances to the LAST parallel tool after all are allocated
        // so the next message (the following assistant turn) still chains correctly.
        let parallel_persist_meta: Vec<Option<(Uuid, Option<Uuid>)>> = if persist_ctx.is_some() {
            let shared_parent = chain_parent;
            let mut out: Vec<Option<(Uuid, Option<Uuid>)>> = vec![None; n];
            for &i in &parallel_indices {
                let new_id = Uuid::new_v4();
                out[i] = Some((new_id, shared_parent));
                persisted_ids[i] = Some(new_id);
            }
            // Advance chain_parent to the last parallel tool (by declaration order)
            // so the subsequent sequential tools / assistant message chain off it.
            if let Some(&last_i) = parallel_indices.last()
                && let Some((last_id, _)) = out[last_i]
            {
                chain_parent = Some(last_id);
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
                        executor.tool_timeout(&name)
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
                    let truncated = super::context::truncate_tool_result(
                        model,
                        &result,
                        current_context_chars,
                    );
                    (i, truncated)
                }
            })
            .collect();

        for (i, result) in futures_util::future::join_all(futs).await {
            // Record the result FIRST so a subsequent loop-break check still
            // surfaces this completed tool's output to the caller. Without
            // this, the early Err path would leave results[i] = None and the
            // tool would render as "in flight forever" on the UI. NOTE: this
            // is the RAW (unwrapped) result — the loop-break early-return
            // below intentionally surfaces it as-is rather than paying the
            // async provenance-wrap cost on a path that's about to terminate
            // the turn; the provenance wrap is applied on the normal
            // (non-broken) completion path further down before persist.
            results[i] = Some(result.clone());

            if detect_loops {
                let key = loop_detector_key(&tool_calls[i]);
                if let LoopStatus::Break(reason) =
                    detector.check_limits(&key, &tool_calls[i].arguments)
                {
                    tracing::error!(tool = %tool_calls[i].name, reason = %reason, "tool loop broken (parallel post-check)");
                    return BatchOutcome {
                        results: assemble_ordered(
                            original_calls,
                            tool_calls,
                            &mut results,
                            &persisted_ids,
                            &denied_results,
                        ),
                        loop_break: Some(Some(reason)),
                    };
                }
                let success = !result.starts_with("Error:")
                    && !result.starts_with("tool error:")
                    && !result.contains("timed out");
                detector.record_execution(&key, &tool_calls[i].arguments, success);
            }

            // Store in semantic cache if successful
            if executor.semantic_cache_config(&tool_calls[i].name).is_some()
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
                    let ttl_secs = executor
                        .semantic_cache_config(&tool_calls[i].name)
                        .map(|c| c.ttl_secs as i64)
                        .unwrap_or(3600);
                    let _ = SemanticCache::store(
                        db,
                        embedder,
                        &tool_calls[i].name,
                        query_text,
                        &result,
                        ttl_secs,
                    )
                    .await;
                }
            }

            let _ = crate::db::session_timeline::log_event(
                db,
                session_id,
                "tool_end",
                Some(&end_payload(&tool_calls[i], &result)),
            )
            .await;

            // Provenance wrap happens LAST, after loop-detection /
            // semantic-cache / timeline bookkeeping above (all of which need
            // the raw "Error:"/"tool error:" prefix on `result` to classify
            // success/failure). Re-assign `results[i]` to the wrapped text so
            // the LLM context and persisted row both see the same
            // provenance-tagged content the model reasons over.
            let wrapped =
                maybe_wrap_untrusted_result(&tool_calls[i].name, result.clone(), mcp, yaml_tools)
                    .await;
            results[i] = Some(wrapped.clone());

            // Durable persist for THIS tool — spawned immediately after its
            // `tool_end` timeline entry so we don't leave a window where the timeline says
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
                    tool_calls[i].id.as_str(),
                    &wrapped,
                    parent_for_this,
                    active_batch_id,
                );
            }
        }
    }

    // 4b. Sequential
    //
    // Parallel branch intentionally does NOT inject `tool_message_id` into
    // `enriched[i]._context` — YAML channel-action tools are partitioned to
    // sequential by line ~339 (`tool.parallel && tool.channel_action.is_none()`),
    // so the parallel path never reaches a YAML tool that would consume
    // `_context.tool_message_id`. Threading the id through the parallel branch
    // would require restructuring `parallel_persist_meta` for zero behaviour
    // change.
    for &i in &sequential_indices {
        let seq_key = loop_detector_key(&tool_calls[i]);
        if detect_loops
            && let LoopStatus::Break(reason) =
                detector.check_limits(&seq_key, &tool_calls[i].arguments)
        {
            tracing::error!(tool = %tool_calls[i].name, reason = %reason, "tool loop broken (pre-check)");
            return BatchOutcome {
                results: assemble_ordered(
                    original_calls,
                    tool_calls,
                    &mut results,
                    &persisted_ids,
                    &denied_results,
                ),
                loop_break: Some(Some(reason)),
            };
        }
        let _ = crate::db::session_timeline::log_event(
            db,
            session_id,
            "tool_start",
            Some(&start_payload(&tool_calls[i])),
        )
        .await;
        // Pre-generate the persisted message-row id BEFORE dispatch so
        // `_context.tool_message_id` can be threaded into the YAML
        // channel-action tools (e.g. TTS / generate_image). The same id is
        // reused by `spawn_persist_tool_message` after dispatch returns, so
        // the chain stays deterministic. Gated on `persist_ctx.is_some()` —
        // off-the-record paths (subagent / openai_compat) keep the id
        // absent so no UUID leaks into their `_context`.
        if persist_ctx.is_some() {
            let new_id = Uuid::new_v4();
            persisted_ids[i] = Some(new_id);
            if let Some(obj) = enriched[i].as_object_mut()
                && let Some(ctx) = obj.get_mut("_context").and_then(|v| v.as_object_mut())
            {
                ctx.insert(
                    "tool_message_id".to_string(),
                    serde_json::json!(new_id.to_string()),
                );
            }
        }
        // See note on the parallel branch: the `agent` tool owns its own
        // longer sync timeouts. The outer wrapper here is a defense-in-depth
        // safety net — strictly larger than every inner cap so the inner
        // timeouts fire first under normal conditions.
        let timeout = if tool_calls[i].name == "agent" {
            agent_safety_timeout
        } else {
            executor.tool_timeout(&tool_calls[i].name)
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
            detector.record_execution(&seq_key, &tool_calls[i].arguments, success);
        }

        // Store in semantic cache if successful
        if executor.semantic_cache_config(&tool_calls[i].name).is_some()
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
                let ttl_secs = executor
                    .semantic_cache_config(&tool_calls[i].name)
                    .map(|c| c.ttl_secs as i64)
                    .unwrap_or(3600);
                let _ = SemanticCache::store(
                    db,
                    embedder,
                    &tool_calls[i].name,
                    query_text,
                    &res,
                    ttl_secs,
                )
                .await;
            }
        }

        // Provenance wrap happens LAST, after loop-detection / semantic-cache
        // bookkeeping above, which need to see the raw "Error:"/"tool error:"
        // prefix on `res` to classify success/failure. Wrapping earlier would
        // hide those prefixes behind the `<untrusted_tool_output>` tag and
        // silently defeat error classification for untrusted tools.
        let wrapped =
            maybe_wrap_untrusted_result(&tool_calls[i].name, res.clone(), mcp, yaml_tools).await;
        let _ = crate::db::session_timeline::log_event(
            db,
            session_id,
            "tool_end",
            Some(&end_payload(&tool_calls[i], &res)),
        )
        .await;

        // Durable persist for this sequential tool. Detached so it survives
        // parent-task cancellation between here and `execute()` returning.
        // T3: sequential tools are by definition not part of a parallel
        // batch — pass `None` for parallel_batch_id.
        //
        // The row id was pre-generated above (before dispatch) so the YAML
        // channel-action path could thread it into `_context.tool_message_id`.
        // Reuse it here — `Uuid::new_v4()` is NOT called twice.
        //
        // Persist the WRAPPED text (same as `results[i]`, which feeds the LLM
        // context and the display path) so a session reload/resume sees the
        // same provenance-tagged content the model originally reasoned over.
        if let Some(pctx) = persist_ctx {
            let new_id = persisted_ids[i].expect(
                "persisted_ids[i] was set above when persist_ctx is Some — invariant",
            );
            spawn_persist_tool_message(
                db,
                new_id,
                session_id,
                pctx.agent_name,
                tool_calls[i].id.as_str(),
                &wrapped,
                chain_parent,
                None,
            );
            chain_parent = Some(new_id);
        }
        results[i] = Some(wrapped);
    }

    // 5. Final reassemble — merge denied + dispatched, re-order by original input.
    BatchOutcome {
        results: assemble_ordered(
            original_calls,
            tool_calls,
            &mut results,
            &persisted_ids,
            &denied_results,
        ),
        loop_break: None,
    }
}

/// Merge dispatched tool results (`results`/`persisted_ids` indexed by
/// `dispatched_calls`) with `denied` (`(id, reason)` pairs synthesized by the
/// rewrite step), re-ordered by the original input slice. Empty result strings
/// are emitted for any dispatched id that was never written (e.g. early-loop-
/// break path) so the SSE event still fires and the UI doesn't render a
/// perpetual "in flight" spinner.
fn assemble_ordered(
    original_calls: &[ToolCall],
    dispatched_calls: &[ToolCall],
    results: &mut [Option<String>],
    persisted_ids: &[Option<Uuid>],
    denied: &[(String, String)],
) -> Vec<ToolBatchResult> {
    // Keyed by string so we can mix dispatcher-denied ids (`String`) with
    // dispatched ToolCallId values without unifying the key type.
    let mut by_id: std::collections::HashMap<String, ToolBatchResult> =
        std::collections::HashMap::with_capacity(original_calls.len());

    for (id, reason) in denied {
        by_id.insert(
            id.clone(),
            ToolBatchResult {
                tool_call_id: id.clone(),
                result: format!("Error: {reason}"),
                tool_msg_id: None,
            },
        );
    }

    for (j, tc) in dispatched_calls.iter().enumerate() {
        by_id.insert(
            tc.id.as_str().to_string(),
            ToolBatchResult {
                tool_call_id: tc.id.as_str().to_string(),
                result: results[j].take().unwrap_or_default(),
                tool_msg_id: persisted_ids[j],
            },
        );
    }

    original_calls
        .iter()
        .filter_map(|tc| by_id.remove(tc.id.as_str()))
        .collect()
}

// ── Detached persistence helpers ─────────────────────────────────────────────

/// Internal: spawn a fire-and-forget tokio task that inserts one row into
/// `messages` via `save_message_ex_with_id`. Single source of truth for the
/// detached-persist scaffolding (`db.clone`, owned-string conversion,
/// `tokio::spawn`, `tracing::warn` formatting on insert error). The two public
/// wrappers below ([`spawn_persist_tool_message`],
/// [`spawn_persist_assistant_message`]) shape arguments and delegate here.
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
/// SIGTERM mid-flight is one row, which the user can reproduce by re-asking.
/// Threading `bg_tasks` through every persist-aware call site is a larger
/// refactor and not the scope of this gap-fix.
#[allow(clippy::too_many_arguments)]
fn spawn_persist_message_row(
    db: &sqlx::PgPool,
    id: Uuid,
    session_id: Uuid,
    role: &'static str,
    agent_name: &str,
    content: &str,
    tool_calls_json: Option<&serde_json::Value>,
    thinking_blocks_json: Option<&serde_json::Value>,
    tool_call_id: Option<&str>,
    parent_id: Option<Uuid>,
    parallel_batch_id: Option<ParallelBatchId>,
) {
    let db = db.clone();
    let agent_name = agent_name.to_string();
    let content = content.to_string();
    let tool_call_id_owned = tool_call_id.map(std::string::ToString::to_string);
    let tool_calls_owned = tool_calls_json.cloned();
    let thinking_owned = thinking_blocks_json.cloned();
    // AUDIT-FF-015: see docs/superpowers/specs/2026-05-06-s5-tech-debt-hygiene-design.md
    tokio::spawn(async move {
        // Retry up to 3 times with short backoff to handle the race where a
        // parent message insert (also detached) hasn't committed yet when this
        // child insert fires. Retryable: FK violation (parent not yet visible)
        // and transient connection errors. Non-retryable errors bail immediately.
        let mut last_err: Option<anyhow::Error> = None;
        for attempt in 0..3u32 {
            if attempt > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(50 * (1 << attempt))).await;
            }
            match crate::db::sessions::save_message_ex_with_id(
                &db,
                id,
                session_id,
                role,
                &content,
                tool_calls_owned.as_ref(),
                tool_call_id_owned.as_deref(),
                Some(&agent_name),
                thinking_owned.as_ref(),
                parent_id,
                parallel_batch_id,
            )
            .await
            {
                Ok(()) => return,
                Err(e) => {
                    let msg = e.to_string();
                    let retryable = msg.contains("foreign key") || msg.contains("fkey")
                        || msg.contains("connection") || msg.contains("pool");
                    if !retryable {
                        tracing::warn!(
                            error = %e,
                            session_id = %session_id,
                            msg_id = %id,
                            role = role,
                            tool_call_id = ?tool_call_id_owned,
                            "failed to persist message row (detached)"
                        );
                        return;
                    }
                    last_err = Some(e);
                }
            }
        }
        if let Some(e) = last_err {
            tracing::warn!(
                error = %e,
                session_id = %session_id,
                msg_id = %id,
                role = role,
                tool_call_id = ?tool_call_id_owned,
                "failed to persist message row after retries (detached)"
            );
        }
    });
}

/// Spawn a fire-and-forget tokio task that persists a single tool result row
/// to the `messages` table. Thin wrapper over [`spawn_persist_message_row`]
/// fixing `role = "tool"` and `tool_calls_json = thinking_blocks_json = None`.
///
/// `parallel_batch_id` — `Some(_)` when this tool ran in a parallel batch
/// (≥2 concurrent tools in one turn); `None` for sequential / single-tool
/// turns. Written atomically into `messages.parallel_batch_id` (m047) by
/// the main INSERT in `save_message_ex_with_id` (D3 fix, 2026-05-13).
#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn_persist_tool_message(
    db: &sqlx::PgPool,
    id: Uuid,
    session_id: Uuid,
    agent_name: &str,
    tool_call_id: &str,
    content: &str,
    parent_id: Option<Uuid>,
    parallel_batch_id: Option<ParallelBatchId>,
) {
    spawn_persist_message_row(
        db,
        id,
        session_id,
        "tool",
        agent_name,
        content,
        None,
        None,
        Some(tool_call_id),
        parent_id,
        parallel_batch_id,
    );
}

/// Spawn a fire-and-forget tokio task that persists an INTERMEDIATE assistant
/// message (the one that holds `tool_calls`) to the `messages` table. Thin
/// wrapper over [`spawn_persist_message_row`] fixing `role = "assistant"` and
/// `tool_call_id = None`.
///
/// Mirrors [`spawn_persist_tool_message`] for the assistant-with-tool-calls
/// case in `pipeline::execute` and the legacy `engine/stream.rs` path. The
/// synchronous-await variant (`SessionManager::save_message_ex(...).await`)
/// leaves a cancellation gap: if the engine task is aborted during the await
/// (e.g. SSE client disconnect), the row is never written and subsequent tool
/// messages have no parent assistant — the chain is broken on reload. Detached
/// spawn closes that gap.
///
/// `step_id` — when `Some`, an UPDATE follows the insert to set the row's
/// `step_id` column (added by migration 046). Lets analytics or per-step
/// UI features query intermediate iterations by their tool-loop position.
/// `None` is treated as "don't set" so legacy callers keep working.
///
/// TODO (post-D3, 2026-05-13): same latent race shape as the `parallel_batch_id`
/// UPDATE that D3 eliminated — the 20ms lead-in `tokio::sleep` is racing the
/// detached INSERT, and `UPDATE ... WHERE id = X` against a not-yet-committed
/// row returns 0 rows affected with `Ok(())`, silently losing the step_id tag.
/// Fix by following the D3 pattern: thread `step_id` into the INSERT signature
/// of `save_message_ex_with_id` (add `step_id: Option<i32>` parameter and a new
/// column to the INSERT), then drop this secondary UPDATE entirely. Out of
/// scope for the D3 spec — see
/// `docs/superpowers/specs/2026-05-13-session-active-path-truncation-design.md`
/// §"Out of scope".
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
    step_id: Option<i32>,
) {
    spawn_persist_message_row(
        db,
        id,
        session_id,
        "assistant",
        agent_name,
        content,
        tool_calls_json,
        thinking_blocks_json,
        None,
        parent_id,
        None, // assistant rows are never part of a parallel tool batch
    );
    if let Some(step) = step_id {
        let db_clone = db.clone();
        tokio::spawn(async move {
            // Tiny delay so the insert above (also detached) gets a head start.
            // Failures are non-fatal — step_id is metadata, not load-bearing.
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            for attempt in 0..3u32 {
                match crate::db::sessions::update_message_step_id(&db_clone, id, step).await {
                    Ok(()) => return,
                    Err(_) if attempt < 2 => {
                        tokio::time::sleep(std::time::Duration::from_millis(50 * (1 << attempt))).await;
                    }
                    Err(e) => {
                        tracing::debug!(error = %e, msg_id = %id, "step_id update failed (non-fatal)");
                        return;
                    }
                }
            }
        });
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_result(id: &str, body: &str) -> ToolBatchResult {
        ToolBatchResult {
            tool_call_id: id.to_string(),
            result: body.to_string(),
            tool_msg_id: None,
        }
    }

    #[test]
    fn batch_outcome_no_loop_break_carries_all_results() {
        // Ok path: every tool completed, no break — outcome.results contains
        // the full vec, loop_break is None. This is what callers depend on
        // when emitting ToolResult SSE events.
        let outcome = BatchOutcome {
            results: vec![mk_result("t1", "ok1"), mk_result("t2", "ok2")],
            loop_break: None,
        };
        assert_eq!(outcome.results.len(), 2);
        assert!(outcome.loop_break.is_none());
        // Both ids visible to the caller iteration:
        let ids: Vec<&str> = outcome
            .results
            .iter()
            .map(|r| r.tool_call_id.as_str())
            .collect();
        assert_eq!(ids, vec!["t1", "t2"]);
    }

    #[test]
    fn batch_outcome_loop_break_preserves_completed_results() {
        // The Phase-5 invariant: a loop break does NOT discard completed
        // tools. Caller (execute.rs) must still see them so it can emit
        // ToolResult SSE events for each, preventing perpetual frontend
        // spinners on tools that actually completed.
        let outcome = BatchOutcome {
            results: vec![mk_result("t1", "completed"), mk_result("t2", "")],
            loop_break: Some(Some("repeated_pattern".to_string())),
        };
        // Caller observes both completed and uncompleted entries; uncompleted
        // entries (t2) have empty result string so the SSE event still fires.
        assert_eq!(outcome.results.len(), 2);
        assert_eq!(outcome.results[0].result, "completed");
        assert_eq!(outcome.results[1].result, "");
        assert_eq!(
            outcome.loop_break,
            Some(Some("repeated_pattern".to_string()))
        );
    }

    #[test]
    fn batch_outcome_loop_break_without_reason() {
        // Loop break can fire without a specific reason string — the inner
        // Option<String> represents "we know it's a loop, no specifics".
        // Caller still sees Some(_) so it knows to break.
        let outcome = BatchOutcome {
            results: vec![],
            loop_break: Some(None),
        };
        assert!(outcome.loop_break.is_some());
        assert!(outcome.loop_break.as_ref().unwrap().is_none());
    }

    #[test]
    fn tool_batch_result_tool_msg_id_optional() {
        // tool_msg_id is None when persist_ctx was None (e.g. subagent path
        // that doesn't persist tool results). Caller must treat as ephemeral.
        let r = mk_result("t1", "result");
        assert!(r.tool_msg_id.is_none());

        let with_id = ToolBatchResult {
            tool_call_id: "t2".to_string(),
            result: "x".to_string(),
            tool_msg_id: Some(uuid::Uuid::nil()),
        };
        assert!(with_id.tool_msg_id.is_some());
    }

    // ── maybe_wrap_untrusted_result (Batch J) ─────────────────────────────────

    fn channel_action_yaml_tool(name: &str) -> crate::tools::yaml_tools::YamlToolDef {
        serde_yaml::from_str(&format!(
            "name: {name}\n\
             description: capture-only channel-action YAML tool\n\
             endpoint: \"http://127.0.0.1:1\"\n\
             method: POST\n\
             timeout: 5\n\
             channel_action:\n  action: send_photo\n  data_field: _binary\n",
        ))
        .expect("valid yaml")
    }

    #[tokio::test]
    async fn wraps_web_fetch_plain_text_result() {
        let yaml_tools: HashMap<String, crate::tools::yaml_tools::YamlToolDef> = HashMap::new();
        let out =
            maybe_wrap_untrusted_result("web_fetch", "page body".to_string(), None, &yaml_tools)
                .await;
        assert!(out.starts_with("<untrusted_tool_output tool=\"web_fetch\""), "{out}");
        assert!(out.contains("page body"));
    }

    #[tokio::test]
    async fn does_not_wrap_trusted_workspace_tool() {
        let yaml_tools: HashMap<String, crate::tools::yaml_tools::YamlToolDef> = HashMap::new();
        let out = maybe_wrap_untrusted_result(
            "workspace_read",
            "file contents".to_string(),
            None,
            &yaml_tools,
        )
        .await;
        assert_eq!(out, "file contents", "trusted tool must pass through unwrapped");
    }

    #[tokio::test]
    async fn does_not_wrap_result_carrying_file_marker() {
        // Even an untrusted-by-name tool (e.g. a hypothetical "web_capture")
        // must NOT be wrapped when its result carries an inline __file__:
        // marker — extract_tool_result_events downstream depends on parsing
        // that marker verbatim.
        let yaml_tools: HashMap<String, crate::tools::yaml_tools::YamlToolDef> = HashMap::new();
        let marker_body = format!(
            "{}{}",
            crate::agent::engine::FILE_PREFIX,
            r#"{"url":"/api/uploads/1?sig=x","mediaType":"image/png"}"#
        );
        let out =
            maybe_wrap_untrusted_result("screenshot_web", marker_body.clone(), None, &yaml_tools)
                .await;
        assert_eq!(out, marker_body, "file-marker result must pass through unwrapped");
    }

    #[tokio::test]
    async fn does_not_wrap_result_carrying_rich_card_marker() {
        let yaml_tools: HashMap<String, crate::tools::yaml_tools::YamlToolDef> = HashMap::new();
        let card_body = format!(
            "{}{}",
            crate::agent::engine::RICH_CARD_PREFIX,
            r#"{"card_type":"table","data":[]}"#
        );
        let out = maybe_wrap_untrusted_result("search_web", card_body.clone(), None, &yaml_tools)
            .await;
        assert_eq!(out, card_body, "rich-card result must pass through unwrapped");
    }

    #[tokio::test]
    async fn does_not_wrap_channel_action_background_dispatch_instruction() {
        // Critical regression guard: a YAML tool with `channel_action`
        // configured whose name also matches the untrusted "web" hint
        // (mirrors the real `screenshot_web` capability tool). When
        // delivered via the Telegram/Discord background-dispatch path, its
        // result is a trusted OPEX-authored "[SYSTEM] ... dispatched in
        // background ..." instruction, NOT externally fetched content — it
        // must never be wrapped as untrusted.
        let mut yaml_tools: HashMap<String, crate::tools::yaml_tools::YamlToolDef> =
            HashMap::new();
        yaml_tools.insert("screenshot_web".to_string(), channel_action_yaml_tool("screenshot_web"));

        let system_instruction =
            "[SYSTEM] Image dispatched in background; the user will receive a photo message directly. \
             End your turn immediately.".to_string();
        let out = maybe_wrap_untrusted_result(
            "screenshot_web",
            system_instruction.clone(),
            None,
            &yaml_tools,
        )
        .await;
        assert_eq!(
            out, system_instruction,
            "channel_action tool's background-dispatch instruction must pass through unwrapped"
        );
    }

    #[tokio::test]
    async fn wraps_yaml_tool_without_channel_action_matching_name_hint() {
        // Sanity check that the channel_action exemption is scoped: a YAML
        // tool with NO channel_action but a "web"/"browser"/"search" name
        // hint is still wrapped normally.
        let mut yaml_tools: HashMap<String, crate::tools::yaml_tools::YamlToolDef> =
            HashMap::new();
        let plain_web_tool: crate::tools::yaml_tools::YamlToolDef = serde_yaml::from_str(
            "name: web\n\
             description: read web pages\n\
             endpoint: \"http://127.0.0.1:1\"\n\
             method: POST\n\
             timeout: 5\n",
        )
        .expect("valid yaml");
        yaml_tools.insert("web".to_string(), plain_web_tool);

        let out =
            maybe_wrap_untrusted_result("web", "some article text".to_string(), None, &yaml_tools)
                .await;
        assert!(out.starts_with("<untrusted_tool_output tool=\"web\""), "{out}");
    }

    #[tokio::test]
    async fn wraps_external_endpoint_yaml_tool_with_no_name_hint() {
        // Regression guard for the under-wrap gap: `urban_dictionary` has no
        // web/browser/search/its name hint, but its endpoint is a real
        // external third-party API. `maybe_wrap_untrusted_result` must
        // consult the YAML tool's endpoint (via
        // `provenance::is_untrusted_yaml_tool`), not just its name, so this
        // still gets wrapped.
        let mut yaml_tools: HashMap<String, crate::tools::yaml_tools::YamlToolDef> =
            HashMap::new();
        let urban_dictionary_tool: crate::tools::yaml_tools::YamlToolDef = serde_yaml::from_str(
            "name: urban_dictionary\n\
             description: look up slang definitions\n\
             endpoint: \"https://api.urbandictionary.com/v0/define\"\n\
             method: GET\n\
             timeout: 5\n",
        )
        .expect("valid yaml");
        yaml_tools.insert("urban_dictionary".to_string(), urban_dictionary_tool);

        let out = maybe_wrap_untrusted_result(
            "urban_dictionary",
            "some slang definition".to_string(),
            None,
            &yaml_tools,
        )
        .await;
        assert!(
            out.starts_with("<untrusted_tool_output tool=\"urban_dictionary\""),
            "{out}"
        );
    }

    #[tokio::test]
    async fn does_not_wrap_yaml_tool_with_internal_endpoint_and_no_name_hint() {
        // A YAML tool routed to an admin-configured internal endpoint with no
        // untrusted name hint stays trusted internal — must not be wrapped.
        let mut yaml_tools: HashMap<String, crate::tools::yaml_tools::YamlToolDef> =
            HashMap::new();
        let internal_tool: crate::tools::yaml_tools::YamlToolDef = serde_yaml::from_str(
            "name: some_internal_tool\n\
             description: internal helper\n\
             endpoint: \"http://localhost:9011/do-thing\"\n\
             method: POST\n\
             timeout: 5\n",
        )
        .expect("valid yaml");
        yaml_tools.insert("some_internal_tool".to_string(), internal_tool);

        let out = maybe_wrap_untrusted_result(
            "some_internal_tool",
            "internal result".to_string(),
            None,
            &yaml_tools,
        )
        .await;
        assert_eq!(out, "internal result", "internal-endpoint tool must pass through unwrapped");
    }
}

#[cfg(test)]
mod sequential_enrichment_tests {
    //! Verify the QUICK-260508-0dj timing fix: the sequential dispatch branch
    //! of `execute_tool_calls_partitioned` MUST stamp `_context.tool_message_id`
    //! into the enriched arguments BEFORE calling `executor.execute_tool_call`,
    //! when (and only when) `persist_ctx = Some(_)`. The id used for the stamp
    //! is the same UUID that subsequently lands in `persisted_ids[i]` and gets
    //! handed to `spawn_persist_tool_message`, so the YAML channel-action path
    //! can resolve back to the persisted row.
    use super::*;
    use crate::memory::EmbeddingService;
    use crate::memory::embedding::FakeEmbedder;
    use opex_types::ToolCall;
    use opex_types::ids::ToolCallId;
    use std::sync::Mutex;

    /// Captures the enriched arguments handed to each `execute_tool_call`
    /// invocation so the test can assert on `_context.tool_message_id`.
    struct CapturingExecutor {
        captured: Arc<Mutex<Vec<serde_json::Value>>>,
    }

    impl ToolExecutor for CapturingExecutor {
        fn execute_tool_call<'a>(
            &'a self,
            _name: &'a str,
            arguments: &'a serde_json::Value,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = String> + Send + 'a>> {
            let captured = self.captured.clone();
            let args = arguments.clone();
            Box::pin(async move {
                captured.lock().expect("poisoned").push(args);
                "ok".to_string()
            })
        }

        fn needs_approval(&self, _tool_name: &str) -> bool {
            false
        }
    }

    /// Lazy PgPool that never connects. The sequential branch issues
    /// `crate::db::session_timeline::log_event(...)` and `spawn_persist_tool_message`,
    /// both of which swallow DB errors (the former via `let _ = ...`, the
    /// latter via detached `tokio::spawn`). Safe for unit-test shape checks.
    ///
    /// `acquire_timeout` is shrunk to 100ms so the timeline-event call doesn't
    /// stall the test for the default 30s pool acquire timeout.
    fn fake_db() -> sqlx::PgPool {
        sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(std::time::Duration::from_millis(100))
            .connect_lazy("postgres://invalid:invalid@127.0.0.1:1/invalid")
            .expect("lazy connect cannot fail")
    }

    fn make_yaml_channel_tool() -> (String, crate::tools::yaml_tools::YamlToolDef) {
        // Force sequential partitioning: a YAML tool with `parallel = true`
        // and a `channel_action` is routed to the sequential branch by line
        // ~339 of `execute_tool_calls_partitioned`. Mirrors the production
        // synthesize_speech / generate_image capability-tool shape.
        let tool: crate::tools::yaml_tools::YamlToolDef = serde_yaml::from_str(
            "name: tts_capture\n\
             description: capture-only TTS-style YAML tool\n\
             endpoint: \"http://127.0.0.1:1\"\n\
             method: POST\n\
             timeout: 5\n\
             parallel: true\n\
             channel_action:\n  action: send_voice\n  data_field: _binary\n",
        )
        .expect("valid yaml");
        ("tts_capture".to_string(), tool)
    }

    fn make_tool_call(name: &str, id: &str) -> ToolCall {
        ToolCall {
            id: ToolCallId::from(id.to_string()),
            name: name.to_string(),
            arguments: serde_json::json!({"input": "hello"}),
            thought_signature: None,
        }
    }

    #[tokio::test]
    async fn sequential_branch_stamps_tool_message_id_when_persist_ctx_some() {
        // Happy path: a YAML channel-action tool dispatched through the
        // sequential branch with `persist_ctx = Some(_)` MUST receive an
        // `_context.tool_message_id` that parses as a UUID, and that UUID
        // MUST match `persisted_ids[i]` returned in `BatchOutcome.results`.
        let (tool_name, tool_def) = make_yaml_channel_tool();
        let mut yaml_tools = HashMap::new();
        yaml_tools.insert(tool_name.clone(), tool_def);

        let captured: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
        let executor = CapturingExecutor { captured: captured.clone() };

        let calls = vec![make_tool_call(&tool_name, "call_001")];
        let context = serde_json::json!({"chat_id": 42, "channel": "telegram"});
        let session_id = uuid::Uuid::new_v4();

        let db = fake_db();
        let embedder: Arc<dyn EmbeddingService> = Arc::new(FakeEmbedder { available: false });
        let cfg = crate::agent::tool_loop::ToolLoopConfig::default();
        let mut detector = LoopDetector::new(&cfg);

        let pctx = ToolPersistCtx {
            agent_name: "test-agent",
            initial_parent: None,
        };

        let outcome = execute_tool_calls_partitioned(
            &calls,
            &context,
            session_id,
            "telegram",
            "test-model",
            10_000,
            &mut detector,
            false,
            &db,
            &embedder,
            &yaml_tools,
            &executor,
            Some(&pctx),
            None,
            &[], // extra_deny: no subagent isolation in this test
            None,
            None,
        )
        .await;

        let captured = captured.lock().expect("poisoned");
        assert_eq!(captured.len(), 1, "executor must be called exactly once");
        let stamped_id = captured[0]
            .get("_context")
            .and_then(|c| c.get("tool_message_id"))
            .and_then(|v| v.as_str())
            .map(uuid::Uuid::parse_str)
            .transpose()
            .expect("tool_message_id must parse as UUID")
            .expect("tool_message_id must be present in _context");

        assert_eq!(outcome.results.len(), 1);
        let persisted_id = outcome.results[0]
            .tool_msg_id
            .expect("persisted id must be Some when persist_ctx is Some");
        assert_eq!(
            stamped_id, persisted_id,
            "stamped _context.tool_message_id MUST equal persisted_ids[0] — \
             that's the whole reason for hoisting the UUID generation"
        );
    }

    #[tokio::test]
    async fn sequential_branch_omits_tool_message_id_when_persist_ctx_none() {
        // Regression guard: when `persist_ctx = None` (subagent / openai_compat
        // path), the sequential branch must NOT stamp `_context.tool_message_id`
        // — leaking a UUID into off-the-record paths could confuse YAML tools
        // that key off it.
        let (tool_name, tool_def) = make_yaml_channel_tool();
        let mut yaml_tools = HashMap::new();
        yaml_tools.insert(tool_name.clone(), tool_def);

        let captured: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
        let executor = CapturingExecutor { captured: captured.clone() };

        let calls = vec![make_tool_call(&tool_name, "call_002")];
        let context = serde_json::json!({"chat_id": 99, "channel": "telegram"});
        let session_id = uuid::Uuid::new_v4();

        let db = fake_db();
        let embedder: Arc<dyn EmbeddingService> = Arc::new(FakeEmbedder { available: false });
        let cfg = crate::agent::tool_loop::ToolLoopConfig::default();
        let mut detector = LoopDetector::new(&cfg);

        let outcome = execute_tool_calls_partitioned(
            &calls,
            &context,
            session_id,
            "telegram",
            "test-model",
            10_000,
            &mut detector,
            false,
            &db,
            &embedder,
            &yaml_tools,
            &executor,
            None, // persist_ctx = None — subagent/openai_compat path
            None,
            &[], // extra_deny: empty in this test
            None,
            None,
        )
        .await;

        let captured = captured.lock().expect("poisoned");
        assert_eq!(captured.len(), 1, "executor must be called exactly once");
        assert!(
            captured[0]
                .get("_context")
                .and_then(|c| c.get("tool_message_id"))
                .is_none(),
            "_context.tool_message_id MUST be absent when persist_ctx is None; got: {}",
            captured[0]
        );

        // Sanity: persisted_ids stays None too — no UUID was generated at all.
        assert!(
            outcome.results[0].tool_msg_id.is_none(),
            "tool_msg_id must be None on the off-the-record path"
        );
    }
}

#[cfg(test)]
mod loop_key_tests {
    use super::*;

    fn make_tc(name: &str, args: serde_json::Value) -> opex_types::ToolCall {
        opex_types::ToolCall {
            id: "test".into(),
            name: name.to_string(),
            arguments: args,
            thought_signature: None,
        }
    }

    #[test]
    fn key_for_non_tool_use_is_real_name() {
        let tc = make_tc("cron", serde_json::json!({}));
        assert_eq!(loop_detector_key(&tc), "cron");
    }

    #[test]
    fn key_for_tool_use_includes_action() {
        let tc = make_tc("tool_use", serde_json::json!({"action": "search"}));
        assert_eq!(loop_detector_key(&tc), "tool_use:search");

        let tc = make_tc("tool_use", serde_json::json!({"action": "describe"}));
        assert_eq!(loop_detector_key(&tc), "tool_use:describe");
    }

    #[test]
    fn key_for_tool_use_missing_action_is_question_mark() {
        let tc = make_tc("tool_use", serde_json::json!({}));
        assert_eq!(loop_detector_key(&tc), "tool_use:?");
    }
}

#[cfg(test)]
mod parallel_batch_persistence_tests {
    //! DB-backed regression tests for the D3 fix (2026-05-13):
    //! `parallel_batch_id` is written by the main INSERT in
    //! `save_message_ex_with_id`, not a follow-up UPDATE — eliminating the
    //! race where the UPDATE matched 0 rows because the INSERT hadn't
    //! committed yet (silent batch-tag loss observed on Arty's session).
    use opex_types::ids::ParallelBatchId;
    use sqlx::PgPool;
    use uuid::Uuid;

    #[sqlx::test(migrations = "../../migrations")]
    async fn save_message_writes_parallel_batch_id_atomically(pool: PgPool) -> anyhow::Result<()> {
        // Seed a session so the FK on messages.session_id is satisfied.
        let session_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO sessions (id, agent_id, user_id, channel) VALUES ($1, 'Test', 'u', 'ui')"
        )
        .bind(session_id)
        .execute(&pool)
        .await?;

        let msg_id = Uuid::new_v4();
        let batch = ParallelBatchId::new();

        // After D3 fix: parallel_batch_id is the 11th parameter and is
        // written in the same INSERT, no follow-up UPDATE needed.
        crate::db::sessions::save_message_ex_with_id(
            &pool,
            msg_id,
            session_id,
            "tool",
            "result body",
            None,
            Some("call_x"),
            Some("Test"),
            None,
            None,
            Some(batch),
        )
        .await?;

        let stored: Option<Uuid> = sqlx::query_scalar(
            "SELECT parallel_batch_id FROM messages WHERE id = $1"
        )
        .bind(msg_id)
        .fetch_one(&pool)
        .await?;

        assert_eq!(
            stored,
            Some(batch.as_uuid()),
            "batch id must be written by INSERT, not by a follow-up UPDATE"
        );
        Ok(())
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn mixed_parallel_sequential_dispatch_persists_tree_correctly(pool: PgPool) -> anyhow::Result<()> {
        // Seed: session + assistant row that "emitted" the tool calls.
        let session_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO sessions (id, agent_id, user_id, channel) VALUES ($1, 'Test', 'u', 'ui')"
        )
        .bind(session_id)
        .execute(&pool)
        .await?;

        let assistant_id = Uuid::new_v4();
        crate::db::sessions::save_message_ex_with_id(
            &pool,
            assistant_id,
            session_id,
            "assistant",
            "calling tools",
            None,
            None,
            Some("Test"),
            None,
            None,
            None,
        )
        .await?;

        // Simulate: 2 parallel tools (batch X), then 1 sequential.
        let batch = ParallelBatchId::new();
        let parallel_a = Uuid::new_v4();
        let parallel_b = Uuid::new_v4();
        let sequential_c = Uuid::new_v4();

        // Both parallel rows: parent = assistant, batch_id = X, atomically.
        for (id, tcid) in [(parallel_a, "call_a"), (parallel_b, "call_b")] {
            crate::db::sessions::save_message_ex_with_id(
                &pool,
                id,
                session_id,
                "tool",
                "ok",
                None,
                Some(tcid),
                Some("Test"),
                None,
                Some(assistant_id),
                Some(batch),
            )
            .await?;
        }

        // Sequential follow-up: parent = parallel_b (declaration-last parallel),
        // batch_id = None.
        crate::db::sessions::save_message_ex_with_id(
            &pool,
            sequential_c,
            session_id,
            "tool",
            "ok",
            None,
            Some("call_c"),
            Some("Test"),
            None,
            Some(parallel_b),
            None,
        )
        .await?;

        // Assertions: all 3 tool rows persisted with the correct shape.
        let rows: Vec<(Uuid, Option<Uuid>, Option<Uuid>)> = sqlx::query_as(
            "SELECT id, parent_message_id, parallel_batch_id FROM messages \
             WHERE session_id = $1 AND role = 'tool' ORDER BY created_at"
        )
        .bind(session_id)
        .fetch_all(&pool)
        .await?;

        assert_eq!(rows.len(), 3, "expected 3 tool rows");

        // First two: parent = assistant, batch_id = batch.
        assert_eq!(rows[0].1, Some(assistant_id), "parallel_a parent must be assistant");
        assert_eq!(rows[0].2, Some(batch.as_uuid()), "parallel_a must have batch_id");
        assert_eq!(rows[1].1, Some(assistant_id), "parallel_b parent must be assistant");
        assert_eq!(rows[1].2, Some(batch.as_uuid()), "parallel_b must have batch_id");

        // Third: parent = parallel_b (declaration-last), batch_id = None.
        // NOTE: UI walker (D2 option B) does NOT depend on this invariant —
        // it uses hasDescendants derived from parent_message_id references.
        // This assertion guards the backend contract from regression.
        assert_eq!(rows[2].1, Some(parallel_b), "sequential_c parent must chain off parallel_b");
        assert_eq!(rows[2].2, None, "sequential_c must NOT have batch_id");

        Ok(())
    }
}
