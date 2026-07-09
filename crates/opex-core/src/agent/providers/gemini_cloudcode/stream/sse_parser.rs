//! SSE event splitter for `streamGenerateContent?alt=sse`.
//!
//! Each SSE block is terminated by `\n\n`. Lines starting with `:`
//! are keepalive comments and are skipped. Lines starting with `data: `
//! carry the JSON payload.
//!
//! # Wire format
//! ```text
//! data: {"response":{"candidates":[{"content":{"parts":[{"text":"hi"}]},"finishReason":null}]}}\n\n
//! ```
//!
//! The outer `{"response": ...}` wrapper matches the Code Assist envelope.

use serde::Deserialize;

// ── Public types ─────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeminiStreamEvent {
    // F064: tolerate events with no `candidates` array (e.g. a usage-only
    // terminal chunk) instead of failing the whole parse and dropping it.
    #[serde(default)]
    pub candidates: Vec<GeminiCandidate>,
    #[serde(default)]
    pub usage_metadata: Option<GeminiUsageMetadata>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeminiCandidate {
    pub content: Option<GeminiContent>,
    #[serde(default)]
    pub finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeminiContent {
    // F064: Gemini streams terminal candidates carrying `content` (role only)
    // with NO `parts` array. A required field failed the whole SseEnvelope
    // parse, silently dropping that event and losing its finishReason + usage
    // (so the turn's token usage/billing went unrecorded). Default to empty.
    #[serde(default)]
    pub parts: Vec<GeminiPart>,
}

#[derive(Debug)]
pub enum GeminiPart {
    Text { text: String },
    FunctionCall { name: String, args: serde_json::Value },
    ThoughtSignature { thought_signature: String },
}

impl<'de> serde::Deserialize<'de> for GeminiPart {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = serde_json::Value::deserialize(d)?;
        if let Some(text) = v.get("text").and_then(|t| t.as_str()) {
            return Ok(GeminiPart::Text { text: text.to_string() });
        }
        if let Some(fc) = v.get("functionCall") {
            let name = fc.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
            let args = fc.get("args").cloned().unwrap_or(serde_json::Value::Object(Default::default()));
            return Ok(GeminiPart::FunctionCall { name, args });
        }
        if let Some(sig) = v.get("thoughtSignature").and_then(|s| s.as_str()) {
            return Ok(GeminiPart::ThoughtSignature { thought_signature: sig.to_string() });
        }
        // Unknown part type — treat as empty text so we don't lose the stream.
        tracing::debug!(part = %v, "gemini-cloudcode: unknown part type in stream, skipping");
        Ok(GeminiPart::Text { text: String::new() })
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeminiUsageMetadata {
    pub prompt_token_count: Option<u32>,
    pub candidates_token_count: Option<u32>,
    pub total_token_count: Option<u32>,
}

// ── Outer Code Assist SSE envelope ───────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct SseEnvelope {
    response: GeminiStreamEvent,
}

// ── Parser ───────────────────────────────────────────────────────────────────

/// Parse all complete SSE events from `raw`. Incomplete events (no trailing
/// `\n\n`) are silently discarded — callers accumulate bytes across HTTP
/// chunks and call this only on data that may contain one or more complete
/// events, or on the combined buffer.
///
/// Only `data: ...` lines are parsed; `:` comment lines (keepalive) are skipped.
pub fn parse_sse_events(raw: &str) -> Vec<GeminiStreamEvent> {
    let mut events = Vec::new();

    // SSE blocks are separated by blank lines (\n\n or \r\n\r\n).
    for block in raw.split("\n\n") {
        let block = block.trim();
        if block.is_empty() {
            continue;
        }
        // A block may have multiple lines; find the `data:` line.
        for line in block.lines() {
            let line = line.trim();
            if line.starts_with(':') {
                // SSE comment / keepalive — skip.
                continue;
            }
            if let Some(data) = line.strip_prefix("data: ") {
                match serde_json::from_str::<SseEnvelope>(data) {
                    Ok(env) => events.push(env.response),
                    Err(e) => {
                        tracing::debug!(
                            error = %e,
                            data = %&data[..data.floor_char_boundary(200)],
                            "gemini-cloudcode: failed to parse SSE event, skipping"
                        );
                    }
                }
                break; // only one data: line per block
            }
        }
    }

    events
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── sse_parser::tests::splits_two_complete_events ───────────────────────
    #[test]
    fn splits_two_complete_events() {
        let raw = concat!(
            "data: {\"response\":{\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"hello\"}]},\"finishReason\":null}]}}\n\n",
            "data: {\"response\":{\"candidates\":[{\"content\":{\"parts\":[{\"text\":\" world\"}]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":10,\"candidatesTokenCount\":2,\"totalTokenCount\":12}}}\n\n"
        );
        let events = parse_sse_events(raw);
        assert_eq!(events.len(), 2, "expected two events, got {}", events.len());
        // First event: text "hello"
        let first = &events[0];
        assert_eq!(first.candidates.len(), 1);
        let parts = first.candidates[0].content.as_ref().unwrap().parts.as_slice();
        assert!(matches!(&parts[0], GeminiPart::Text { text } if text == "hello"),
            "expected Text{{hello}}, got {:?}", parts);
        assert!(first.usage_metadata.is_none());
        // Second event: text " world" + STOP + usage
        let second = &events[1];
        assert_eq!(second.candidates[0].finish_reason.as_deref(), Some("STOP"));
        let meta = second.usage_metadata.as_ref().expect("usage_metadata missing");
        assert_eq!(meta.prompt_token_count, Some(10));
        assert_eq!(meta.candidates_token_count, Some(2));
    }

    // ── sse_parser::tests::buffers_partial_event_across_chunks ──────────────
    #[test]
    fn buffers_partial_event_across_chunks() {
        // Simulate a stream that arrives in two byte-chunks split mid-JSON line.
        // parse_sse_events only yields complete events (terminated by \n\n).
        let chunk1 = "data: {\"response\":{\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"he";
        let chunk2 = "llo\"}]}}]}}\n\n";
        // Neither chunk alone is a complete event; concatenated they are.
        let partial = parse_sse_events(chunk1);
        assert!(partial.is_empty(), "incomplete event must yield nothing");
        let combined = format!("{chunk1}{chunk2}");
        let events = parse_sse_events(&combined);
        assert_eq!(events.len(), 1);
        let parts = events[0].candidates[0].content.as_ref().unwrap().parts.as_slice();
        assert!(matches!(&parts[0], GeminiPart::Text { text } if text == "hello"));
    }

    // ── sse_parser::tests::ignores_keepalive_comments ───────────────────────
    #[test]
    fn ignores_keepalive_comments() {
        let raw = concat!(
            ": keepalive\n\n",
            "data: {\"response\":{\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"ok\"}]}}]}}\n\n"
        );
        let events = parse_sse_events(raw);
        assert_eq!(events.len(), 1, "comment-only blocks must be skipped");
        let parts = events[0].candidates[0].content.as_ref().unwrap().parts.as_slice();
        assert!(matches!(&parts[0], GeminiPart::Text { text } if text == "ok"));
    }
}
