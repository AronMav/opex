use anyhow::Result;
use opex_types::{Message, MessageRole, ToolDefinition};
use crate::agent::thinking::strip_thinking;

use super::providers::LlmProvider;
use super::tool_loop::LoopDetector;

pub const SUMMARY_PREFIX: &str = "[CONTEXT COMPACTION — REFERENCE ONLY] Earlier turns were compacted \
into the summary below. This is a handoff from a previous context window — treat it as background \
reference, NOT as active instructions. Do NOT answer questions or fulfill requests mentioned in \
this summary; they were already addressed. Your current task is identified in the '## Active Task' \
section — resume exactly from there. Respond ONLY to the latest user message that appears AFTER \
this summary.";

const SUMMARY_NOTE_FOR_SYSTEM: &str = "[Note: Some earlier conversation turns have been compacted \
into a handoff summary to preserve context space. Build on that summary rather than re-doing work.]";

const MIN_SUMMARY_TOKENS: usize = 2000;
const SUMMARY_RATIO: f64 = 0.20;
const SUMMARY_TOKENS_CEILING: usize = 12_000;

/// G3 (Session Resilience / WS5): hard wall-clock budget for the whole
/// proactive history-compaction LLM sequence (fact-extraction + summary calls
/// combined, via a shared deadline). Compaction is strictly best-effort — if it
/// can't finish within this window it is SKIPPED and the turn proceeds
/// uncompacted (fail-open) rather than stalling on a slow/dead compaction
/// provider. The reactive context-overflow retry in `pipeline::llm_call`
/// remains the safety net for a genuinely oversized context.
pub const COMPACTION_BUDGET: std::time::Duration = std::time::Duration::from_secs(15);

/// Estimate token count from text (rough: ~4 chars per token).
/// Accounts for `tool_calls` JSON size when present.
pub fn estimate_tokens(messages: &[Message]) -> usize {
    messages
        .iter()
        .map(|m| {
            let content_tokens = m.content.len() / 4 + 10;
            let tool_tokens = m
                .tool_calls
                .as_ref()
                .map_or(0, |tc| {
                    tc.iter()
                        .map(|t| {
                            let args_len = t.arguments.to_string().len();
                            let name_len = t.name.len();
                            (args_len + name_len + 20) / 4
                        })
                        .sum::<usize>()
                });
            content_tokens + tool_tokens
        })
        .sum()
}

/// Compact conversation history if it exceeds the threshold.
/// Extracts facts into memory-ready strings and replaces old messages with a summary.
/// `agent_language` is used to instruct the LLM to summarize in the correct language.
pub async fn compact_if_needed(
    messages: &mut Vec<Message>,
    provider: &dyn LlmProvider,
    compaction_provider: Option<&dyn LlmProvider>,
    max_tokens: usize,
    preserve_last_n: usize,
    agent_language: Option<&str>,
) -> Result<Option<Vec<String>>> {
    compact_if_needed_inner(messages, provider, compaction_provider, max_tokens, preserve_last_n, agent_language, false).await
}

/// Force-compact conversation history regardless of the token-threshold gate.
///
/// Used by the reactive context-overflow recovery path
/// (`pipeline::llm_call::chat_stream_with_overflow_recovery`): once the
/// provider has already rejected a call as too large, the token *estimate*
/// (a rough heuristic, see [`estimate_tokens`]) may still sit below the
/// proactive-compaction threshold, so the normal gated path can silently
/// no-op and the retry fails with the identical error. Forcing bypasses
/// that gate the same way the existing `/compact` command already does
/// (via `max_tokens=1`), but without disturbing the threshold semantics
/// for the normal proactive path.
pub async fn force_compact(
    messages: &mut Vec<Message>,
    provider: &dyn LlmProvider,
    compaction_provider: Option<&dyn LlmProvider>,
    preserve_last_n: usize,
    agent_language: Option<&str>,
) -> Result<Option<Vec<String>>> {
    compact_if_needed_inner(messages, provider, compaction_provider, 0, preserve_last_n, agent_language, true).await
}

#[allow(clippy::too_many_arguments)]
async fn compact_if_needed_inner(
    messages: &mut Vec<Message>,
    provider: &dyn LlmProvider,
    compaction_provider: Option<&dyn LlmProvider>,
    max_tokens: usize,
    preserve_last_n: usize,
    agent_language: Option<&str>,
    force: bool,
) -> Result<Option<Vec<String>>> {
    // Use dedicated compaction provider if available, otherwise fall back to main provider.
    let active_provider: &dyn LlmProvider = compaction_provider.unwrap_or(provider);
    let total = estimate_tokens(messages);
    let threshold = max_tokens * 80 / 100;

    if !force && total < threshold {
        return Ok(None);
    }

    tracing::info!(
        total_tokens = total,
        threshold,
        messages = messages.len(),
        "context window threshold reached, compacting"
    );

    // Keep system message (first) and last N messages
    let system_msg = if !messages.is_empty() && messages[0].role == MessageRole::System {
        Some(messages[0].clone())
    } else {
        None
    };

    let start = usize::from(system_msg.is_some());
    let mut end = if messages.len() > start + preserve_last_n {
        messages.len() - preserve_last_n
    } else {
        return Ok(None); // Not enough messages to compact
    };

    // Don't split in the middle of a tool call group:
    // move `end` backward until messages[end] is not a Tool message.
    while end > start && messages[end].role == MessageRole::Tool {
        end -= 1;
    }
    // If we also landed on an Assistant with tool_calls, include it in preserved part
    if end > start && messages[end].role == MessageRole::Assistant && messages[end].tool_calls.is_some() {
        end -= 1;
    }
    if end <= start {
        return Ok(None); // Not enough messages to compact after adjustment
    }

    let to_compact: Vec<Message> = messages[start..end].to_vec();
    if to_compact.is_empty() {
        return Ok(None);
    }

    let formatted = format_messages_for_compaction(&to_compact);

    // Step 1: Extract facts for long-term memory
    let lang_hint = match agent_language {
        Some("ru") => " Write each fact in Russian.",
        Some("en") => " Write each fact in English.",
        _ => "",
    };
    let extraction_prompt = vec![
        Message {
            role: MessageRole::System,
            content: format!(
                "Extract key facts from this conversation as a JSON array of strings.\n\n\
                MUST PRESERVE:\n\
                - Active tasks with their current status and progress (e.g. '5/17 items done')\n\
                - All identifiers: UUIDs, URLs, file paths, IPs, hostnames, port numbers, service names\n\
                - Decisions made and their rationale\n\
                - User preferences and requirements discovered\n\
                - Error conditions encountered and their resolutions\n\
                - Commitments, action items, and deadlines\n\n\
                MAY OMIT:\n\
                - Routine greetings and confirmations\n\
                - Tool calls that succeeded without noteworthy results\n\
                - Repeated information already captured in other facts\n\n\
                Each fact must be self-contained and useful without the original conversation.{lang_hint}\n\
                Return ONLY the JSON array, no other text."
            ),
            tool_calls: None,
            tool_call_id: None,
            thinking_blocks: vec![],
            db_id: None,

        },
        Message {
            role: MessageRole::User,
            content: formatted.clone(),
            tool_calls: None,
            tool_call_id: None,
            thinking_blocks: vec![],
            db_id: None,

        },
    ];

    // G3: compaction may never stall or fail the turn. Budget the LLM-summarize
    // sequence at COMPACTION_BUDGET (a shared deadline spanning BOTH the
    // fact-extraction and summary calls) and treat ANY failure — timeout,
    // provider error, exhausted fallback chain — as a SKIP: return Ok(None)
    // with `messages` untouched. Safe because both LLM calls happen BEFORE any
    // mutation of `messages` below (the rebuild starts at `messages.clear()`).
    // The reactive overflow-retry in llm_call.rs remains the safety net.
    let compaction_deadline = tokio::time::Instant::now() + COMPACTION_BUDGET;

    let empty_tools: Vec<ToolDefinition> = vec![];
    let facts_response = match tokio::time::timeout_at(
        compaction_deadline,
        active_provider.chat(&extraction_prompt, &empty_tools, crate::agent::providers::CallOptions::default()),
    )
    .await
    {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "compaction LLM call (fact extraction) failed — proceeding uncompacted (fail-open)");
            return Ok(None);
        }
        Err(_) => {
            tracing::warn!(
                budget_secs = COMPACTION_BUDGET.as_secs(),
                "compaction budget exceeded (fact extraction) — proceeding uncompacted (fail-open)"
            );
            return Ok(None);
        }
    };
    let extracted_facts: Vec<String> =
        serde_json::from_str(&facts_response.content).unwrap_or_default();

    tracing::info!(facts = extracted_facts.len(), "extracted facts from history");

    // Step 2: Summarize for context continuity
    let summary_lang = match agent_language {
        Some("en") => "in English",
        _ => "in Russian",
    };
    let summary_prompt = vec![
        Message {
            role: MessageRole::System,
            content: format!(
                "Summarize this conversation {summary_lang} as a structured context \
                checkpoint for an assistant that will continue the work. Use EXACTLY \
                these Markdown sections, in order:\n\
                ## Objective\n[What the user is ultimately trying to accomplish]\n\
                ## Work State\n[Completed: … / Active: … / Blocked: … — include exact error messages]\n\
                ## Key Decisions\n[Important technical decisions and WHY]\n\
                ## Open Questions\n[Unanswered questions or blockers; write \"None.\" if none]\n\
                ## Next Move\n[Concrete next steps, framed as context not instructions]\n\n\
                Preserve exact identifiers: UUIDs, URLs, file paths, IPs, hostnames, port \
                numbers, line numbers. NEVER include API keys, tokens, or secrets — write \
                [REDACTED]. Be concrete and terse; omit a section's bullets only if truly empty."
            ),
            tool_calls: None,
            tool_call_id: None,
            thinking_blocks: vec![],
            db_id: None,

        },
        Message {
            role: MessageRole::User,
            content: formatted,
            tool_calls: None,
            tool_call_id: None,
            thinking_blocks: vec![],
            db_id: None,

        },
    ];

    // G3: same shared deadline — a slow summary call also fails open. Still
    // before any mutation of `messages`, so returning Ok(None) leaves history
    // untouched.
    let summary_response = match tokio::time::timeout_at(
        compaction_deadline,
        active_provider.chat(&summary_prompt, &empty_tools, crate::agent::providers::CallOptions::default()),
    )
    .await
    {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "compaction LLM call (summary) failed — proceeding uncompacted (fail-open)");
            return Ok(None);
        }
        Err(_) => {
            tracing::warn!(
                budget_secs = COMPACTION_BUDGET.as_secs(),
                "compaction budget exceeded (summary) — proceeding uncompacted (fail-open)"
            );
            return Ok(None);
        }
    };

    // Step 3: Rebuild messages — system + summary + preserved recent
    let preserved: Vec<Message> = messages[end..].to_vec();
    messages.clear();

    if let Some(sys) = system_msg {
        messages.push(sys);
    }

    let summary_text = strip_thinking(&summary_response.content);
    if !summary_text.trim().is_empty() {
        messages.push(Message {
            role: MessageRole::System,
            content: format!("[Previous conversation summary]\n{}", summary_text),
            tool_calls: None,
            tool_call_id: None,
            thinking_blocks: vec![],
            db_id: None,

        });
    }

    messages.extend(preserved);

    tracing::info!(
        new_messages = messages.len(),
        new_tokens = estimate_tokens(messages),
        "compaction complete"
    );

    if extracted_facts.is_empty() {
        Ok(None)
    } else {
        Ok(Some(extracted_facts))
    }
}

/// Generate a compact progress header that survives compaction.
/// Includes iteration count, top-5 tools by call count, and loop warning if active.
/// The header is marked with `[Session Progress]` so subsequent compactions can
/// replace it rather than accumulate multiple headers.
pub fn generate_progress_header(messages: &[Message], detector: &LoopDetector) -> String {
    let iterations = detector.iteration_count();
    let tool_counts = detector.tool_counts();

    // Sort tools by count descending, take top 5
    let mut sorted: Vec<(&String, &usize)> = tool_counts.iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(a.1));
    let top_tools: Vec<String> = sorted.iter()
        .take(5)
        .map(|(name, count)| format!("{name}({count})"))
        .collect();

    // Count tool role messages that contain "error"
    let error_count = messages.iter()
        .filter(|m| m.role == MessageRole::Tool && m.content.to_lowercase().contains("error"))
        .count();

    let tools_str = if top_tools.is_empty() {
        "none".to_string()
    } else {
        top_tools.join(", ")
    };

    let mut header = format!(
        "[Session Progress] Iterations: {iterations}. Tools called: {tools_str}."
    );

    if error_count > 0 {
        header.push_str(&format!(" Errors encountered: {error_count} tool failures."));
    }

    header
}

/// Remove any existing `[Session Progress]` system message from the messages list.
/// Used before injecting a fresh progress header after compaction.
pub fn remove_progress_header(messages: &mut Vec<Message>) {
    messages.retain(|m| {
        !(m.role == MessageRole::Tool
            && m.content.starts_with("[Session Progress]")
            || m.role == MessageRole::System
                && m.content.starts_with("[Session Progress]"))
    });
}

/// Phase 1 pre-pass: replace old tool result contents with 1-line summaries,
/// deduplicate identical results. `protect_tail` is the number of messages from
/// the end that are never pruned (matches `preserve_last_n` from config).
pub fn prune_old_tool_results(messages: &[Message], protect_tail: usize) -> Vec<Message> {
    if messages.is_empty() {
        return Vec::new();
    }
    let mut result: Vec<Message> = messages.to_vec();
    let prune_end = result.len().saturating_sub(protect_tail);

    // Pass 1: deduplicate — keep newest full copy, replace older dups
    use std::collections::{HashMap, HashSet};
    let mut content_hashes: HashMap<u64, usize> = HashMap::new();
    // Track indices that are the canonical (newest) copy of a dup group — these are
    // exempt from Pass 2 size-based pruning so callers can see the surviving content.
    let mut canonical_dup_indices: HashSet<usize> = HashSet::new();
    for i in (0..result.len()).rev() {
        if result[i].role != MessageRole::Tool { continue; }
        let content = &result[i].content;
        if content.len() < 200 { continue; }
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        content.hash(&mut hasher);
        let h = hasher.finish();
        if let Some(&newer_idx) = content_hashes.get(&h) {
            if i < newer_idx && i < prune_end {
                result[i].content =
                    "[Duplicate tool output — same content as a more recent call]".into();
                canonical_dup_indices.insert(newer_idx);
            }
        } else {
            content_hashes.insert(h, i);
        }
    }

    // Pass 2: replace large tool results outside protected tail with 1-line summary.
    // Skip canonical dup indices — their content must survive intact so callers can
    // verify deduplication worked correctly.
    for (i, msg) in result.iter_mut().enumerate().take(prune_end) {
        if canonical_dup_indices.contains(&i) { continue; }
        if msg.role != MessageRole::Tool { continue; }
        if msg.content.len() <= 200 { continue; }
        if msg.content.starts_with('[') { continue; }
        let tool_call_id = msg
            .tool_call_id
            .as_ref()
            .map(|id| id.as_str())
            .unwrap_or("");
        let char_count = msg.content.len();
        msg.content = format!("[tool result {tool_call_id}] ({char_count} chars — pruned)");
    }

    result
}

/// Phase 2: find where the compressed middle begins.
/// Starts at `protect_first_n`, then slides forward past any leading Tool messages.
pub fn find_head_end(messages: &[Message], protect_first_n: usize) -> usize {
    let mut idx = protect_first_n.min(messages.len());
    while idx < messages.len() && messages[idx].role == MessageRole::Tool {
        idx += 1;
    }
    idx
}

/// Phase 2: find where the protected tail begins, using a token budget.
pub fn find_tail_start_by_tokens(messages: &[Message], head_end: usize, tail_budget: usize) -> usize {
    let n = messages.len();
    if n <= head_end + 1 {
        return n;
    }
    let min_tail = 3.min(n.saturating_sub(head_end).saturating_sub(1));
    let soft_ceiling = (tail_budget as f64 * 1.5) as usize;
    let mut accumulated: usize = 0;
    let mut cut_idx = n;

    for i in (head_end..n).rev() {
        let msg_tokens = messages[i].content.len() / 4 + 10;
        if accumulated + msg_tokens > soft_ceiling && n - i >= min_tail {
            break;
        }
        accumulated += msg_tokens;
        cut_idx = i;
    }

    cut_idx = cut_idx.min(n.saturating_sub(min_tail));

    // Invariant: most recent User message must be in the tail.
    // Skip previously-inserted summary markers (role=User, content starts with
    // SUMMARY_PREFIX) — on a second/later compaction they would otherwise be
    // mistaken for "the last user message" and the anchor would stick to the
    // banner instead of the real last user turn (hermes parity, T12 pt.6).
    let last_user_idx = messages[head_end..n]
        .iter()
        .rposition(|m| m.role == MessageRole::User && !m.content.starts_with(SUMMARY_PREFIX))
        .map(|rel| rel + head_end);
    if let Some(user_idx) = last_user_idx
        && user_idx < cut_idx {
            cut_idx = user_idx;
        }

    // Align backward past tool groups
    while cut_idx > head_end && messages[cut_idx].role == MessageRole::Tool {
        cut_idx = cut_idx.saturating_sub(1);
    }
    if cut_idx > head_end
        && messages[cut_idx].role == MessageRole::Assistant
        && messages[cut_idx].tool_calls.is_some()
    {
        cut_idx = cut_idx.saturating_sub(1);
    }

    // Causal-coupling guard (hermes parity, T12 pt.3): the plain
    // `max(head_end + 1)` clamp below exists to guarantee a non-empty tail
    // when `cut_idx` fell back to (or below) `head_end`. But if the last user
    // turn sits exactly at `head_end`, blindly bumping the boundary to
    // `head_end + 1` pushes it one slot past the User message while keeping
    // its Assistant/Tool response — splitting the turn-pair and leaving the
    // user message alone in the compressed region without its answer. When
    // that collision happens, keep the boundary AT `head_end` instead: the
    // tail (`messages[tail_start..]`) then naturally contains the whole
    // turn-pair (User -> Assistant [-> Tool*]), not just the response.
    if let Some(user_idx) = last_user_idx
        && user_idx == head_end
        && cut_idx <= head_end {
            return head_end;
        }

    cut_idx.max(head_end + 1)
}

/// Phase 5: fix orphaned tool_call / tool_result pairs after compression.
pub fn sanitize_tool_pairs(messages: Vec<Message>) -> Vec<Message> {
    use opex_types::ids::ToolCallId;
    use std::collections::HashSet;

    let surviving_call_ids: HashSet<ToolCallId> = messages
        .iter()
        .filter(|m| m.role == MessageRole::Assistant)
        .flat_map(|m| m.tool_calls.iter().flatten())
        .map(|tc| tc.id.clone())
        .collect();

    let result_call_ids: HashSet<ToolCallId> = messages
        .iter()
        .filter(|m| m.role == MessageRole::Tool)
        .filter_map(|m| m.tool_call_id.clone())
        .collect();

    // 1. Remove orphaned tool results
    let messages: Vec<Message> = messages
        .into_iter()
        .filter(|m| {
            if m.role == MessageRole::Tool {
                m.tool_call_id
                    .as_ref()
                    .map(|id| surviving_call_ids.contains(id))
                    .unwrap_or(false)
            } else {
                true
            }
        })
        .collect();

    // 2. Insert stubs for orphaned calls
    let missing_ids: HashSet<ToolCallId> = surviving_call_ids
        .difference(&result_call_ids)
        .cloned()
        .collect();

    if missing_ids.is_empty() {
        return messages;
    }

    let mut patched: Vec<Message> = Vec::with_capacity(messages.len() + missing_ids.len());
    for msg in messages {
        if msg.role == MessageRole::Assistant {
            let needs_stubs: Vec<ToolCallId> = msg
                .tool_calls
                .iter()
                .flatten()
                .filter(|tc| missing_ids.contains(&tc.id))
                .map(|tc| tc.id.clone())
                .collect();
            patched.push(msg);
            for call_id in needs_stubs {
                patched.push(Message {
                    role: MessageRole::Tool,
                    content: "[Result from earlier conversation — see context summary above]"
                        .into(),
                    tool_call_id: Some(call_id),
                    tool_calls: None,
                    thinking_blocks: vec![],
            db_id: None,

                });
            }
        } else {
            patched.push(msg);
        }
    }
    patched
}

/// Format messages for compaction prompt.
fn format_messages_for_compaction(messages: &[Message]) -> String {
    let mut formatted = String::new();
    for msg in messages {
        let role = match msg.role {
            MessageRole::User => "User",
            MessageRole::Assistant => "Assistant",
            MessageRole::System => "System",
            MessageRole::Tool => "Tool",
        };
        formatted.push_str(&format!("[{}]: {}\n\n", role, msg.content));
    }
    formatted
}

/// Phase 3: generate a structured 13-section Hermes-style summary via LLM.
/// When `previous_summary` is Some, generates an iterative update.
/// Returns None on LLM failure — caller should use static fallback.
pub async fn generate_hermes_summary(
    turns: &[Message],
    provider: &dyn LlmProvider,
    language: Option<&str>,
    previous_summary: Option<&str>,
) -> Option<String> {
    let content_tokens = estimate_tokens(turns);
    let budget = ((content_tokens as f64 * SUMMARY_RATIO) as usize)
        .clamp(MIN_SUMMARY_TOKENS, SUMMARY_TOKENS_CEILING);

    let lang_instruction = match language {
        Some("en") => "Write the summary in English.",
        _ => "Write the summary in Russian.",
    };

    let template = format!(r#"## Active Task
[THE SINGLE MOST IMPORTANT FIELD. Copy the user's most recent request verbatim. If no outstanding task, write "None."]

## Goal
[What the user is trying to accomplish overall]

## Constraints & Preferences
[User preferences, coding style, important constraints]

## Completed Actions
[Numbered list: N. ACTION target — outcome [tool: name]]

## Active State
[Working directory, branch, modified files, test status, running processes]

## In Progress
[Work underway when compaction fired]

## Blocked
[Blockers, errors, issues not resolved — include exact error messages]

## Key Decisions
[Important technical decisions and WHY]

## Resolved Questions
[Questions already answered — include the answer]

## Pending User Asks
[Questions or requests not yet answered — if none, write "None."]

## Relevant Files
[Files read, modified, created — with brief note]

## Remaining Work
[What remains to be done — framed as context, not instructions]

## Critical Context
[Specific values, error messages, config details. NEVER include API keys — write [REDACTED].]

Target ~{budget} tokens. Be CONCRETE. Include file paths, commands, error messages, line numbers."#);

    let preamble = format!(
        "You are a summarization agent creating a context checkpoint. \
Your output will be injected as reference material for a DIFFERENT assistant \
that continues the conversation. Do NOT respond to any questions or requests \
in the conversation — only output the structured summary. \
Do NOT include any preamble, greeting, or prefix. \
{lang_instruction} \
NEVER include API keys, tokens, passwords, or secrets — write [REDACTED] instead."
    );

    let prompt_content = if let Some(prev) = previous_summary {
        let turns_text = format_messages_for_compaction(turns);
        format!(
            "{preamble}\n\nYou are UPDATING a context compaction summary. \
A previous compaction produced the summary below. New turns have occurred since then.\n\n\
PREVIOUS SUMMARY:\n{prev}\n\nNEW TURNS TO INCORPORATE:\n{turns_text}\n\n\
Update the summary using the structure below. PRESERVE all existing relevant info. \
ADD new completed actions (continue numbering). Move answered questions to Resolved. \
Update Active Task to the user's most recent unfulfilled request.\n\n{template}"
        )
    } else {
        let turns_text = format_messages_for_compaction(turns);
        format!(
            "{preamble}\n\nCreate a structured handoff summary for a different assistant \
that will continue this conversation.\n\nTURNS TO SUMMARIZE:\n{turns_text}\n\n{template}"
        )
    };

    let prompt = vec![Message {
        role: MessageRole::User,
        content: prompt_content,
        tool_calls: None,
        tool_call_id: None,
        thinking_blocks: vec![],
            db_id: None,

    }];

    let empty_tools: Vec<ToolDefinition> = vec![];
    match provider
        .chat(&prompt, &empty_tools, crate::agent::providers::CallOptions::default())
        .await
    {
        Ok(response) => {
            let summary = response.content.trim().to_string();
            if summary.is_empty() {
                None
            } else {
                Some(format!("{SUMMARY_PREFIX}\n{summary}"))
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "failed to generate context summary");
            None
        }
    }
}

/// Extract facts from a conversation slice into memory-ready strings.
/// Called in parallel with summary generation during Phase 3.
/// Returns empty Vec on failure (non-fatal).
async fn extract_facts_only(
    turns: &[Message],
    provider: &dyn LlmProvider,
    language: Option<&str>,
) -> Vec<String> {
    let lang_hint = match language {
        Some("ru") => " Write each fact in Russian.",
        Some("en") => " Write each fact in English.",
        _ => "",
    };
    let formatted = format_messages_for_compaction(turns);
    let extraction_prompt = vec![
        Message {
            role: MessageRole::System,
            content: format!(
                "Extract key facts from this conversation as a JSON array of strings.\n\
MUST PRESERVE: active tasks with progress, UUIDs/URLs/file paths/IPs, decisions \
and rationale, user preferences, error conditions and resolutions, commitments.\n\
MAY OMIT: routine greetings, tool calls without noteworthy results, repeated info.\n\
Each fact must be self-contained.{lang_hint}\n\
Return ONLY the JSON array, no other text."
            ),
            tool_calls: None,
            tool_call_id: None,
            thinking_blocks: vec![],
            db_id: None,

        },
        Message {
            role: MessageRole::User,
            content: formatted,
            tool_calls: None,
            tool_call_id: None,
            thinking_blocks: vec![],
            db_id: None,

        },
    ];
    let empty_tools: Vec<ToolDefinition> = vec![];
    match provider
        .chat(
            &extraction_prompt,
            &empty_tools,
            crate::agent::providers::CallOptions::default(),
        )
        .await
    {
        Ok(resp) => serde_json::from_str::<Vec<String>>(&resp.content).unwrap_or_default(),
        Err(e) => {
            tracing::warn!(error = %e, "fact extraction failed, skipping");
            vec![]
        }
    }
}

/// Build the initial message list for a chain child session.
///
/// Returns `[system_msg (if present), summary_as_assistant, ...tail_msgs]`.
/// The summary role is always `MessageRole::Assistant` — since the head is
/// at most a `system` message, the first conversation message must be `assistant`
/// to satisfy the alternating-roles invariant expected by LLM providers.
#[cfg(test)]
pub fn build_compressed_seed(
    system_msg: Option<&Message>,
    summary: &str,
    tail: &[Message],
) -> Vec<Message> {
    let mut result = Vec::new();

    if let Some(sys) = system_msg {
        result.push(sys.clone());
    }

    let summary_content = if summary.is_empty() {
        format!(
            "{SUMMARY_PREFIX}\nSummary generation was unavailable. \
Context was compacted to free space. Continue based on the recent messages below."
        )
    } else if summary.starts_with(SUMMARY_PREFIX) {
        summary.to_string()
    } else {
        format!("{SUMMARY_PREFIX}\n{summary}")
    };

    result.push(Message {
        role: MessageRole::Assistant,
        content: summary_content,
        tool_calls: None,
        tool_call_id: None,
        thinking_blocks: vec![],
            db_id: None,

    });

    result.extend_from_slice(tail);
    result
}

/// Main entry point: run all 5 phases of compression on `messages`.
/// `language` should be the agent's language (e.g. "ru" or "en"), used for summary.
/// Returns extracted facts (empty Vec if `cfg.extract_to_memory = false`).
///
/// `db` + `session_id` are used to persist the compression boundary to the DB
/// (mark messages compressed, insert session_timeline record). Failures are
/// non-fatal: logged as warnings, compression continues in memory regardless.
pub async fn compress_messages(
    messages: &mut Vec<Message>,
    compressor: &mut crate::agent::compressor::Compressor,
    cfg: &crate::config::CompactionConfig,
    provider: &dyn LlmProvider,
    language: Option<&str>,
    db: &sqlx::PgPool,
    session_id: uuid::Uuid,
) -> anyhow::Result<Vec<String>> {
    let tokens_before = estimate_tokens(messages) as u32;

    // Phase 1: pre-pass — prune + deduplicate tool results
    let pruned = prune_old_tool_results(messages, cfg.preserve_last_n as usize);

    // Phase 2: boundaries
    let head_end = find_head_end(&pruned, cfg.protect_first_n);
    let tail_budget = (compressor.context_limit as f64
        * cfg.threshold
        * cfg.summary_target_ratio) as usize;
    let tail_start = find_tail_start_by_tokens(&pruned, head_end, tail_budget);

    if head_end >= tail_start {
        tracing::debug!(
            head_end,
            tail_start,
            "compression skipped: nothing to summarise"
        );
        return Ok(vec![]);
    }

    // Collect DB IDs from the middle range before they're dropped.
    // S2 T5: `Message.db_id` is now `Option<MessageId>`. Convert to bare
    // `Uuid` for the DB layer (which still takes `&[Uuid]` / `Option<Uuid>`
    // — those are persistence-plumbing helpers, not identity surfaces).
    let compressed_ids: Vec<uuid::Uuid> = pruned[head_end..tail_start]
        .iter()
        .filter_map(|m| m.db_id.map(|id| id.as_uuid()))
        .collect();
    let first_compressed_id = pruned[head_end..tail_start]
        .first()
        .and_then(|m| m.db_id.map(|id| id.as_uuid()));
    let first_live_id = pruned
        .get(tail_start)
        .and_then(|m| m.db_id.map(|id| id.as_uuid()));
    let segment_index = compressor.compression_count;

    let turns_to_summarize: Vec<Message> = pruned[head_end..tail_start].to_vec();
    let tail: Vec<Message> = pruned[tail_start..].to_vec();
    let head: Vec<Message> = pruned[..head_end].to_vec();

    // Phase 3: LLM summary + fact extraction — parallel on read-only snapshot
    let previous = compressor.previous_summary.as_deref();
    let (summary, facts) = tokio::join!(
        generate_hermes_summary(&turns_to_summarize, provider, language, previous),
        async {
            if cfg.extract_to_memory {
                extract_facts_only(&turns_to_summarize, provider, language).await
            } else {
                vec![]
            }
        }
    );

    // Fallback if LLM failed
    let summary_text = summary.unwrap_or_else(|| {
        let n = turns_to_summarize.len();
        tracing::warn!(n, "summary generation failed — inserting static fallback");
        format!(
            "{SUMMARY_PREFIX}\nSummary generation was unavailable. \
{n} message(s) were removed to free context space. \
Continue based on the recent messages below."
        )
    });

    // Phase 4: assemble head + summary message + tail
    //
    // hermes parity (T12 pt.5, `_force_user_leading` guard): if `head` is
    // empty or ends in (or consists solely of) a System message, the summary
    // message becomes the FIRST non-system message sent to the provider.
    // Anthropic's Messages API requires `messages[0].role == "user"` — a
    // leading `assistant` message is rejected with HTTP 400. Pin
    // `summary_role = User` in that case and mark it as forced so the
    // role-collision flip below cannot revert it back to `Assistant`.
    let head_is_system_only = head.iter().all(|m| m.role == MessageRole::System);
    let force_user_leading = head.is_empty() || head_is_system_only;
    let summary_role = if force_user_leading
        || head
            .last()
            .map(|m| m.role == MessageRole::Assistant || m.role == MessageRole::Tool)
            .unwrap_or(false)
    {
        MessageRole::User
    } else {
        MessageRole::Assistant
    };

    let mut assembled: Vec<Message> = Vec::with_capacity(head.len() + 1 + tail.len());

    for (i, mut msg) in head.into_iter().enumerate() {
        if i == 0
            && msg.role == MessageRole::System
            && !msg.content.contains(SUMMARY_NOTE_FOR_SYSTEM)
        {
            msg.content.push_str(&format!("\n\n{SUMMARY_NOTE_FOR_SYSTEM}"));
        }
        assembled.push(msg);
    }

    // Check for role collision with tail
    let first_tail_role = tail.first().map(|m| m.role.clone());
    let merge_into_tail = first_tail_role.as_ref() == Some(&summary_role)
        && assembled
            .last()
            .map(|m| m.role.clone())
            .as_ref()
            == Some(&if summary_role == MessageRole::User {
                MessageRole::Assistant
            } else {
                MessageRole::User
            });

    if !merge_into_tail {
        // hermes parity (T12 pt.5): when the summary role was forced to
        // `User` because head is system-only (or empty), never flip it back
        // to `Assistant` to resolve a role collision with the tail — that
        // would recreate the exact bug this guard exists to prevent (an
        // `assistant`-first `messages[]` sent to Anthropic). Instead prefer
        // merging into the tail (handled by `merge_into_tail` above); if we
        // reach here with a collision, keep `User` and rely on downstream
        // role-alternation being provider-tolerant (Anthropic only rejects
        // a non-`user` FIRST message, not adjacent same-role messages).
        let role = if force_user_leading {
            summary_role.clone()
        } else if first_tail_role.as_ref() == Some(&summary_role) {
            if summary_role == MessageRole::User {
                MessageRole::Assistant
            } else {
                MessageRole::User
            }
        } else {
            summary_role
        };
        assembled.push(Message {
            role,
            content: summary_text.clone(),
            tool_calls: None,
            tool_call_id: None,
            thinking_blocks: vec![],
            db_id: None,

        });
        assembled.extend(tail);
    } else {
        let mut tail_iter = tail.into_iter();
        if let Some(mut first_tail) = tail_iter.next() {
            first_tail.content = format!(
                "{summary_text}\n\n--- END OF CONTEXT SUMMARY — respond to the message below ---\n\n{}",
                first_tail.content
            );
            assembled.push(first_tail);
        }
        assembled.extend(tail_iter);
    }

    // Phase 5: sanitize tool pairs
    let assembled = sanitize_tool_pairs(assembled);

    // Commit
    *messages = assembled;
    compressor.previous_summary = Some(summary_text.clone());

    let tokens_after = estimate_tokens(messages) as u32;
    compressor.record_compression_result(tokens_before, tokens_after, cfg);

    // Persist compression boundary to DB (best-effort, non-fatal).
    if !compressed_ids.is_empty() {
        if let Err(e) = crate::db::sessions::mark_messages_compressed(db, &compressed_ids).await {
            tracing::warn!(error = %e, count = compressed_ids.len(), "failed to mark messages as compressed");
        }
        if let Err(e) = crate::db::sessions::insert_compression_event(
            db,
            session_id,
            segment_index,
            &summary_text,
            first_compressed_id,
            first_live_id,
            tokens_before as i64,
            tokens_after as i64,
        ).await {
            tracing::warn!(error = %e, "failed to insert compression timeline event");
        }
    }

    tracing::info!(
        tokens_before,
        tokens_after,
        msgs_after = messages.len(),
        compression_count = compressor.compression_count,
        "compress_messages complete"
    );

    Ok(facts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use opex_types::{LlmResponse, Message, MessageRole, ToolCall, ToolDefinition};
    use tokio::sync::mpsc;
    use crate::agent::tool_loop::{LoopDetector, ToolLoopConfig};
    use crate::agent::providers::LlmProvider;

    // ── build_compressed_seed tests ───────────────────────────────────────────

    #[test]
    fn build_compressed_seed_correct_order_and_roles() {
        let system = Message {
            role: MessageRole::System,
            content: "You are a helpful assistant.".into(),
            tool_calls: None, tool_call_id: None, thinking_blocks: vec![],
            db_id: None,

        };
        let tail = vec![
            Message { role: MessageRole::User,      content: "what is 2+2".into(), tool_calls: None, tool_call_id: None, thinking_blocks: vec![], db_id: None },
            Message { role: MessageRole::Assistant, content: "4".into(),            tool_calls: None, tool_call_id: None, thinking_blocks: vec![], db_id: None },
        ];
        let seed = build_compressed_seed(Some(&system), "my summary", &tail);
        assert_eq!(seed.len(), 4, "system + summary + 2 tail");
        assert_eq!(seed[0].role, MessageRole::System);
        assert_eq!(seed[1].role, MessageRole::Assistant);
        assert!(seed[1].content.contains("my summary"), "summary must be in content");
        assert!(seed[1].content.contains(SUMMARY_PREFIX), "SUMMARY_PREFIX must be prepended");
        assert_eq!(seed[2].role, MessageRole::User);
        assert_eq!(seed[3].role, MessageRole::Assistant);
    }

    #[test]
    fn build_compressed_seed_no_system_message() {
        let tail = vec![
            Message { role: MessageRole::User, content: "hi".into(), tool_calls: None, tool_call_id: None, thinking_blocks: vec![], db_id: None },
        ];
        let seed = build_compressed_seed(None, "summary text", &tail);
        assert_eq!(seed.len(), 2, "summary + 1 tail (no system)");
        assert_eq!(seed[0].role, MessageRole::Assistant);
        assert!(seed[0].content.contains("summary text"));
        assert_eq!(seed[1].role, MessageRole::User);
    }

    #[test]
    fn build_compressed_seed_empty_summary_uses_fallback() {
        let seed = build_compressed_seed(None, "", &[]);
        assert_eq!(seed.len(), 1, "only fallback summary message");
        assert!(seed[0].content.contains(SUMMARY_PREFIX));
        assert!(seed[0].content.contains("unavailable"), "fallback must mention unavailability");
    }

    struct StaticProvider(String);

    #[async_trait]
    impl LlmProvider for StaticProvider {
        async fn chat(&self, _msgs: &[Message], _tools: &[ToolDefinition], _opts: crate::agent::providers::CallOptions) -> anyhow::Result<LlmResponse> {
            Ok(LlmResponse {
                content: self.0.clone(),
                tool_calls: vec![],
                usage: None,
                finish_reason: None,
                model: None,
                provider: None,
                fallback_notice: None,
                tools_used: vec![],
                iterations: 0,
                thinking_blocks: vec![],
            })
        }
        async fn chat_stream(&self, msgs: &[Message], tools: &[ToolDefinition], _tx: mpsc::Sender<String>, _opts: crate::agent::providers::CallOptions) -> anyhow::Result<LlmResponse> {
            self.chat(msgs, tools, _opts).await
        }
        fn name(&self) -> &str { "static" }
    }

    fn make_message(role: MessageRole, content: &str) -> Message {
        Message {
            role,
            content: content.to_string(),
            tool_calls: None,
            tool_call_id: None,
            thinking_blocks: vec![],
            db_id: None,

        }
    }

    #[test]
    fn estimate_tokens_empty_slice() {
        assert_eq!(estimate_tokens(&[]), 0);
    }

    #[test]
    fn estimate_tokens_single_user_message() {
        let msg = make_message(MessageRole::User, "hello");
        // "hello".len() = 5, 5 / 4 + 10 = 11
        assert_eq!(estimate_tokens(&[msg]), 11);
    }

    #[test]
    fn estimate_tokens_multiple_messages() {
        let msgs = vec![
            make_message(MessageRole::User, "hello"),       // 5/4+10 = 11
            make_message(MessageRole::Assistant, "world!!"), // 7/4+10 = 11
            make_message(MessageRole::System, ""),           // 0/4+10 = 10
        ];
        let individual: usize = msgs.iter().map(|m| estimate_tokens(std::slice::from_ref(m))).sum();
        assert_eq!(estimate_tokens(&msgs), individual);
        assert_eq!(estimate_tokens(&msgs), 11 + 11 + 10);
    }

    #[test]
    fn estimate_tokens_with_tool_calls() {
        let tool_call = ToolCall {
            id: "call_1".into(),
            name: "get_weather".to_string(),
            arguments: serde_json::json!({"city": "Moscow"}),
            thought_signature: None,
        };
        let msg = Message {
            role: MessageRole::Assistant,
            content: "".to_string(),
            tool_calls: Some(vec![tool_call.clone()]),
            tool_call_id: None,
            thinking_blocks: vec![],
            db_id: None,

        };

        let content_tokens = 10; // 0 / 4 + 10
        let args_str = tool_call.arguments.to_string();
        let tool_tokens = (args_str.len() + tool_call.name.len() + 20) / 4;
        let expected = content_tokens + tool_tokens;

        assert_eq!(estimate_tokens(&[msg]), expected);
        assert!(expected > 10, "tool calls should add tokens beyond content");
    }

    #[test]
    fn format_messages_mixed_roles() {
        let msgs = vec![
            make_message(MessageRole::User, "hi"),
            make_message(MessageRole::Assistant, "hello"),
            make_message(MessageRole::System, "you are helpful"),
            make_message(MessageRole::Tool, "result: 42"),
        ];

        let formatted = format_messages_for_compaction(&msgs);

        assert_eq!(
            formatted,
            "[User]: hi\n\n[Assistant]: hello\n\n[System]: you are helpful\n\n[Tool]: result: 42\n\n"
        );
    }

    #[test]
    fn format_messages_empty_content() {
        let msgs = vec![
            make_message(MessageRole::User, ""),
            make_message(MessageRole::Assistant, ""),
        ];

        let formatted = format_messages_for_compaction(&msgs);

        assert_eq!(formatted, "[User]: \n\n[Assistant]: \n\n");
    }

    // ── progress_header tests ─────────────────────────────────────────────────

    fn make_detector_with_counts(counts: &[(&str, usize)]) -> LoopDetector {
        let cfg = ToolLoopConfig::default();
        let mut det = LoopDetector::new(&cfg);
        for (tool, n) in counts {
            for _ in 0..*n {
                det.record_execution(tool, &serde_json::json!({}), true);
            }
        }
        det
    }

    #[test]
    fn progress_header_contains_tools() {
        let det = make_detector_with_counts(&[("search", 5), ("read", 3), ("write", 1)]);
        let msgs = vec![];
        let header = generate_progress_header(&msgs, &det);
        assert!(header.starts_with("[Session Progress]"), "header must start with sentinel");
        assert!(header.contains("search(5)"), "must list search with count 5");
        assert!(header.contains("read(3)"), "must list read with count 3");
        assert!(header.contains("write(1)"), "must list write with count 1");
        assert!(header.contains("Iterations: 9"), "must show total iteration count");
    }

    #[test]
    fn progress_header_with_error_messages() {
        let det = make_detector_with_counts(&[("search", 2)]);
        let msgs = vec![
            make_message(MessageRole::Tool, "error: file not found"),
            make_message(MessageRole::Tool, "success result"),
            make_message(MessageRole::Tool, "Error: timeout"),
        ];
        let header = generate_progress_header(&msgs, &det);
        assert!(header.contains("Errors encountered: 2"), "should count 2 error tool messages");
    }

    #[test]
    fn progress_header_no_tools() {
        let cfg = ToolLoopConfig::default();
        let det = LoopDetector::new(&cfg);
        let msgs = vec![];
        let header = generate_progress_header(&msgs, &det);
        assert!(header.starts_with("[Session Progress]"), "must have sentinel even with no tools");
        assert!(header.contains("none"), "should say 'none' when no tools called");
    }

    #[test]
    fn progress_header_top_5_tools() {
        let det = make_detector_with_counts(&[
            ("a", 10), ("b", 9), ("c", 8), ("d", 7), ("e", 6), ("f", 5),
        ]);
        let msgs = vec![];
        let header = generate_progress_header(&msgs, &det);
        // Should include top 5, not the 6th
        assert!(header.contains("a(10)"));
        assert!(header.contains("b(9)"));
        assert!(header.contains("e(6)"));
        // f(5) is 6th — should NOT appear
        assert!(!header.contains("f(5)"), "should not include 6th tool beyond top 5");
    }

    #[test]
    fn remove_progress_header_clears_existing() {
        let mut msgs = vec![
            make_message(MessageRole::System, "You are an assistant."),
            make_message(MessageRole::System, "[Session Progress] Iterations: 5. Tools called: search(5)."),
            make_message(MessageRole::User, "hello"),
        ];
        remove_progress_header(&mut msgs);
        assert_eq!(msgs.len(), 2, "progress header system message should be removed");
        assert!(!msgs.iter().any(|m| m.content.starts_with("[Session Progress]")));
    }

    // ── compact_if_needed: thinking-block summary regression ────────────────

    fn make_compactable_messages() -> Vec<Message> {
        // Build enough tokens to trigger compaction (>80% of a small threshold).
        // compact_if_needed threshold = max_tokens * 80/100.
        // We pass max_tokens=1 to force compaction regardless of actual size.
        vec![
            make_message(MessageRole::System, "You are an assistant."),
            make_message(MessageRole::User, "Hello"),
            make_message(MessageRole::Assistant, "Hi there"),
            make_message(MessageRole::User, "How are you"),
            make_message(MessageRole::Assistant, "I'm fine"),
            make_message(MessageRole::User, "What is 2+2"),
            make_message(MessageRole::Assistant, "4"),
        ]
    }

    #[tokio::test]
    async fn compact_thinking_only_summary_skips_summary_message() {
        // Regression: when a reasoning model returns only <think>…</think> as the compaction
        // summary (no visible text), strip_thinking would produce "" and a useless
        // "[Previous conversation summary]\n" message would be injected.
        // Now it should be skipped entirely.
        let provider = StaticProvider("<think>internal reasoning only</think>".to_string());
        let mut msgs = make_compactable_messages();
        compact_if_needed(&mut msgs, &provider, None, 1, 2, None).await.unwrap();

        let summary_msgs: Vec<_> = msgs.iter()
            .filter(|m| m.role == MessageRole::System && m.content.contains("[Previous conversation summary]"))
            .collect();
        assert!(
            summary_msgs.is_empty(),
            "thinking-only summary should be skipped, but got: {:?}",
            summary_msgs.iter().map(|m| &m.content).collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn compact_real_summary_is_injected_stripped_of_thinking() {
        // When the summary contains both thinking and visible text, only visible text is kept.
        let provider = StaticProvider(
            "<think>reasoning</think>Summary: user asked math questions.".to_string()
        );
        let mut msgs = make_compactable_messages();
        compact_if_needed(&mut msgs, &provider, None, 1, 2, None).await.unwrap();

        let summary_msg = msgs.iter().find(|m| {
            m.role == MessageRole::System && m.content.contains("[Previous conversation summary]")
        });
        assert!(summary_msg.is_some(), "real summary should be injected");
        let content = &summary_msg.unwrap().content;
        assert!(content.contains("Summary: user asked math questions."), "got: {content}");
        assert!(!content.contains("<think>"), "thinking tags must be stripped, got: {content}");
    }

    #[tokio::test]
    async fn compact_empty_summary_skips_summary_message() {
        let provider = StaticProvider(String::new());
        let mut msgs = make_compactable_messages();
        compact_if_needed(&mut msgs, &provider, None, 1, 2, None).await.unwrap();

        let has_summary = msgs.iter().any(|m| {
            m.role == MessageRole::System && m.content.contains("[Previous conversation summary]")
        });
        assert!(!has_summary, "empty summary should not inject a summary message");
    }

    // ── G3 (WS5): compaction is budgeted + fail-open ─────────────────────────
    //
    // A compaction provider that hangs longer than `COMPACTION_BUDGET` must NOT
    // stall or fail the turn: `compact_if_needed` returns `Ok(None)` (skip) and
    // leaves `messages` untouched. The reactive overflow-retry remains the net.
    struct SlowMockProvider(std::time::Duration);

    #[async_trait]
    impl LlmProvider for SlowMockProvider {
        async fn chat(&self, _msgs: &[Message], _tools: &[ToolDefinition], _opts: crate::agent::providers::CallOptions) -> anyhow::Result<LlmResponse> {
            tokio::time::sleep(self.0).await;
            Ok(LlmResponse {
                content: "should never be reached before the budget fires".to_string(),
                tool_calls: vec![],
                usage: None,
                finish_reason: None,
                model: None,
                provider: None,
                fallback_notice: None,
                tools_used: vec![],
                iterations: 0,
                thinking_blocks: vec![],
            })
        }
        async fn chat_stream(&self, msgs: &[Message], tools: &[ToolDefinition], _tx: mpsc::Sender<String>, _opts: crate::agent::providers::CallOptions) -> anyhow::Result<LlmResponse> {
            self.chat(msgs, tools, _opts).await
        }
        fn name(&self) -> &str { "slow-mock" }
    }

    #[tokio::test(start_paused = true)]
    async fn compaction_budget_exceeded_is_fail_open() {
        // chat() sleeps 120s — far beyond the 15s COMPACTION_BUDGET. With the
        // paused clock, tokio auto-advances time to fire the timeout.
        let slow = SlowMockProvider(std::time::Duration::from_secs(120));
        let mut messages = make_compactable_messages();
        let before = messages.clone();
        // max_tokens=1 forces the compaction gate open (threshold = 1*80/100 = 0).
        let result = compact_if_needed(&mut messages, &slow, None, 1, 2, None).await;
        assert!(
            matches!(result, Ok(None)),
            "budget overrun must SKIP (Ok(None)), not error — got {result:?}"
        );
        assert_eq!(
            messages.len(),
            before.len(),
            "messages must be untouched on a fail-open skip"
        );
        let contents: Vec<&String> = messages.iter().map(|m| &m.content).collect();
        let before_contents: Vec<&String> = before.iter().map(|m| &m.content).collect();
        assert_eq!(contents, before_contents, "messages content must be untouched on skip");
    }

    #[tokio::test(start_paused = true)]
    async fn compaction_provider_error_is_fail_open() {
        // A provider that errors (not just slow) must also fail open.
        struct ErrProvider;
        #[async_trait]
        impl LlmProvider for ErrProvider {
            async fn chat(&self, _m: &[Message], _t: &[ToolDefinition], _o: crate::agent::providers::CallOptions) -> anyhow::Result<LlmResponse> {
                anyhow::bail!("compaction provider is down")
            }
            async fn chat_stream(&self, m: &[Message], t: &[ToolDefinition], _tx: mpsc::Sender<String>, o: crate::agent::providers::CallOptions) -> anyhow::Result<LlmResponse> {
                self.chat(m, t, o).await
            }
            fn name(&self) -> &str { "err" }
        }
        let mut messages = make_compactable_messages();
        let before = messages.clone();
        let result = compact_if_needed(&mut messages, &ErrProvider, None, 1, 2, None).await;
        assert!(matches!(result, Ok(None)), "provider error must SKIP, not propagate — got {result:?}");
        let contents: Vec<&String> = messages.iter().map(|m| &m.content).collect();
        let before_contents: Vec<&String> = before.iter().map(|m| &m.content).collect();
        assert_eq!(contents, before_contents, "messages must be untouched on provider-error skip");
    }

    // ── prune_old_tool_results tests ─────────────────────────────────────────

    #[test]
    fn prune_deduplicates_identical_tool_results() {
        let dup_content = "x".repeat(300);
        let msgs = vec![
            Message { role: MessageRole::Tool, content: dup_content.clone(),
                      tool_call_id: Some("a".into()), tool_calls: None, thinking_blocks: vec![], db_id: None },
            Message { role: MessageRole::Tool, content: dup_content.clone(),
                      tool_call_id: Some("b".into()), tool_calls: None, thinking_blocks: vec![], db_id: None },
        ];
        let pruned = prune_old_tool_results(&msgs, 0);
        assert!(pruned[0].content.contains("Duplicate"));
        assert_eq!(pruned[1].content, dup_content);
    }

    #[test]
    fn prune_replaces_large_tool_result_with_summary_line() {
        let msgs = vec![
            Message { role: MessageRole::Tool, content: "a".repeat(300),
                      tool_call_id: Some("x".into()), tool_calls: None, thinking_blocks: vec![], db_id: None },
        ];
        let pruned = prune_old_tool_results(&msgs, 0);
        assert!(pruned[0].content.starts_with('['));
        assert!(pruned[0].content.len() < 300);
    }

    #[test]
    fn prune_skips_messages_in_protected_tail() {
        let content = "b".repeat(300);
        let msgs = vec![
            Message { role: MessageRole::Tool, content: content.clone(),
                      tool_call_id: Some("x".into()), tool_calls: None, thinking_blocks: vec![], db_id: None },
        ];
        let pruned = prune_old_tool_results(&msgs, 1);
        assert_eq!(pruned[0].content, content);
    }

    // ── find_tail_start_by_tokens / find_head_end tests ──────────────────────

    #[test]
    fn tail_cut_respects_token_budget() {
        let msgs: Vec<Message> = (0..10).map(|i| Message {
            role: if i % 2 == 0 { MessageRole::User } else { MessageRole::Assistant },
            content: "a".repeat(400),
            tool_calls: None, tool_call_id: None, thinking_blocks: vec![],
            db_id: None,

        }).collect();
        let tail_start = find_tail_start_by_tokens(&msgs, 0, 200);
        assert!(tail_start >= msgs.len() - 4);
        assert!(tail_start < msgs.len());
    }

    #[test]
    fn tail_cut_always_includes_last_user_message() {
        let mut msgs: Vec<Message> = (0..8).map(|_| Message {
            role: MessageRole::Assistant,
            content: "a".repeat(400),
            tool_calls: None, tool_call_id: None, thinking_blocks: vec![],
            db_id: None,

        }).collect();
        msgs[3].role = MessageRole::User;
        let tail_start = find_tail_start_by_tokens(&msgs, 0, 50);
        assert!(tail_start <= 3, "last user message must be in tail, tail_start={tail_start}");
    }

    /// T12 pt.3 — turn-pair preservation (hermes "Causal Coupling" parity).
    /// When the last user turn sits exactly at head_end, the tail boundary
    /// must not land between the user message and its assistant response —
    /// both must survive compaction together, or the agent re-does the task
    /// on the next turn.
    #[test]
    fn tail_cut_preserves_orphan_user_turn_pair() {
        let msgs = vec![
            Message { role: MessageRole::System,    content: "s".into(), tool_calls: None, tool_call_id: None, thinking_blocks: vec![], db_id: None },
            Message { role: MessageRole::Assistant, content: "a1".repeat(50), tool_calls: None, tool_call_id: None, thinking_blocks: vec![], db_id: None },
            Message { role: MessageRole::User,      content: "please do X".into(), tool_calls: None, tool_call_id: None, thinking_blocks: vec![], db_id: None },
            Message { role: MessageRole::Assistant, content: "done, X is complete".into(), tool_calls: None, tool_call_id: None, thinking_blocks: vec![], db_id: None },
        ];
        // protect_first_n=2 -> head_end=2 (messages[2] is the last User message).
        let head_end = find_head_end(&msgs, 2);
        assert_eq!(head_end, 2);
        // Tiny tail_budget so the token-budget loop wants to cut as tight as possible.
        let tail_start = find_tail_start_by_tokens(&msgs, head_end, 1);
        // tail_start == head_end (2) means messages[2..4] (User + its Assistant
        // response) both survive in the tail together. Anything > head_end
        // would split the pair and orphan the user turn (the bug being fixed).
        assert_eq!(
            tail_start, head_end,
            "user turn at head_end must stay in tail together with its response, tail_start={tail_start}"
        );
    }

    /// T12 pt.6 — a previously-inserted summary marker (role=User, content
    /// starting with SUMMARY_PREFIX) must never be picked as the "last user"
    /// anchor; the real last user message after it must win instead.
    #[test]
    fn tail_cut_anchor_skips_summary_marker() {
        let msgs = vec![
            Message { role: MessageRole::System,    content: "s".into(), tool_calls: None, tool_call_id: None, thinking_blocks: vec![], db_id: None },
            Message { role: MessageRole::User,      content: format!("{SUMMARY_PREFIX}\nEarlier stuff happened."), tool_calls: None, tool_call_id: None, thinking_blocks: vec![], db_id: None },
            Message { role: MessageRole::Assistant, content: "ack".repeat(200), tool_calls: None, tool_call_id: None, thinking_blocks: vec![], db_id: None },
            Message { role: MessageRole::User,      content: "real last user message".into(), tool_calls: None, tool_call_id: None, thinking_blocks: vec![], db_id: None },
            Message { role: MessageRole::Assistant, content: "real last response".into(), tool_calls: None, tool_call_id: None, thinking_blocks: vec![], db_id: None },
        ];
        let head_end = find_head_end(&msgs, 1);
        assert_eq!(head_end, 1);
        let tail_start = find_tail_start_by_tokens(&msgs, head_end, 50);
        assert!(
            tail_start <= 3,
            "anchor must point at the real last user message (idx 3), not the summary marker (idx 1); tail_start={tail_start}"
        );
    }

    #[test]
    fn head_end_skips_orphan_tool_results() {
        let msgs = vec![
            Message { role: MessageRole::System,    content: "s".into(), tool_calls: None, tool_call_id: None, thinking_blocks: vec![], db_id: None },
            Message { role: MessageRole::User,      content: "u".into(), tool_calls: None, tool_call_id: None, thinking_blocks: vec![], db_id: None },
            Message { role: MessageRole::Tool,      content: "t".into(), tool_call_id: Some("x".into()), tool_calls: None, thinking_blocks: vec![], db_id: None },
            Message { role: MessageRole::Assistant, content: "a".into(), tool_calls: None, tool_call_id: None, thinking_blocks: vec![], db_id: None },
        ];
        let head_end = find_head_end(&msgs, 2);
        assert_eq!(head_end, 3);
    }

    // ── sanitize_tool_pairs tests ─────────────────────────────────────────────

    #[test]
    fn sanitize_removes_orphaned_tool_results() {
        let msgs = vec![
            Message { role: MessageRole::Tool, content: "orphan".into(),
                      tool_call_id: Some("orphan_id".into()), tool_calls: None, thinking_blocks: vec![], db_id: None },
            Message { role: MessageRole::User, content: "hello".into(),
                      tool_calls: None, tool_call_id: None, thinking_blocks: vec![], db_id: None },
        ];
        let sanitized = sanitize_tool_pairs(msgs);
        assert_eq!(sanitized.len(), 1);
        assert_eq!(sanitized[0].role, MessageRole::User);
    }

    #[test]
    fn sanitize_adds_stub_for_orphaned_calls() {
        let msgs = vec![
            Message {
                role: MessageRole::Assistant,
                content: "".into(),
                tool_calls: Some(vec![ToolCall {
                    id: opex_types::ids::ToolCallId::from("tc_1"),
                    name: "workspace_read".into(),
                    arguments: serde_json::json!({}),
                    thought_signature: None,
                }]),
                tool_call_id: None,
                thinking_blocks: vec![],
            db_id: None,

            },
            Message { role: MessageRole::User, content: "next".into(),
                      tool_calls: None, tool_call_id: None, thinking_blocks: vec![], db_id: None },
        ];
        let sanitized = sanitize_tool_pairs(msgs);
        assert_eq!(sanitized.len(), 3);
        assert_eq!(sanitized[1].role, MessageRole::Tool);
        assert_eq!(
            sanitized[1].tool_call_id.as_ref().map(|id| id.as_str()),
            Some("tc_1")
        );
        assert!(sanitized[1].content.contains("earlier conversation"));
    }

    // ── generate_hermes_summary tests ──────────────────────────────────────────

    struct EchoProvider(String);

    #[async_trait]
    impl LlmProvider for EchoProvider {
        async fn chat(
            &self,
            _msgs: &[Message],
            _tools: &[ToolDefinition],
            _opts: crate::agent::providers::CallOptions,
        ) -> anyhow::Result<LlmResponse> {
            Ok(LlmResponse {
                content: self.0.clone(),
                tool_calls: vec![],
                usage: None,
                finish_reason: None,
                model: None,
                provider: None,
                fallback_notice: None,
                tools_used: vec![],
                iterations: 0,
                thinking_blocks: vec![],
            })
        }
        async fn chat_stream(
            &self,
            msgs: &[Message],
            tools: &[ToolDefinition],
            _tx: mpsc::Sender<String>,
            opts: crate::agent::providers::CallOptions,
        ) -> anyhow::Result<LlmResponse> {
            self.chat(msgs, tools, opts).await
        }
        fn name(&self) -> &str { "echo" }
    }

    struct FailProvider;

    #[async_trait]
    impl LlmProvider for FailProvider {
        async fn chat(
            &self,
            _msgs: &[Message],
            _tools: &[ToolDefinition],
            _opts: crate::agent::providers::CallOptions,
        ) -> anyhow::Result<LlmResponse> {
            anyhow::bail!("simulated LLM failure")
        }
        async fn chat_stream(
            &self,
            msgs: &[Message],
            tools: &[ToolDefinition],
            _tx: mpsc::Sender<String>,
            opts: crate::agent::providers::CallOptions,
        ) -> anyhow::Result<LlmResponse> {
            self.chat(msgs, tools, opts).await
        }
        fn name(&self) -> &str { "fail" }
    }

    #[tokio::test]
    async fn generate_hermes_summary_prepends_prefix() {
        let turns = vec![make_message(MessageRole::User, "hello")];
        let provider = EchoProvider("My summary body".into());
        let result = generate_hermes_summary(&turns, &provider, None, None).await;
        let text = result.unwrap();
        assert!(text.starts_with(SUMMARY_PREFIX), "must start with SUMMARY_PREFIX");
        assert!(text.contains("My summary body"));
    }

    #[tokio::test]
    // reviewed: floor_char_boundary-bounded slice in assert message — char boundary
    #[allow(clippy::string_slice)]
    async fn generate_hermes_summary_iterative_update_includes_previous() {
        struct PromptEchoProvider;

        #[async_trait]
        impl LlmProvider for PromptEchoProvider {
            async fn chat(
                &self,
                msgs: &[Message],
                _tools: &[ToolDefinition],
                _opts: crate::agent::providers::CallOptions,
            ) -> anyhow::Result<LlmResponse> {
                let content = msgs.iter()
                    .find(|m| m.role == MessageRole::User)
                    .map(|m| m.content.clone())
                    .unwrap_or_default();
                Ok(LlmResponse {
                    content,
                    tool_calls: vec![],
                    usage: None,
                    finish_reason: None,
                    model: None,
                    provider: None,
                    fallback_notice: None,
                    tools_used: vec![],
                    iterations: 0,
                    thinking_blocks: vec![],
                })
            }
            async fn chat_stream(
                &self,
                msgs: &[Message],
                tools: &[ToolDefinition],
                _tx: mpsc::Sender<String>,
                opts: crate::agent::providers::CallOptions,
            ) -> anyhow::Result<LlmResponse> {
                self.chat(msgs, tools, opts).await
            }
            fn name(&self) -> &str { "prompt-echo" }
        }

        let turns = vec![make_message(MessageRole::User, "new turn")];
        let provider = PromptEchoProvider;
        let prev = "PREVIOUS SUMMARY CONTENT";
        let result = generate_hermes_summary(&turns, &provider, None, Some(prev)).await;
        let text = result.unwrap();
        // The prompt sent to LLM should contain "UPDATING" and the previous summary
        assert!(
            text.contains("UPDATING") || text.contains(prev),
            "iterative update must reference previous summary; got: {}",
            &text[..text.floor_char_boundary(100)]
        );
    }

    #[tokio::test]
    async fn generate_hermes_summary_returns_none_on_llm_failure() {
        let turns = vec![make_message(MessageRole::User, "hello")];
        let provider = FailProvider;
        let result = generate_hermes_summary(&turns, &provider, None, None).await;
        assert!(result.is_none(), "must return None when LLM fails");
    }

    #[tokio::test]
    async fn compress_messages_reduces_token_count_and_keeps_tail() {
        // Build 20-message alternating User/Assistant conversation
        let mut msgs: Vec<Message> = (0..20)
            .map(|i| make_message(
                if i % 2 == 0 { MessageRole::User } else { MessageRole::Assistant },
                &"word ".repeat(100), // ~125 tokens each
            ))
            .collect();
        // Ensure last message is User
        msgs[19].role = MessageRole::User;

        let provider = EchoProvider("Mock summary content".into());

        let cfg = crate::config::CompactionConfig {
            enabled: true,
            threshold: 0.75,
            protect_first_n: 3,
            preserve_last_n: 3,
            summary_target_ratio: 0.20,
            extract_to_memory: false, // skip pgvector
            ..Default::default()
        };

        let tokens_before = estimate_tokens(&msgs) as u32;
        let mut compressor = crate::agent::compressor::Compressor::new(200_000);

        // Test messages have db_id: None so no DB calls will be made.
        // connect_lazy creates a pool without connecting — safe for this unit test.
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgres://localhost/test_unused")
            .unwrap();
        let facts = compress_messages(
            &mut msgs, &mut compressor, &cfg, &provider, None,
            &pool, uuid::Uuid::nil(),
        )
            .await
            .unwrap();

        let tokens_after = estimate_tokens(&msgs) as u32;
        assert!(tokens_after < tokens_before, "compression must reduce tokens");
        assert!(msgs.len() >= 3, "must keep at least tail messages");
        assert_eq!(
            msgs.last().unwrap().role,
            MessageRole::User,
            "last message must be User"
        );
        assert!(
            compressor.previous_summary.is_some(),
            "previous_summary must be populated"
        );
        assert_eq!(compressor.compression_count, 1, "compression_count must be 1");
        assert!(facts.is_empty(), "extract_to_memory=false → no facts");
    }

    /// H2 (T12 pt.5, hermes `_force_user_leading` parity): when
    /// `protect_first_n` collapses `head` down to a lone System message,
    /// the summary message must never become an `Assistant`-first message —
    /// Anthropic rejects `messages[0].role != "user"` with HTTP 400.
    #[tokio::test]
    async fn compress_messages_pins_summary_role_to_user_when_head_is_system_only() {
        let mut msgs: Vec<Message> = vec![make_message(MessageRole::System, "system prompt")];
        // messages[1] is Assistant (NOT Tool) so `find_head_end` does not slide
        // the boundary forward — head stays exactly `[System]`.
        for i in 0..20 {
            msgs.push(make_message(
                if i % 2 == 0 { MessageRole::Assistant } else { MessageRole::User },
                &"word ".repeat(100), // ~125 tokens each
            ));
        }
        // Ensure the real last message is User (tail-anchor invariant).
        let last_idx = msgs.len() - 1;
        msgs[last_idx].role = MessageRole::User;

        let provider = EchoProvider("Mock summary content".into());

        let cfg = crate::config::CompactionConfig {
            enabled: true,
            threshold: 0.75,
            protect_first_n: 1,
            preserve_last_n: 3,
            summary_target_ratio: 0.20,
            extract_to_memory: false,
            ..Default::default()
        };

        let mut compressor = crate::agent::compressor::Compressor::new(200_000);
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgres://localhost/test_unused")
            .unwrap();

        // Sanity: head really does collapse to exactly [System].
        let head_end = find_head_end(&msgs, cfg.protect_first_n);
        assert_eq!(head_end, 1, "head must be exactly [System] for this test to be valid");

        compress_messages(
            &mut msgs, &mut compressor, &cfg, &provider, None,
            &pool, uuid::Uuid::nil(),
        )
            .await
            .unwrap();

        // First non-system message (the summary, or whatever merged with it)
        // must be `User`, never `Assistant`.
        let first_non_system = msgs
            .iter()
            .find(|m| m.role != MessageRole::System)
            .expect("must have at least one non-system message after compression");
        assert_eq!(
            first_non_system.role,
            MessageRole::User,
            "first non-system message after compaction must be User when head is system-only"
        );
    }
}
