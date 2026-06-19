//! OpenAI-shaped request → Gemini Code Assist wire format translation.
//!
//! Mirrors Hermes `gemini_cloudcode_adapter.py::_build_gemini_contents` and
//! `_build_function_declarations`.

use hydeclaw_types::{Message, MessageRole, ToolDefinition};
use serde_json::{json, Value};

use super::schema::sanitize_gemini_tool_parameters;

// ── Public API ────────────────────────────────────────────────────────────────

/// Build the inner Gemini `request` object from OpenAI-shaped messages + tools.
///
/// - `messages`: full conversation history in internal format
/// - `tools`: tool definitions to convert to `functionDeclarations`
/// - `tool_choice`: OpenAI-style tool choice directive (None = AUTO)
/// - `generation_config`: passthrough `generationConfig` object
///   (temperature, maxOutputTokens, etc.)
pub fn build_gemini_request(
    messages: &[Message],
    tools: &[ToolDefinition],
    tool_choice: Option<&Value>,
    generation_config: Value,
) -> Value {
    let (system_instruction, contents) = messages_to_gemini_contents(messages);
    let tool_config = translate_tool_choice(tool_choice);

    let mut req = json!({
        "contents": contents,
        "toolConfig": tool_config,
        "generationConfig": generation_config,
    });

    if let Some(sys) = system_instruction {
        req["systemInstruction"] = json!({ "parts": [{ "text": sys }] });
    }

    if !tools.is_empty() {
        let fn_decls: Vec<Value> = tools
            .iter()
            .map(|t| {
                let sanitized = sanitize_gemini_tool_parameters(t.input_schema.clone());
                json!({
                    "name": t.name,
                    "description": t.description,
                    "parameters": sanitized,
                })
            })
            .collect();
        req["tools"] = json!([{ "functionDeclarations": fn_decls }]);
    }

    req
}

/// Wrap the inner request in the Code Assist outer envelope.
///
/// ```json
/// {
///   "project": "<project_id_or_empty>",
///   "model": "gemini-2.5-pro",
///   "user_prompt_id": "<uuid>",
///   "request": { ... }
/// }
/// ```
pub fn wrap_code_assist_request(
    project: &str,
    model: &str,
    user_prompt_id: &str,
    request: Value,
) -> Value {
    json!({
        "project": project,
        "model": model,
        "user_prompt_id": user_prompt_id,
        "request": request,
    })
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Convert messages to Gemini `contents[]` + optional `systemInstruction` text.
///
/// Translation table (from spec §Module 2 / Hermes `_build_gemini_contents`):
/// - First `system` msg → returned as `systemInstruction` text (out-of-band)
/// - Subsequent `system` msgs → concatenated as USER text parts in `contents`
/// - `user` → `{ role: "user", parts: [{ text }] }`
/// - `assistant` (no tool_calls) → `{ role: "model", parts: [{ text }] }`
/// - `assistant` (with tool_calls) → `{ role: "model", parts: [text?, functionCall...] }`
/// - `tool` → `{ role: "user", parts: [{ functionResponse: { name, response: { result } } }] }`
///
/// Tool name for `functionResponse` is resolved by looking backward in the message
/// list for the assistant message whose `tool_calls` contain the matching `tool_call_id`.
fn messages_to_gemini_contents(messages: &[Message]) -> (Option<String>, Vec<Value>) {
    let mut system_instruction: Option<String> = None;
    let mut contents: Vec<Value> = Vec::new();

    for (idx, msg) in messages.iter().enumerate() {
        match msg.role {
            MessageRole::System => {
                if system_instruction.is_none() {
                    // First system message → systemInstruction (out-of-band)
                    system_instruction = Some(msg.content.clone());
                } else {
                    // Subsequent system messages → user text in contents
                    contents.push(json!({
                        "role": "user",
                        "parts": [{ "text": msg.content }]
                    }));
                }
            }
            MessageRole::User => {
                contents.push(json!({
                    "role": "user",
                    "parts": [{ "text": msg.content }]
                }));
            }
            MessageRole::Assistant => {
                if let Some(ref tool_calls) = msg.tool_calls
                    && !tool_calls.is_empty()
                {
                    let mut parts: Vec<Value> = Vec::new();
                    if !msg.content.is_empty() {
                        parts.push(json!({ "text": msg.content }));
                    }
                    for tc in tool_calls {
                        parts.push(json!({
                            "functionCall": {
                                "name": tc.name,
                                "args": tc.arguments,
                            }
                        }));
                    }
                    contents.push(json!({ "role": "model", "parts": parts }));
                } else {
                    contents.push(json!({
                        "role": "model",
                        "parts": [{ "text": msg.content }]
                    }));
                }
            }
            MessageRole::Tool => {
                // Look backward for the assistant message that issued this tool_call_id
                let call_id = msg.tool_call_id.as_ref().map(|id| id.as_str()).unwrap_or("");
                let tool_name = messages[..idx]
                    .iter()
                    .rev()
                    .find_map(|m| {
                        m.tool_calls
                            .as_ref()?
                            .iter()
                            .find(|tc| tc.id.as_str() == call_id)
                            .map(|tc| tc.name.clone())
                    })
                    .unwrap_or_else(|| {
                        tracing::warn!(
                            tool_call_id = %call_id,
                            "gemini-request: could not resolve tool name for functionResponse, \
                             using call_id as fallback"
                        );
                        call_id.to_string()
                    });

                contents.push(json!({
                    "role": "user",
                    "parts": [{
                        "functionResponse": {
                            "name": tool_name,
                            "response": { "result": msg.content }
                        }
                    }]
                }));
            }
        }
    }

    (system_instruction, contents)
}

/// Translate OpenAI `tool_choice` to Gemini `toolConfig.functionCallingConfig`.
fn translate_tool_choice(tool_choice: Option<&Value>) -> Value {
    match tool_choice {
        None => json!({ "functionCallingConfig": { "mode": "AUTO" } }),
        Some(Value::String(s)) => match s.as_str() {
            "none" => json!({ "functionCallingConfig": { "mode": "NONE" } }),
            "required" => json!({ "functionCallingConfig": { "mode": "ANY" } }),
            // "auto" or unknown string
            _ => json!({ "functionCallingConfig": { "mode": "AUTO" } }),
        },
        Some(Value::Object(obj)) => {
            // { "type": "function", "function": { "name": "X" } }
            if let Some(name) = obj
                .get("function")
                .and_then(|f| f.get("name"))
                .and_then(|n| n.as_str())
            {
                json!({
                    "functionCallingConfig": {
                        "mode": "ANY",
                        "allowedFunctionNames": [name]
                    }
                })
            } else {
                json!({ "functionCallingConfig": { "mode": "AUTO" } })
            }
        }
        _ => json!({ "functionCallingConfig": { "mode": "AUTO" } }),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use hydeclaw_types::{Message, MessageRole, ToolCall, ToolDefinition};
    use hydeclaw_types::ids::ToolCallId;
    use serde_json::json;

    fn user_msg(content: &str) -> Message {
        Message {
            role: MessageRole::User,
            content: content.to_string(),
            tool_calls: None,
            tool_call_id: None,
            thinking_blocks: vec![],
            db_id: None,
        }
    }

    fn system_msg(content: &str) -> Message {
        Message {
            role: MessageRole::System,
            content: content.to_string(),
            tool_calls: None,
            tool_call_id: None,
            thinking_blocks: vec![],
            db_id: None,
        }
    }

    fn assistant_msg(content: &str) -> Message {
        Message {
            role: MessageRole::Assistant,
            content: content.to_string(),
            tool_calls: None,
            tool_call_id: None,
            thinking_blocks: vec![],
            db_id: None,
        }
    }

    fn assistant_with_calls(content: &str, calls: Vec<(&str, &str)>) -> Message {
        let tool_calls = calls
            .into_iter()
            .map(|(id, name)| ToolCall {
                id: ToolCallId::from(id.to_string()),
                name: name.to_string(),
                arguments: json!({"q": "test"}),
            })
            .collect();
        Message {
            role: MessageRole::Assistant,
            content: content.to_string(),
            tool_calls: Some(tool_calls),
            tool_call_id: None,
            thinking_blocks: vec![],
            db_id: None,
        }
    }

    fn tool_msg(call_id: &str, content: &str) -> Message {
        Message {
            role: MessageRole::Tool,
            content: content.to_string(),
            tool_calls: None,
            tool_call_id: Some(ToolCallId::from(call_id.to_string())),
            thinking_blocks: vec![],
            db_id: None,
        }
    }

    #[test]
    fn system_message_extracted_to_system_instruction() {
        let msgs = vec![
            system_msg("You are a helpful assistant."),
            user_msg("Hello"),
        ];
        let result = build_gemini_request(&msgs, &[], None, json!({}));

        // System message must become systemInstruction, not appear in contents
        let sys = &result["systemInstruction"];
        assert!(sys.is_object(), "systemInstruction must be an object, got: {result}");
        assert_eq!(sys["parts"][0]["text"], "You are a helpful assistant.");

        // contents should only have the user message
        let contents = result["contents"].as_array().unwrap();
        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0]["role"], "user");
        assert_eq!(contents[0]["parts"][0]["text"], "Hello");
    }

    #[test]
    fn user_assistant_alternation_preserved() {
        let msgs = vec![
            user_msg("Question"),
            assistant_msg("Answer"),
            user_msg("Follow-up"),
        ];
        let result = build_gemini_request(&msgs, &[], None, json!({}));
        let contents = result["contents"].as_array().unwrap();
        assert_eq!(contents.len(), 3);
        assert_eq!(contents[0]["role"], "user");
        assert_eq!(contents[0]["parts"][0]["text"], "Question");
        assert_eq!(contents[1]["role"], "model");
        assert_eq!(contents[1]["parts"][0]["text"], "Answer");
        assert_eq!(contents[2]["role"], "user");
        assert_eq!(contents[2]["parts"][0]["text"], "Follow-up");
    }

    #[test]
    #[allow(non_snake_case)]
    fn assistant_with_tool_calls_emits_functionCall_parts() {
        let msgs = vec![
            user_msg("Search for something"),
            assistant_with_calls("Looking...", vec![("tc1", "searxng_search")]),
        ];
        let result = build_gemini_request(&msgs, &[], None, json!({}));
        let contents = result["contents"].as_array().unwrap();
        // Second content is the assistant with tool call
        let model_content = &contents[1];
        assert_eq!(model_content["role"], "model");
        let parts = model_content["parts"].as_array().unwrap();
        // Text part first (non-empty content), then functionCall part
        assert_eq!(parts[0]["text"], "Looking...");
        let fc = &parts[1]["functionCall"];
        assert!(fc.is_object(), "functionCall part must be present");
        assert_eq!(fc["name"], "searxng_search");
        assert!(fc["args"].is_object());
    }

    #[test]
    #[allow(non_snake_case)]
    fn tool_message_becomes_functionResponse_part() {
        let msgs = vec![
            user_msg("q"),
            assistant_with_calls("", vec![("tc42", "get_weather")]),
            tool_msg("tc42", "Sunny, 25°C"),
        ];
        let result = build_gemini_request(&msgs, &[], None, json!({}));
        let contents = result["contents"].as_array().unwrap();
        // Third content is the tool response
        let tool_content = &contents[2];
        assert_eq!(tool_content["role"], "user");
        let fr = &tool_content["parts"][0]["functionResponse"];
        assert!(fr.is_object(), "functionResponse must be present");
        assert_eq!(fr["name"], "get_weather");
        assert_eq!(fr["response"]["result"], "Sunny, 25°C");
    }

    #[test]
    fn tool_choice_required_emits_mode_any() {
        let choice = json!("required");
        let result = build_gemini_request(&[user_msg("hi")], &[], Some(&choice), json!({}));
        let tc = &result["toolConfig"]["functionCallingConfig"];
        assert_eq!(tc["mode"], "ANY");
        assert!(tc.get("allowedFunctionNames").is_none());
    }

    #[test]
    #[allow(non_snake_case)]
    fn tool_choice_specific_function_uses_allowedFunctionNames() {
        let choice = json!({"type": "function", "function": {"name": "my_tool"}});
        let result = build_gemini_request(&[user_msg("hi")], &[], Some(&choice), json!({}));
        let tc = &result["toolConfig"]["functionCallingConfig"];
        assert_eq!(tc["mode"], "ANY");
        let allowed = tc["allowedFunctionNames"].as_array().unwrap();
        assert_eq!(allowed.len(), 1);
        assert_eq!(allowed[0], "my_tool");
    }

    #[test]
    fn tool_choice_none_emits_mode_none() {
        let choice = json!("none");
        let result = build_gemini_request(&[user_msg("hi")], &[], Some(&choice), json!({}));
        let tc = &result["toolConfig"]["functionCallingConfig"];
        assert_eq!(tc["mode"], "NONE");
    }

    #[test]
    fn tool_choice_auto_emits_mode_auto() {
        let choice = json!("auto");
        let result = build_gemini_request(&[user_msg("hi")], &[], Some(&choice), json!({}));
        let tc = &result["toolConfig"]["functionCallingConfig"];
        assert_eq!(tc["mode"], "AUTO");
    }

    #[test]
    fn missing_tool_choice_emits_mode_auto() {
        let result = build_gemini_request(&[user_msg("hi")], &[], None, json!({}));
        let tc = &result["toolConfig"]["functionCallingConfig"];
        assert_eq!(tc["mode"], "AUTO");
    }

    #[test]
    #[allow(non_snake_case)]
    fn tools_translated_to_functionDeclarations() {
        let tools = vec![ToolDefinition {
            name: "get_weather".to_string(),
            description: "Get weather for a city".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "city": {"type": "string"}
                },
                "required": ["city"]
            }),
        }];
        let result = build_gemini_request(&[user_msg("hi")], &tools, None, json!({}));
        let decls = result["tools"][0]["functionDeclarations"].as_array().unwrap();
        assert_eq!(decls.len(), 1);
        assert_eq!(decls[0]["name"], "get_weather");
        assert_eq!(decls[0]["description"], "Get weather for a city");
        assert_eq!(decls[0]["parameters"]["properties"]["city"]["type"], "string");
    }

    #[test]
    fn wrap_code_assist_request_structure() {
        let inner = json!({"contents": [], "systemInstruction": {"parts": []}});
        let wrapped =
            wrap_code_assist_request("my-project", "gemini-2.5-pro", "uuid-123", inner.clone());
        assert_eq!(wrapped["project"], "my-project");
        assert_eq!(wrapped["model"], "gemini-2.5-pro");
        assert_eq!(wrapped["user_prompt_id"], "uuid-123");
        assert_eq!(wrapped["request"], inner);
    }
}
