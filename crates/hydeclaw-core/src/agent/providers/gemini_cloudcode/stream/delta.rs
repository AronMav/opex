//! Per-chunk OpenAI delta synthesis from `GeminiStreamEvent`.
//!
//! Gemini sends a complete `functionCall` in one chunk (no partial-args
//! streaming). We synthesize the OpenAI convention of streaming arg
//! deltas by emitting the full args object as a single chunk's
//! `arguments` field.

use super::sse_parser::{GeminiPart, GeminiStreamEvent, GeminiUsageMetadata};
use uuid::Uuid;

// ── Public types ─────────────────────────────────────────────────────────────

/// A synthesized OpenAI-style delta chunk ready to be shipped upstream.
#[derive(Debug)]
pub struct DeltaChunk {
    /// Text delta for the UI (empty when this chunk carries a tool call).
    pub text: String,
    /// Complete tool call synthesized from a single FunctionCall part.
    pub tool_call: Option<SynthesizedToolCall>,
    /// Finish reason when this is the last chunk (maps Gemini → OpenAI).
    pub finish_reason: Option<String>,
    /// Usage — only present on the last event with `usageMetadata`.
    pub usage: Option<StreamingUsage>,
}

#[derive(Debug)]
pub struct SynthesizedToolCall {
    pub id: String,
    pub name: String,
    /// Full args JSON (Gemini sends the complete object in one chunk).
    pub arguments: serde_json::Value,
}

/// Gemini-specific streaming usage bag.
/// Per D13: this is a LOCAL type for Gemini's usageMetadata shape (promptTokenCount,
/// candidatesTokenCount). Do NOT reference or reuse openai::stream::StreamingUsage —
/// that has a different shape and lives in a sibling provider module.
#[derive(Debug, Default, Clone)]
pub struct StreamingUsage {
    pub input: u32,
    pub output: u32,
}

// ── Private helpers ───────────────────────────────────────────────────────────

/// Convert `GeminiUsageMetadata` to local `StreamingUsage`.
fn metadata_to_usage(meta: GeminiUsageMetadata) -> StreamingUsage {
    StreamingUsage {
        input: meta.prompt_token_count.unwrap_or(0),
        output: meta.candidates_token_count.unwrap_or(0),
    }
}

/// Map a Gemini finish reason string to the OpenAI convention.
fn map_finish_reason(reason: &str) -> &'static str {
    match reason {
        "STOP" => "stop",
        "MAX_TOKENS" => "length",
        "SAFETY" | "RECITATION" => "content_filter",
        _ => "stop",
    }
}

// ── Core function ─────────────────────────────────────────────────────────────

/// Convert a `Vec<GeminiStreamEvent>` (from the SSE parser) into a vec of
/// `DeltaChunk` ready to be forwarded over `mpsc::Sender<String>`.
///
/// Rules:
/// - Each `Text` part → `DeltaChunk { text, tool_call: None, ... }`.
/// - Each `FunctionCall` part → `DeltaChunk { text: "", tool_call: Some(...) }`.
///   A fresh UUID is generated because Gemini does not provide a tool-call id;
///   the engine uses it for tool result correlation.
/// - `ThoughtSignature` parts are captured but not streamed (they feed
///   `LlmResponse.thinking_blocks` in the provider's final assembly).
/// - The finish reason and usage from the final candidate/event are
///   placed on the last chunk, or a sentinel empty chunk is emitted if no
///   content parts preceded them.
pub fn events_to_deltas(events: Vec<GeminiStreamEvent>) -> Vec<DeltaChunk> {
    let mut chunks: Vec<DeltaChunk> = Vec::new();

    for event in events {
        // usage_metadata is per-event (not per-candidate); consume it at most
        // once by wrapping in a mutable Option and calling `.take()` below.
        let mut usage = event.usage_metadata.map(metadata_to_usage);

        for candidate in event.candidates {
            let finish_reason = candidate
                .finish_reason
                .as_deref()
                .map(map_finish_reason);

            let parts = match candidate.content {
                Some(c) => c.parts,
                None => vec![],
            };

            for part in parts {
                match part {
                    GeminiPart::Text { text } if !text.is_empty() => {
                        chunks.push(DeltaChunk {
                            text,
                            tool_call: None,
                            finish_reason: None,
                            usage: None,
                        });
                    }
                    GeminiPart::FunctionCall { name, args } => {
                        let id = Uuid::new_v4().to_string();
                        chunks.push(DeltaChunk {
                            text: String::new(),
                            tool_call: Some(SynthesizedToolCall { id, name, arguments: args }),
                            finish_reason: None,
                            usage: None,
                        });
                    }
                    GeminiPart::ThoughtSignature { .. } | GeminiPart::Text { .. } => {
                        // Empty text or thinking-only parts: no visible delta.
                    }
                }
            }

            // Stamp finish_reason + usage on the last chunk, or synthesize a
            // sentinel chunk if this candidate has no content parts.
            // `.take()` ensures usage is consumed at most once across candidates.
            let candidate_usage = usage.take();
            if finish_reason.is_some() || candidate_usage.is_some() {
                let can_stamp = chunks
                    .last()
                    .map(|c| c.finish_reason.is_none())
                    .unwrap_or(false);
                if can_stamp {
                    let last = chunks.last_mut().unwrap();
                    last.finish_reason = finish_reason.map(str::to_string);
                    if last.usage.is_none() {
                        last.usage = candidate_usage;
                    }
                } else {
                    // No prior chunk to stamp — emit an empty sentinel chunk.
                    chunks.push(DeltaChunk {
                        text: String::new(),
                        tool_call: None,
                        finish_reason: finish_reason.map(str::to_string),
                        usage: candidate_usage,
                    });
                }
            }
        }
    }

    chunks
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::providers::gemini_cloudcode::stream::sse_parser::{
        GeminiCandidate, GeminiContent, GeminiPart, GeminiStreamEvent, GeminiUsageMetadata,
    };

    fn text_event(text: &str, finish: Option<&str>) -> GeminiStreamEvent {
        GeminiStreamEvent {
            candidates: vec![GeminiCandidate {
                content: Some(GeminiContent {
                    parts: vec![GeminiPart::Text { text: text.to_string() }],
                }),
                finish_reason: finish.map(str::to_string),
            }],
            usage_metadata: None,
        }
    }

    fn fc_event(name: &str, args: serde_json::Value) -> GeminiStreamEvent {
        GeminiStreamEvent {
            candidates: vec![GeminiCandidate {
                content: Some(GeminiContent {
                    parts: vec![GeminiPart::FunctionCall { name: name.to_string(), args }],
                }),
                finish_reason: Some("STOP".to_string()),
            }],
            usage_metadata: Some(GeminiUsageMetadata {
                prompt_token_count: Some(5),
                candidates_token_count: Some(10),
                total_token_count: Some(15),
            }),
        }
    }

    // ── delta::tests::text_part_becomes_content_delta ───────────────────────
    #[test]
    fn text_part_becomes_content_delta() {
        let events = vec![text_event("hello", None), text_event(" world", Some("STOP"))];
        let deltas = events_to_deltas(events);
        assert!(!deltas.is_empty());
        let texts: String = deltas.iter().map(|d| d.text.as_str()).collect();
        assert_eq!(texts, "hello world");
        let last = deltas.last().unwrap();
        assert_eq!(last.finish_reason.as_deref(), Some("stop")); // mapped from STOP
        assert!(last.tool_call.is_none());
    }

    // ── delta::tests::functionCall_part_becomes_full_tool_call_chunk ────────
    // Per D23: this test verifies a Gemini SSE event with { functionCall: { name, args } }
    // produces a DeltaChunk whose tool_call has the correct name, arguments, and a non-empty id.
    #[test]
    fn function_call_part_becomes_full_tool_call_chunk() {
        let args = serde_json::json!({"q": "rust"});
        let events = vec![fc_event("search", args.clone())];
        let deltas = events_to_deltas(events);
        let call_chunk = deltas
            .iter()
            .find(|d| d.tool_call.is_some())
            .expect("expected a DeltaChunk with tool_call");
        let tc = call_chunk.tool_call.as_ref().unwrap();
        assert_eq!(tc.name, "search");
        assert_eq!(tc.arguments, args);
        assert!(!tc.id.is_empty(), "synthesized tool call id must not be empty");
    }

    // ── delta::tests::final_event_carries_finish_reason ─────────────────────
    #[test]
    fn final_event_carries_finish_reason() {
        let events = vec![text_event("hi", Some("STOP"))];
        let deltas = events_to_deltas(events);
        let last = deltas.last().unwrap();
        assert_eq!(last.finish_reason.as_deref(), Some("stop"));
    }
}
