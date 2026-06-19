//! Converts a sequence of [`GeminiStreamEvent`]s into provider-neutral
//! [`DeltaChunk`]s consumed by the streaming pipeline.
//!
//! This module is the bridge between the SSE wire format and the
//! `LlmProvider::chat_stream` output — it accumulates partial function-call
//! argument JSON across events and emits clean, ordered deltas.
//!
//! # Lifecycle
//! 1. Caller drives an HTTP response body through [`parse_sse_events`].
//! 2. Each yielded [`GeminiStreamEvent`] is passed to [`events_to_deltas`].
//! 3. The returned [`DeltaChunk`]s are forwarded to the engine's `chunk_tx`.
#![allow(dead_code)]

use super::sse_parser::{GeminiPart, GeminiStreamEvent};

// ── Public types ─────────────────────────────────────────────────────────────

/// A single, provider-neutral streaming increment emitted to the engine.
#[derive(Debug, Clone)]
pub enum DeltaChunk {
    /// Incremental text delta — append to the current assistant message.
    Text(String),
    /// A complete function-call is ready (name + fully-accumulated JSON args).
    FunctionCall {
        /// Index of the tool call within this turn (0-based, stable).
        index: usize,
        name: String,
        args_json: String,
    },
    /// The model finished generating; `reason` mirrors Gemini's `finishReason`.
    Finish { reason: String },
}

// ── Converter ────────────────────────────────────────────────────────────────

/// Translate one [`GeminiStreamEvent`] into zero or more [`DeltaChunk`]s.
///
/// Function-call parts arrive with their full `args` object in a single SSE
/// event (Gemini does not stream partial JSON), so each `functionCall` part
/// maps directly to one [`DeltaChunk::FunctionCall`].
///
/// `tool_call_index` is a mutable counter owned by the caller so that indices
/// remain stable across multiple events in the same turn.
pub fn events_to_deltas(
    event: GeminiStreamEvent,
    tool_call_index: &mut usize,
) -> Vec<DeltaChunk> {
    let mut deltas = Vec::new();

    for candidate in event.candidates {
        if let Some(content) = candidate.content {
            for part in content.parts {
                match part {
                    GeminiPart::Text { text } if !text.is_empty() => {
                        deltas.push(DeltaChunk::Text(text));
                    }
                    GeminiPart::Text { .. } => {
                        // Empty text delta — skip.
                    }
                    GeminiPart::FunctionCall { name, args } => {
                        let args_json = serde_json::to_string(&args)
                            .unwrap_or_else(|_| "{}".to_string());
                        deltas.push(DeltaChunk::FunctionCall {
                            index: *tool_call_index,
                            name,
                            args_json,
                        });
                        *tool_call_index += 1;
                    }
                    GeminiPart::ThoughtSignature { .. } => {
                        // Thinking signatures are handled by the non-streaming
                        // path (translate_gemini_response); skip in stream.
                    }
                }
            }
        }

        if let Some(reason) = candidate.finish_reason
            && !reason.is_empty() && reason != "null"
        {
            deltas.push(DeltaChunk::Finish { reason });
        }
    }

    deltas
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::sse_parser::parse_sse_events;

    // ── delta::tests::text_parts_become_text_deltas ──────────────────────────
    #[test]
    fn text_parts_become_text_deltas() {
        let raw = "data: {\"response\":{\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"hi\"}]},\"finishReason\":null}]}}\n\n";
        let events = parse_sse_events(raw);
        assert_eq!(events.len(), 1);
        let mut idx = 0usize;
        let deltas = events_to_deltas(events.into_iter().next().unwrap(), &mut idx);
        assert_eq!(deltas.len(), 1);
        assert!(matches!(&deltas[0], DeltaChunk::Text(t) if t == "hi"));
    }

    // ── delta::tests::function_call_emits_delta_with_stable_index ───────────
    #[test]
    fn function_call_emits_delta_with_stable_index() {
        let raw = concat!(
            "data: {\"response\":{\"candidates\":[{\"content\":{\"parts\":[",
            "{\"functionCall\":{\"name\":\"my_tool\",\"args\":{\"k\":\"v\"}}}",
            "]},\"finishReason\":\"STOP\"}]}}\n\n"
        );
        let events = parse_sse_events(raw);
        assert_eq!(events.len(), 1);
        let mut idx = 0usize;
        let deltas = events_to_deltas(events.into_iter().next().unwrap(), &mut idx);
        // Expect: FunctionCall(index=0) + Finish
        let fc = deltas.iter().find(|d| matches!(d, DeltaChunk::FunctionCall { .. }));
        assert!(fc.is_some(), "expected FunctionCall delta");
        if let Some(DeltaChunk::FunctionCall { index, name, args_json }) = fc {
            assert_eq!(*index, 0);
            assert_eq!(name, "my_tool");
            let v: serde_json::Value = serde_json::from_str(args_json).unwrap();
            assert_eq!(v["k"], "v");
        }
        // Index counter advanced
        assert_eq!(idx, 1);
    }

    // ── delta::tests::finish_reason_emits_finish_delta ───────────────────────
    #[test]
    fn finish_reason_emits_finish_delta() {
        let raw = "data: {\"response\":{\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"done\"}]},\"finishReason\":\"STOP\"}]}}\n\n";
        let events = parse_sse_events(raw);
        let mut idx = 0usize;
        let deltas = events_to_deltas(events.into_iter().next().unwrap(), &mut idx);
        let finish = deltas.iter().find(|d| matches!(d, DeltaChunk::Finish { .. }));
        assert!(finish.is_some(), "expected Finish delta");
        if let Some(DeltaChunk::Finish { reason }) = finish {
            assert_eq!(reason, "STOP");
        }
    }
}
