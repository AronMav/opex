//! Streaming chat orchestration for OpenAI-compatible providers.
//!
//! Тело extracted из mod.rs::impl LlmProvider::chat_stream. Никаких поведенческих
//! изменений — move-only commit.

use super::{LlmProvider, LlmResponse, Message, Result, ToolDefinition, mpsc};
use super::minimax_xml::extract_minimax_xml_tool_calls;
use super::stream::{StreamChunk, StreamingUsage};
use super::OpenAiCompatibleProvider;
use crate::agent::providers::http::SendError;

impl OpenAiCompatibleProvider {
    // reviewed: offsets from find('\n')+1 (ASCII) — char boundaries
    #[allow(clippy::string_slice)]
    pub(super) async fn execute_chat_stream(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        chunk_tx: mpsc::Sender<String>,
        opts: super::super::CallOptions,
    ) -> Result<LlmResponse> {
        let effective_model = self.model.effective();
        let body = self.build_chat_body(messages, tools, true);

        tracing::info!(
            provider = %self.provider_name,
            model = %self.model,
            messages = messages.len(),
            tools = tools.len(),
            "calling LLM API (streaming)"
        );

        const LARGE_CONTEXT_CHARS: usize = 200_000;
        let ctx_bytes = Self::context_size(messages);
        if ctx_bytes > LARGE_CONTEXT_CHARS {
            tracing::warn!(
                provider = %self.provider_name,
                model = %self.model,
                context_bytes = ctx_bytes,
                threshold = LARGE_CONTEXT_CHARS,
                "large context being sent to LLM — provider may reject with 5xx or truncate silently"
            );
        }

        let start = std::time::Instant::now();
        let api_key = self.resolve_api_key().await;
        let effective_url = self.resolve_url().await;
        let auth_headers: Vec<(String, String)> = if api_key.is_empty() {
            Vec::new()
        } else {
            vec![("Authorization".to_string(), format!("Bearer {api_key}"))]
        };
        let resp = self.streaming_client
            .post_json_stream(
                &effective_url,
                &body,
                &auth_headers,
                &self.provider_name,
                crate::agent::providers::http::RETRYABLE_OPENAI,
                self.max_retries,
            )
            .await
            .map_err(|e| match e {
                SendError::Http { status, .. } if status == 401 || status == 403 =>
                    anyhow::Error::new(LlmCallError::AuthError {
                        provider: self.provider_name.clone(),
                        status,
                    }),
                SendError::Http { status, .. } if status >= 500 =>
                    anyhow::Error::new(LlmCallError::Server5xx {
                        provider: self.provider_name.clone(),
                        status,
                    }),
                SendError::Http { status, body, retry_after } => {
                    let msg = if let Some(ra) = retry_after {
                        format!("{} API error {status} (retry-after: {ra}): {body}", self.provider_name)
                    } else {
                        format!("{} API error {status}: {body}", self.provider_name)
                    };
                    anyhow::anyhow!(msg)
                }
                SendError::Network(e) =>
                    anyhow::Error::new(crate::agent::providers::classify_reqwest_err(
                        e,
                        &self.provider_name,
                        self.timeouts.connect_secs,
                        self.timeouts.request_secs,
                    )),
            })?;

        // Parse SSE stream: accumulate content (streamed) + tool calls (buffered)
        let mut full_content = String::new();
        let mut full_reasoning = String::new(); // DeepSeek extended thinking (reasoning_content)
        // F062: accumulate RAW BYTES, not a String. Decoding each network
        // chunk with from_utf8_lossy corrupted any multi-byte char (Cyrillic,
        // emoji) split across a chunk boundary into U+FFFD. Buffer bytes and
        // decode only COMPLETE lines (which are whole UTF-8), so a split
        // sequence is reassembled from the next chunk before decoding.
        let mut buffer: Vec<u8> = Vec::new();
        let mut thinking_filter = crate::agent::thinking::ThinkingFilter::new();
        // R5: suppress hallucinated extension-tool "calls" (e.g.
        // `sequentialthinking\n{...}`) that weak-adherence models emit as
        // free-form content. Runs AFTER the thinking filter, so `<think>`
        // reasoning is already stripped. Pure passthrough when the agent has no
        // extension tools (empty list).
        let mut hallucinated_filter =
            super::hallucinated_tool::HallucinatedToolFilter::new(opts.known_extension_tools.clone());
        // Indexed by tool_call index: (id, name, arguments)
        let mut tool_call_parts: Vec<(String, String, String)> = Vec::new();
        let mut usage: Option<StreamingUsage> = None;
        let mut finish_reason: Option<String> = None;

        use tokio_stream::StreamExt;
        use crate::agent::providers::{CancelSlot, LlmCallError, cancellable_stream::stream_with_cancellation};

        let slot = CancelSlot::new();
        let byte_stream = stream_with_cancellation(
            resp.bytes_stream(),
            self.cancel.child_token(),
            slot.clone(),
            self.timeouts,
        );
        let mut byte_stream = std::pin::pin!(byte_stream);
        'outer: loop {
            let chunk_result = match StreamExt::next(&mut byte_stream).await {
                Some(r) => r,
                None => break 'outer, // stream ended (either clean EOF or cancelled — slot tells us which)
            };
            let chunk_bytes = match chunk_result {
                Ok(b) => b,
                Err(e) => {
                    // Wrap as anyhow so the routing layer can downcast to LlmCallError and decide failover.
                    return Err(anyhow::Error::new(LlmCallError::from(e)));
                }
            };
            buffer.extend_from_slice(&chunk_bytes);

            while let Some(line_end) = buffer.iter().position(|&b| b == b'\n') {
                // A complete line (bytes up to '\n') is whole UTF-8, so lossy
                // decoding here can never split a multi-byte char.
                let line = String::from_utf8_lossy(&buffer[..line_end]).trim().to_string();
                buffer.drain(..=line_end);

                if line.is_empty() || line.starts_with(':') {
                    continue;
                }

                if let Some(data) = line.strip_prefix("data: ") {
                    if data.trim() == "[DONE]" {
                        break 'outer;
                    }

                    match serde_json::from_str::<StreamChunk>(data) {
                        Ok(chunk_json) => {
                            // Capture usage if present (some providers send in final chunk)
                            if let Some(ref u) = chunk_json.usage {
                                usage = Some(StreamingUsage {
                                    input: u.prompt_tokens,
                                    output: u.completion_tokens,
                                    cache_read: u
                                        .prompt_tokens_details
                                        .as_ref()
                                        .and_then(|d| d.cached_tokens),
                                    cache_creation: None, // OpenAI does not report cache writes
                                    reasoning: u
                                        .completion_tokens_details
                                        .as_ref()
                                        .and_then(|d| d.reasoning_tokens),
                                });
                            }
                            if let Some(choice) = chunk_json.choices.first() {
                                // Capture finish reason
                                if let Some(ref fr) = choice.finish_reason {
                                    finish_reason = Some(fr.clone());
                                }
                                // Stream content tokens
                                if let Some(ref content) = choice.delta.content {
                                    full_content.push_str(content);
                                    let filtered = thinking_filter.process(content);
                                    if !filtered.is_empty() {
                                        let visible = hallucinated_filter.process(&filtered);
                                        if !visible.is_empty() {
                                            chunk_tx.send(visible).await.ok();
                                        }
                                    }
                                }
                                // Capture DeepSeek reasoning_content (not streamed to UI)
                                if let Some(ref reasoning) = choice.delta.reasoning_content {
                                    full_reasoning.push_str(reasoning);
                                }
                                // Accumulate tool call deltas by index
                                for tc in &choice.delta.tool_calls {
                                    let idx = tc.index;
                                    while tool_call_parts.len() <= idx {
                                        tool_call_parts.push((String::new(), String::new(), String::new()));
                                    }
                                    if let Some(ref id) = tc.id {
                                        tool_call_parts[idx].0 = id.clone();
                                    }
                                    if let Some(ref func) = tc.function {
                                        if let Some(ref name) = func.name {
                                            // Replace, don't append — name arrives once or repeated,
                                            // unlike arguments which stream incrementally.
                                            tool_call_parts[idx].1 = name.clone();
                                        }
                                        if let Some(ref args) = func.arguments {
                                            tool_call_parts[idx].2.push_str(args);
                                        }
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            // Issue C (SchemaError): on the FIRST parse failure
                            // before any text has been streamed, surface a typed
                            // pre-stream SchemaError so RoutingProvider can fail
                            // over (at_bytes=0 ⇒ failover-worthy per spec §4.4).
                            // Once content has already streamed, continue-and-skip
                            // to preserve the in-progress response (subsequent
                            // noise like heartbeats also lands here harmlessly).
                            if full_content.is_empty() {
                                tracing::warn!(
                                    provider = %self.provider_name,
                                    error = %e,
                                    "SSE parse failed pre-stream, classifying as SchemaError"
                                );
                                return Err(anyhow::Error::new(LlmCallError::SchemaError {
                                    provider: self.provider_name.clone(),
                                    detail: e.to_string(),
                                    at_bytes: 0,
                                }));
                            }
                            tracing::debug!(
                                provider = %self.provider_name,
                                error = %e,
                                "failed to parse SSE chunk mid-stream, skipping"
                            );
                            continue;
                        }
                    }
                }
            }
        }

        // Stream exited. If cancellation fired, surface the typed reason with
        // the partial text we already streamed — callers can downcast to
        // `LlmCallError` and either persist a partial assistant turn
        // (user_cancelled / shutdown_drain / max_duration / inactivity) or
        // treat it as failover-worthy (see `LlmCallError::is_failover_worthy`).
        if let Some(reason) = slot.get() {
            use crate::agent::providers::error::{CancelReason, PartialState};
            let partial_state = if !tool_call_parts.is_empty() {
                PartialState::ToolUse
            } else if !full_content.is_empty() {
                PartialState::Text(full_content.clone())
            } else {
                PartialState::Empty
            };
            let err = match reason {
                CancelReason::InactivityTimeout { silent_secs } => LlmCallError::InactivityTimeout {
                    provider: self.name().to_string(),
                    silent_secs,
                    partial_state,
                },
                CancelReason::MaxDurationExceeded { elapsed_secs } => LlmCallError::MaxDurationExceeded {
                    provider: self.name().to_string(),
                    elapsed_secs,
                    partial_state,
                },
                CancelReason::UserCancelled => LlmCallError::UserCancelled { partial_state },
                CancelReason::ShutdownDrain => LlmCallError::ShutdownDrain { partial_state },
            };
            return Err(anyhow::Error::new(err));
        }

        // R5: flush any text the hallucinated-tool filter was still buffering
        // (a `NeedMore`/boundary prefix that never became a call). An in-progress
        // suppression is dropped by `finish()`.
        let tail = hallucinated_filter.finish();
        if !tail.is_empty() {
            chunk_tx.send(tail).await.ok();
        }

        let elapsed = start.elapsed();
        // Convert accumulated tool call parts to ToolCall values
        let mut tool_calls: Vec<opex_types::ToolCall> = tool_call_parts
            .into_iter()
            .filter(|(_, name, _)| !name.is_empty())
            .map(|(id, name, args)| {
                let arguments = match crate::agent::json_repair::repair_json(&args) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(tool = %name, error = %e, raw_len = args.len(), "tool call JSON repair failed, using empty args");
                        serde_json::Value::Object(Default::default())
                    }
                };
                opex_types::ToolCall {
                    id: opex_types::ids::ToolCallId::from(id),
                    name,
                    arguments,
                    thought_signature: None,
                }
            })
            .collect();

        // Extract any MiniMax XML tool calls that leaked into the streamed text.
        let (full_content, xml_calls) = extract_minimax_xml_tool_calls(&full_content);
        if !xml_calls.is_empty() {
            tracing::warn!(
                provider = %self.provider_name,
                count = xml_calls.len(),
                "extracted MiniMax XML tool calls from streaming content"
            );
            tool_calls.extend(xml_calls);
        }

        // R5 fix 3: post-hoc strip hallucinated extension-tool "calls" from the
        // PERSISTED content (same conservative matcher as the live filter) so a
        // reload stays consistent with what was shown live. Runs on the
        // THINKING-STRIPPED content — same input the live path effectively
        // sees (thinking_filter runs before hallucinated_filter per-chunk) —
        // so a call on the SAME line as a `</think>` close (e.g.
        // `</think>sequentialthinking\n{...}`) is still caught; matching on raw
        // `full_content` would leave that call mid-line and unsuppressed.
        // No-op when the known-tool list is empty or nothing matched.
        let full_content = super::hallucinated_tool::strip_hallucinated_tool_calls(
            &crate::agent::thinking::strip_thinking(&full_content),
            opts.known_extension_tools.as_slice(),
        );

        tracing::info!(
            provider = %self.provider_name,
            content_len = full_content.len(),
            tool_calls = tool_calls.len(),
            finish_reason = ?finish_reason,
            elapsed_ms = elapsed.as_millis() as u64,
            "streaming response complete"
        );

        Ok(LlmResponse {
            content: full_content,
            tool_calls,
            usage: usage.map(Into::into),
            model: Some(effective_model),
            provider: Some(self.provider_name.clone()),
            fallback_notice: None,
            finish_reason,
            tools_used: vec![],
            iterations: 0,
            thinking_blocks: if full_reasoning.is_empty() {
                vec![]
            } else {
                vec![opex_types::ThinkingBlock {
                    thinking: full_reasoning,
                    signature: String::new(),
                }]
            },
        })
    }
}
