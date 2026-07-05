//! Build the JSON body for OpenAI-compatible `POST /v1/chat/completions`
//! from a slice of `Message` plus `ToolDefinition`s. Pure — no HTTP,
//! no `await`, no `self.client`. Kept in its own file so the
//! streaming/non-streaming call paths in `mod.rs` stay focused on
//! transport concerns.

use super::{messages_to_openai_format, Message, OpenAiCompatibleProvider, ToolDefinition};

impl OpenAiCompatibleProvider {
    /// Construct the request body for `/v1/chat/completions`.
    ///
    /// When `stream == true`, adds `"stream": true` and
    /// `"stream_options": { "include_usage": true }` so OpenAI-compatible
    /// servers (Ollama, vLLM, SGLang, LiteLLM, DeepSeek, Moonshot, …)
    /// emit usage in the final chunk. Otherwise the response body carries
    /// usage natively and these fields are omitted.
    pub(super) fn build_chat_body(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        stream: bool,
    ) -> serde_json::Value {
        let effective_model = self.model.effective();
        let mut body = if stream {
            serde_json::json!({
                "model": effective_model,
                "messages": messages_to_openai_format(messages, self.uses_reasoning_content()),
                "stream": true,
                // Opt into the usage block on the final chunk. OpenAI-compatible
                // servers (Ollama, vLLM, SGLang, LiteLLM, DeepSeek, Moonshot, …)
                // omit `usage` from streaming responses by default; without this
                // flag we record 0 input/output tokens for every message on
                // locally-hosted backends. The parser already reads usage when
                // present, so this request-side opt-in is all that's needed.
                "stream_options": { "include_usage": true },
            })
        } else {
            serde_json::json!({
                "model": effective_model,
                "messages": messages_to_openai_format(messages, self.uses_reasoning_content()),
            })
        };
        // Temperature — omitted when the catalog marks the model as not accepting
        // it (Phase 3c): o1/reasoning-style models 400 on a `temperature` param.
        // Unknown model → send it (permissive default).
        let allow_temperature = opex_catalog::global_caps(
            &self.provider_name,
            &effective_model,
        )
        .is_none_or(|c| c.temperature);
        if allow_temperature {
            body["temperature"] = serde_json::json!(self.temperature);
        }
        if let Some(mt) = self.max_tokens {
            // Clamp to the model's catalog output limit (Phase 3): a configured
            // max_tokens above the model's cap 400s on some providers.
            let capped = opex_catalog::global_output(
                &self.provider_name,
                &effective_model,
            )
            .map_or(mt, |lim| mt.min(lim));
            body["max_tokens"] = serde_json::json!(capped);
        }

        if !tools.is_empty() {
            // Some OpenAI-compatible backends (e.g. Xiaomi MiMo) reject any
            // tools array containing duplicate function names with HTTP 400.
            // Dedupe by name, keeping the first occurrence — matches the
            // engine's precedence when resolving tool_use → tool dispatch.
            let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
            let tools_json: Vec<serde_json::Value> = tools
                .iter()
                .filter(|t| seen.insert(t.name.clone()))
                .map(|t| {
                    serde_json::json!({
                        "type": "function",
                        "function": {
                            "name": t.name,
                            "description": t.description,
                            "parameters": t.input_schema,
                        }
                    })
                })
                .collect();
            body["tools"] = serde_json::Value::Array(tools_json);
            if let Some(tool_name) = super::super::forced_skill_tool(messages, tools) {
                if self.supports_forced_tool_choice() {
                    body["tool_choice"] = serde_json::json!({
                        "type": "function",
                        "function": {"name": tool_name}
                    });
                }
                // else: reasoner models reject tool_choice — let model pick skill_use naturally
            } else if self.supports_parallel_tools() {
                body["parallel_tool_calls"] = serde_json::json!(true);
            }
        }

        body
    }
}
