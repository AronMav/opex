pub mod approvals;
pub mod channels;
pub mod ids;
pub mod sse;
pub mod ws;

pub use channels::*;

use chrono::{DateTime, Utc};
use ids::{MessageId, ToolCallId};
use serde::{Deserialize, Serialize};

pub(crate) fn default_typing_mode() -> String { "instant".to_string() }

// ── Messages ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: MessageRole,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    /// S2 T6: migrated from `Option<String>` to `Option<ToolCallId>` (newtype).
    /// `ToolCallId` is `#[serde(transparent)]` over `String` — wire format is
    /// identical to pre-T6. The newtype is purely compile-time type-tagging
    /// (caller can't pass a `MessageId` where a `ToolCallId` is expected).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<ToolCallId>,
    /// Thinking blocks (Anthropic only). Stored separately from content.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub thinking_blocks: Vec<ThinkingBlock>,
    /// DB primary key — populated when loaded from DB, None for synthetic messages.
    /// Not serialized; used only for in-session compression tracking.
    ///
    /// S2 T5: migrated from `Option<Uuid>` to `Option<MessageId>` (newtype).
    /// `MessageId` is `#[serde(transparent)]` over `Uuid`, but the field is
    /// `#[serde(skip)]` anyway — the type change is purely compile-time
    /// type-tagging.
    #[serde(skip, default)]
    pub db_id: Option<MessageId>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    /// S2 T6: migrated from `String` to `ToolCallId` (newtype). Wire format
    /// is identical (`#[serde(transparent)]` over `String`). Type-tag only.
    pub id: ToolCallId,
    pub name: String,
    pub arguments: serde_json::Value,
    /// Gemini 3.x thinking mode: each `functionCall` part carries an opaque
    /// `thought_signature` (base64 string) that the model expects to receive
    /// back on the next turn. Other providers leave it `None`. Persisted in
    /// `messages.tool_calls` JSONB; round-trips through history reconstruction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thought_signature: Option<String>,
}

// ── Tasks ──

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum TaskStatus {
    Pending,
    Planning,
    Running,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum StepStatus {
    Pending,
    Queued,
    Running,
    Completed,
    Failed,
}

// ── Incoming/Outgoing messages for channels ──

#[derive(Debug, Clone)]
pub struct IncomingMessage {
    pub user_id: String,
    /// Opaque context set by the channel adapter (`chat_id`, `message_id`, etc.).
    /// Core echoes it back to the adapter with replies/actions.
    pub context: serde_json::Value,
    pub text: Option<String>,
    pub attachments: Vec<MediaAttachment>,
    pub agent_id: String,
    pub channel: String,
    pub timestamp: DateTime<Utc>,
    /// Channel-specific formatting instructions for the LLM system prompt.
    /// Set only when the message arrives through a connected channel adapter.
    pub formatting_prompt: Option<String>,
    /// Optional tool policy override (used by cron jobs).
    /// When set, merged on top of the agent's tool policy before the engine runs.
    pub tool_policy_override: Option<serde_json::Value>,
    /// When set, engine builds LLM context from the branch chain ending at this message
    /// instead of the flat chronological history. Used for branching sessions.
    pub leaf_message_id: Option<uuid::Uuid>,
    /// When set, bootstrap skips saving a new user message and uses this existing
    /// message id directly as the user turn. Used by forkAndRegenerate so the
    /// branch message created by POST /api/sessions/{id}/fork is reused instead
    /// of creating a duplicate.
    pub user_message_id: Option<uuid::Uuid>,
}

/// Render a JSON scalar (string or number) as a `String` for use as a scope
/// key. Returns `None` for null/bool/array/object — those are not valid
/// chat-scope shapes from any known adapter.
fn json_scalar_to_string(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::String(s) if !s.is_empty() => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Extract the per-chat/group/thread disambiguator from an adapter's opaque
/// `context`, so channel-session lookups can be scoped per chat instead of
/// just per platform (T03 triage Point 5: a Telegram user_id writing in
/// group A and group B previously collapsed into ONE session, leaking group
/// A's history into group B).
///
/// Shared by [`IncomingMessage::chat_scope`] and
/// [`channels::IncomingMessageDto::chat_scope`] so the dispatcher's
/// `SessionKey` (computed from the DTO, before `into_incoming`) and the
/// engine's session lookup (computed from `IncomingMessage`, after) always
/// agree on the same scope for the same wire message.
///
/// Tries known field names in priority order — first match wins:
/// - `chat_id` (Telegram; JSON number) — the most common case.
/// - `guild_id` combined with `channel_id` (Discord) as
///   `"{guild_id}:{channel_id}"` when both are present, else whichever is
///   present alone (DM channels have no `guild_id`).
/// - `room_id` (Matrix).
/// - `channel` (Slack channel id — distinct from the top-level platform
///   `channel` field, which is `"slack"`).
///
/// Returns `None` when the context carries none of these (e.g. WhatsApp/
/// email, where `user_id` already uniquely identifies the peer, or web/UI/
/// cron callers whose `context` is `Value::Null`). Callers MUST treat `None`
/// as a valid, non-error degrade — never panic or bail.
#[must_use]
pub fn context_chat_scope(ctx: &serde_json::Value) -> Option<String> {
    if let Some(v) = ctx.get("chat_id") {
        return json_scalar_to_string(v);
    }
    let guild = ctx.get("guild_id").and_then(json_scalar_to_string);
    let chan = ctx.get("channel_id").and_then(json_scalar_to_string);
    match (guild, chan) {
        (Some(g), Some(c)) => return Some(format!("{g}:{c}")),
        (Some(g), None) => return Some(g),
        (None, Some(c)) => return Some(c),
        (None, None) => {}
    }
    if let Some(v) = ctx.get("room_id") {
        return json_scalar_to_string(v);
    }
    if let Some(v) = ctx.get("channel") {
        return json_scalar_to_string(v);
    }
    None
}

impl IncomingMessage {
    /// See [`context_chat_scope`].
    #[must_use]
    pub fn chat_scope(&self) -> Option<String> {
        context_chat_scope(&self.context)
    }
}

// ── Tool definitions for LLM ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

// ── Thinking blocks (Anthropic extended thinking) ──

/// A thinking block from Anthropic extended thinking API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThinkingBlock {
    pub thinking: String,
    pub signature: String,
}

// ── LLM response ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmResponse {
    pub content: String,
    #[serde(default)]
    pub tool_calls: Vec<ToolCall>,
    pub usage: Option<TokenUsage>,
    /// Why the LLM stopped: "stop", "length", "`tool_calls`", "`content_filter`", etc.
    #[serde(default)]
    pub finish_reason: Option<String>,
    /// Which model actually answered (filled by provider).
    #[serde(default)]
    pub model: Option<String>,
    /// Provider name that answered (e.g. "minimax", "anthropic").
    #[serde(default)]
    pub provider: Option<String>,
    /// Set when a fallback provider answered instead of the primary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_notice: Option<String>,
    /// Tool names called during the agent loop (deduplicated, ordered by first use).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools_used: Vec<String>,
    /// Number of LLM iterations in the tool loop (0 = single-shot, no tools).
    #[serde(default)]
    pub iterations: u32,
    /// Thinking blocks from Anthropic extended thinking (empty for other providers).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub thinking_blocks: Vec<ThinkingBlock>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct TokenUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,

    /// Tokens read from prompt cache (Anthropic cache_read_input_tokens,
    /// OpenAI prompt_tokens_details.cached_tokens, Gemini cachedContentTokenCount).
    /// SUBSET of input_tokens — do not add to total.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<u32>,

    /// Tokens written to prompt cache (Anthropic cache_creation_input_tokens).
    /// SUBSET of input_tokens. Cost ×1.25 of base input — show separately in UI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_creation_tokens: Option<u32>,

    /// Hidden reasoning tokens (OpenAI o1/o3, DeepSeek-R1, Gemini thinking).
    /// SUBSET of output_tokens — do not add to total.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use serde_json::json;

    // ── 1. MessageRole serde ──

    #[test]
    fn message_role_serializes_to_lowercase() {
        assert_eq!(serde_json::to_string(&MessageRole::System).unwrap(), "\"system\"");
        assert_eq!(serde_json::to_string(&MessageRole::User).unwrap(), "\"user\"");
        assert_eq!(serde_json::to_string(&MessageRole::Assistant).unwrap(), "\"assistant\"");
        assert_eq!(serde_json::to_string(&MessageRole::Tool).unwrap(), "\"tool\"");
    }

    #[test]
    fn message_role_deserializes_from_lowercase() {
        assert_eq!(serde_json::from_str::<MessageRole>("\"system\"").unwrap(), MessageRole::System);
        assert_eq!(serde_json::from_str::<MessageRole>("\"user\"").unwrap(), MessageRole::User);
        assert_eq!(serde_json::from_str::<MessageRole>("\"assistant\"").unwrap(), MessageRole::Assistant);
        assert_eq!(serde_json::from_str::<MessageRole>("\"tool\"").unwrap(), MessageRole::Tool);
    }

    #[test]
    fn message_role_roundtrip() {
        for role in [MessageRole::System, MessageRole::User, MessageRole::Assistant, MessageRole::Tool] {
            let json = serde_json::to_string(&role).unwrap();
            let back: MessageRole = serde_json::from_str(&json).unwrap();
            assert_eq!(role, back);
        }
    }

    // ── 2. ChannelInbound tagged enum ──

    #[test]
    fn channel_inbound_message_serializes_with_type_tag() {
        let now = Utc::now();
        let dto = IncomingMessageDto {
            user_id: "u123".into(),
            display_name: Some("Alice".into()),
            text: Some("hello".into()),
            attachments: vec![],
            context: json!({"chat_id": 42}),
            timestamp: now,
        };
        let inbound = ChannelInbound::Message {
            request_id: "req-1".into(),
            msg: dto,
        };
        let v: serde_json::Value = serde_json::to_value(&inbound).unwrap();
        assert_eq!(v["type"], "message");
        assert_eq!(v["request_id"], "req-1");
        assert_eq!(v["msg"]["user_id"], "u123");
        assert_eq!(v["msg"]["text"], "hello");
    }

    #[test]
    fn channel_inbound_ping_serializes_with_type_tag() {
        let v: serde_json::Value = serde_json::to_value(&ChannelInbound::Ping).unwrap();
        assert_eq!(v["type"], "ping");
        // Ping has no other fields besides "type"
        assert_eq!(v.as_object().unwrap().len(), 1);
    }

    #[test]
    fn channel_inbound_message_roundtrip() {
        let now = Utc::now();
        let dto = IncomingMessageDto {
            user_id: "u456".into(),
            display_name: None,
            text: Some("test".into()),
            attachments: vec![],
            context: json!(null),
            timestamp: now,
        };
        let original = ChannelInbound::Message {
            request_id: "req-rt".into(),
            msg: dto,
        };
        let json = serde_json::to_string(&original).unwrap();
        let back: ChannelInbound = serde_json::from_str(&json).unwrap();
        match back {
            ChannelInbound::Message { request_id, msg } => {
                assert_eq!(request_id, "req-rt");
                assert_eq!(msg.user_id, "u456");
                assert_eq!(msg.text, Some("test".into()));
            }
            other => panic!("expected Message, got {:?}", other),
        }
    }

    #[test]
    fn channel_inbound_ping_roundtrip() {
        let json = serde_json::to_string(&ChannelInbound::Ping).unwrap();
        let back: ChannelInbound = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, ChannelInbound::Ping));
    }

    #[test]
    fn channel_inbound_access_check_roundtrip() {
        let original = ChannelInbound::AccessCheck {
            request_id: "ac-1".into(),
            user_id: "owner".into(),
        };
        let json = serde_json::to_string(&original).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "access_check");
        let back: ChannelInbound = serde_json::from_str(&json).unwrap();
        match back {
            ChannelInbound::AccessCheck { request_id, user_id } => {
                assert_eq!(request_id, "ac-1");
                assert_eq!(user_id, "owner");
            }
            other => panic!("expected AccessCheck, got {:?}", other),
        }
    }

    #[test]
    fn channel_inbound_ready_roundtrip() {
        let original = ChannelInbound::Ready {
            adapter_type: "telegram".into(),
            version: "1.0".into(),
            formatting_prompt: Some("test prompt".into()),
        };
        let json = serde_json::to_string(&original).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "ready");
        assert_eq!(v["formatting_prompt"], "test prompt");
        let back: ChannelInbound = serde_json::from_str(&json).unwrap();
        match back {
            ChannelInbound::Ready { adapter_type, version, formatting_prompt } => {
                assert_eq!(adapter_type, "telegram");
                assert_eq!(version, "1.0");
                assert_eq!(formatting_prompt, Some("test prompt".into()));
            }
            other => panic!("expected Ready, got {:?}", other),
        }

        // Backward compat: Ready without formatting_prompt
        let json_no_fp = r#"{"type":"ready","adapter_type":"discord","version":"2.0"}"#;
        let back2: ChannelInbound = serde_json::from_str(json_no_fp).unwrap();
        match back2 {
            ChannelInbound::Ready { formatting_prompt, .. } => {
                assert_eq!(formatting_prompt, None);
            }
            other => panic!("expected Ready, got {:?}", other),
        }
    }

    #[test]
    fn channel_inbound_cancel_roundtrip() {
        let original = ChannelInbound::Cancel {
            request_id: "cancel-1".into(),
        };
        let json = serde_json::to_string(&original).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "cancel");
        let back: ChannelInbound = serde_json::from_str(&json).unwrap();
        match back {
            ChannelInbound::Cancel { request_id } => {
                assert_eq!(request_id, "cancel-1");
            }
            other => panic!("expected Cancel, got {:?}", other),
        }
    }

    // ── 3. ChannelOutbound tagged enum ──

    #[test]
    fn channel_outbound_chunk_roundtrip() {
        let original = ChannelOutbound::Chunk {
            request_id: "r1".into(),
            text: "partial".into(),
        };
        let json = serde_json::to_string(&original).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "chunk");
        let back: ChannelOutbound = serde_json::from_str(&json).unwrap();
        match back {
            ChannelOutbound::Chunk { request_id, text } => {
                assert_eq!(request_id, "r1");
                assert_eq!(text, "partial");
            }
            other => panic!("expected Chunk, got {:?}", other),
        }
    }

    #[test]
    fn channel_outbound_done_roundtrip() {
        let original = ChannelOutbound::Done {
            request_id: "r2".into(),
            text: "final answer".into(),
        };
        let json = serde_json::to_string(&original).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "done");
        let back: ChannelOutbound = serde_json::from_str(&json).unwrap();
        match back {
            ChannelOutbound::Done { request_id, text } => {
                assert_eq!(request_id, "r2");
                assert_eq!(text, "final answer");
            }
            other => panic!("expected Done, got {:?}", other),
        }
    }

    #[test]
    fn channel_outbound_error_roundtrip() {
        let original = ChannelOutbound::Error {
            request_id: "r3".into(),
            message: "something broke".into(),
        };
        let json = serde_json::to_string(&original).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "error");
        let back: ChannelOutbound = serde_json::from_str(&json).unwrap();
        match back {
            ChannelOutbound::Error { request_id, message } => {
                assert_eq!(request_id, "r3");
                assert_eq!(message, "something broke");
            }
            other => panic!("expected Error, got {:?}", other),
        }
    }

    #[test]
    fn channel_outbound_pong_roundtrip() {
        let json = serde_json::to_string(&ChannelOutbound::Pong).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "pong");
        assert_eq!(v.as_object().unwrap().len(), 1);
        let back: ChannelOutbound = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, ChannelOutbound::Pong));
    }

    #[test]
    fn channel_outbound_action_roundtrip() {
        let action = ChannelActionDto {
            action: "react".into(),
            params: json!({"emoji": "thumbsup"}),
            context: json!({"chat_id": 100, "message_id": 5}),
        };
        let original = ChannelOutbound::Action {
            action_id: "act-1".into(),
            action,
        };
        let json = serde_json::to_string(&original).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "action");
        let back: ChannelOutbound = serde_json::from_str(&json).unwrap();
        match back {
            ChannelOutbound::Action { action_id, action } => {
                assert_eq!(action_id, "act-1");
                assert_eq!(action.action, "react");
                assert_eq!(action.params["emoji"], "thumbsup");
                assert_eq!(action.context["chat_id"], 100);
            }
            other => panic!("expected Action, got {:?}", other),
        }
    }

    #[test]
    fn channel_outbound_access_result_roundtrip() {
        let original = ChannelOutbound::AccessResult {
            request_id: "ar-1".into(),
            allowed: true,
            is_owner: false,
        };
        let json = serde_json::to_string(&original).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "access_result");
        let back: ChannelOutbound = serde_json::from_str(&json).unwrap();
        match back {
            ChannelOutbound::AccessResult { request_id, allowed, is_owner } => {
                assert_eq!(request_id, "ar-1");
                assert!(allowed);
                assert!(!is_owner);
            }
            other => panic!("expected AccessResult, got {:?}", other),
        }
    }

    // ── 4. IncomingMessageDto::into_incoming ──

    #[test]
    fn into_incoming_transfers_all_fields() {
        let now = Utc::now();
        let attachment = MediaAttachment {
            url: "https://example.com/photo.jpg".into(),
            media_type: MediaType::Image,
            file_name: Some("photo.jpg".into()),
            mime_type: Some("image/jpeg".into()),
            file_size: Some(12345),
        };
        let dto = IncomingMessageDto {
            user_id: "user-42".into(),
            display_name: Some("Bob".into()),
            text: Some("Look at this".into()),
            attachments: vec![attachment],
            context: json!({"chat_id": 999, "message_id": 77}),
            timestamp: now,
        };
        let incoming = dto.into_incoming("agent-main".into(), "telegram".into(), Some("fmt prompt".into()));

        assert_eq!(incoming.user_id, "user-42");
        assert_eq!(incoming.text, Some("Look at this".into()));
        assert_eq!(incoming.agent_id, "agent-main");
        assert_eq!(incoming.channel, "telegram");
        assert_eq!(incoming.formatting_prompt, Some("fmt prompt".into()));
        assert_eq!(incoming.timestamp, now);
        assert_eq!(incoming.context["chat_id"], 999);
        assert_eq!(incoming.context["message_id"], 77);
        assert_eq!(incoming.attachments.len(), 1);
        assert_eq!(incoming.attachments[0].url, "https://example.com/photo.jpg");
        assert_eq!(incoming.attachments[0].file_name, Some("photo.jpg".into()));
    }

    #[test]
    fn into_incoming_with_no_text_and_no_attachments() {
        let now = Utc::now();
        let dto = IncomingMessageDto {
            user_id: "u-empty".into(),
            display_name: None,
            text: None,
            attachments: vec![],
            context: json!(null),
            timestamp: now,
        };
        let incoming = dto.into_incoming("test-agent".into(), "discord".into(), None);
        assert_eq!(incoming.user_id, "u-empty");
        assert_eq!(incoming.text, None);
        assert!(incoming.attachments.is_empty());
        assert_eq!(incoming.agent_id, "test-agent");
        assert_eq!(incoming.channel, "discord");
        assert_eq!(incoming.formatting_prompt, None);
        assert!(incoming.context.is_null());
    }

    // ── 5. MediaAttachment skip_serializing_if ──

    #[test]
    fn media_attachment_omits_none_fields() {
        let attachment = MediaAttachment {
            url: "https://example.com/file.pdf".into(),
            media_type: MediaType::Document,
            file_name: None,
            mime_type: None,
            file_size: None,
        };
        let v: serde_json::Value = serde_json::to_value(&attachment).unwrap();
        assert_eq!(v["url"], "https://example.com/file.pdf");
        assert_eq!(v["media_type"], "document");
        // None fields must be absent from JSON, not null
        assert!(!v.as_object().unwrap().contains_key("file_name"));
        assert!(!v.as_object().unwrap().contains_key("mime_type"));
        assert!(!v.as_object().unwrap().contains_key("file_size"));
    }

    #[test]
    fn media_attachment_includes_some_fields() {
        let attachment = MediaAttachment {
            url: "https://cdn.example.com/track.mp3".into(),
            media_type: MediaType::Audio,
            file_name: Some("track.mp3".into()),
            mime_type: Some("audio/mpeg".into()),
            file_size: Some(5_000_000),
        };
        let v: serde_json::Value = serde_json::to_value(&attachment).unwrap();
        assert_eq!(v["file_name"], "track.mp3");
        assert_eq!(v["mime_type"], "audio/mpeg");
        assert_eq!(v["file_size"], 5_000_000);
    }

    #[test]
    fn media_attachment_deserializes_without_optional_fields() {
        let json = r#"{"url":"https://x.com/img.png","media_type":"image"}"#;
        let att: MediaAttachment = serde_json::from_str(json).unwrap();
        assert_eq!(att.url, "https://x.com/img.png");
        assert!(matches!(att.media_type, MediaType::Image));
        assert_eq!(att.file_name, None);
        assert_eq!(att.mime_type, None);
        assert_eq!(att.file_size, None);
    }

    // ── 6. ToolCall roundtrip with JSON arguments ──

    #[test]
    fn tool_call_roundtrip() {
        let tc = ToolCall {
            id: "call-abc123".into(),
            name: "get_weather".into(),
            arguments: json!({"city": "Samara", "units": "metric"}),
            thought_signature: None,
        };
        let json = serde_json::to_string(&tc).unwrap();
        let back: ToolCall = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id.as_str(), "call-abc123");
        assert_eq!(back.name, "get_weather");
        assert_eq!(back.arguments["city"], "Samara");
        assert_eq!(back.arguments["units"], "metric");
    }

    #[test]
    fn tool_call_with_nested_arguments() {
        let tc = ToolCall {
            id: "call-nested".into(),
            name: "complex_tool".into(),
            arguments: json!({
                "query": "test",
                "options": {"limit": 10, "filters": ["a", "b"]},
                "flag": true
            }),
            thought_signature: None,
        };
        let json = serde_json::to_string(&tc).unwrap();
        let back: ToolCall = serde_json::from_str(&json).unwrap();
        assert_eq!(back.arguments["options"]["limit"], 10);
        assert_eq!(back.arguments["options"]["filters"][0], "a");
        assert_eq!(back.arguments["flag"], true);
    }

    #[test]
    fn message_with_tool_calls_roundtrip() {
        let msg = Message {
            role: MessageRole::Assistant,
            content: String::new(),
            tool_calls: Some(vec![
                ToolCall {
                    id: "tc-1".into(),
                    name: "search".into(),
                    arguments: json!({"q": "rust"}),
                    thought_signature: None,
                },
            ]),
            tool_call_id: None,
            thinking_blocks: vec![],
            db_id: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        // tool_calls present, tool_call_id absent
        assert!(v.get("tool_calls").is_some());
        assert!(!v.as_object().unwrap().contains_key("tool_call_id"));
        let back: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(back.tool_calls.as_ref().unwrap().len(), 1);
        assert_eq!(back.tool_calls.unwrap()[0].name, "search");
    }

    #[test]
    fn message_tool_response_roundtrip() {
        let msg = Message {
            role: MessageRole::Tool,
            content: "{\"result\": 42}".into(),
            tool_calls: None,
            tool_call_id: Some("tc-1".into()),
            thinking_blocks: vec![],
            db_id: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        // tool_call_id present, tool_calls absent
        assert!(v.get("tool_call_id").is_some());
        assert!(!v.as_object().unwrap().contains_key("tool_calls"));
        let back: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(back.tool_call_id, Some(ToolCallId::from("tc-1")));
    }

    // ── 7. LlmResponse with optional fields ──

    #[test]
    fn llm_response_full_roundtrip() {
        let resp = LlmResponse {
            content: "Hello!".into(),
            tool_calls: vec![
                ToolCall {
                    id: "tc-llm".into(),
                    name: "memory".into(),
                    arguments: json!({"query": "test"}),
                    thought_signature: None,
                },
            ],
            usage: Some(TokenUsage {
                input_tokens: 150,
                output_tokens: 42,
                ..Default::default()
            }),
            model: Some("minimax-m2.5".into()),
            provider: Some("minimax".into()),
            fallback_notice: None,
            finish_reason: None,
            tools_used: vec![],
            iterations: 0,
            thinking_blocks: vec![],
        };
        let json = serde_json::to_string(&resp).unwrap();
        let back: LlmResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(back.content, "Hello!");
        assert!(back.fallback_notice.is_none());
        assert_eq!(back.tool_calls.len(), 1);
        assert_eq!(back.tool_calls[0].name, "memory");
        let usage = back.usage.unwrap();
        assert_eq!(usage.input_tokens, 150);
        assert_eq!(usage.output_tokens, 42);
        assert_eq!(back.model, Some("minimax-m2.5".into()));
        assert_eq!(back.provider, Some("minimax".into()));
    }

    #[test]
    fn llm_response_minimal() {
        let json = r#"{"content":"ok","tool_calls":[]}"#;
        let resp: LlmResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.content, "ok");
        assert!(resp.tool_calls.is_empty());
        assert!(resp.usage.is_none());
        assert_eq!(resp.model, None);
        assert_eq!(resp.provider, None);
    }

    #[test]
    fn llm_response_defaults_for_missing_fields() {
        // tool_calls defaults to empty vec, model/provider default to None
        let json = r#"{"content":"hi"}"#;
        let resp: LlmResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.content, "hi");
        assert!(resp.tool_calls.is_empty());
        assert!(resp.usage.is_none());
        assert_eq!(resp.model, None);
        assert_eq!(resp.provider, None);
    }

    // ── 8. TaskStatus serde ──

    #[test]
    fn task_status_serializes_to_lowercase() {
        assert_eq!(serde_json::to_string(&TaskStatus::Pending).unwrap(), "\"pending\"");
        assert_eq!(serde_json::to_string(&TaskStatus::Planning).unwrap(), "\"planning\"");
        assert_eq!(serde_json::to_string(&TaskStatus::Running).unwrap(), "\"running\"");
        assert_eq!(serde_json::to_string(&TaskStatus::Completed).unwrap(), "\"completed\"");
        assert_eq!(serde_json::to_string(&TaskStatus::Failed).unwrap(), "\"failed\"");
        assert_eq!(serde_json::to_string(&TaskStatus::Cancelled).unwrap(), "\"cancelled\"");
    }

    #[test]
    fn task_status_roundtrip() {
        for status in [
            TaskStatus::Pending,
            TaskStatus::Planning,
            TaskStatus::Running,
            TaskStatus::Completed,
            TaskStatus::Failed,
            TaskStatus::Cancelled,
        ] {
            let json = serde_json::to_string(&status).unwrap();
            let back: TaskStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(status, back);
        }
    }

    // ── 9. StepStatus serde ──

    #[test]
    fn step_status_serializes_to_lowercase() {
        assert_eq!(serde_json::to_string(&StepStatus::Pending).unwrap(), "\"pending\"");
        assert_eq!(serde_json::to_string(&StepStatus::Queued).unwrap(), "\"queued\"");
        assert_eq!(serde_json::to_string(&StepStatus::Running).unwrap(), "\"running\"");
        assert_eq!(serde_json::to_string(&StepStatus::Completed).unwrap(), "\"completed\"");
        assert_eq!(serde_json::to_string(&StepStatus::Failed).unwrap(), "\"failed\"");
    }

    #[test]
    fn step_status_roundtrip() {
        for status in [
            StepStatus::Pending,
            StepStatus::Queued,
            StepStatus::Running,
            StepStatus::Completed,
            StepStatus::Failed,
        ] {
            let json = serde_json::to_string(&status).unwrap();
            let back: StepStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(status, back);
        }
    }

    // ── 10. MediaType serde ──

    #[test]
    fn media_type_serializes_to_lowercase() {
        assert_eq!(serde_json::to_string(&MediaType::Image).unwrap(), "\"image\"");
        assert_eq!(serde_json::to_string(&MediaType::Audio).unwrap(), "\"audio\"");
        assert_eq!(serde_json::to_string(&MediaType::Video).unwrap(), "\"video\"");
        assert_eq!(serde_json::to_string(&MediaType::Document).unwrap(), "\"document\"");
    }

    #[test]
    fn media_type_roundtrip() {
        let types = ["image", "audio", "video", "document"];
        for t in types {
            let json = format!("\"{}\"", t);
            let mt: MediaType = serde_json::from_str(&json).unwrap();
            let back = serde_json::to_string(&mt).unwrap();
            assert_eq!(back, json);
        }
    }

    // ── 11. MediaAttachment full roundtrip ──

    #[test]
    fn media_attachment_full_roundtrip() {
        let att = MediaAttachment {
            url: "https://example.com/video.mp4".into(),
            media_type: MediaType::Video,
            file_name: Some("video.mp4".into()),
            mime_type: Some("video/mp4".into()),
            file_size: Some(1_048_576),
        };
        let json = serde_json::to_string(&att).unwrap();
        let back: MediaAttachment = serde_json::from_str(&json).unwrap();
        assert_eq!(back.url, "https://example.com/video.mp4");
        assert!(matches!(back.media_type, MediaType::Video));
        assert_eq!(back.file_name, Some("video.mp4".into()));
        assert_eq!(back.mime_type, Some("video/mp4".into()));
        assert_eq!(back.file_size, Some(1_048_576));
    }

    // ── 12. TokenUsage standalone roundtrip ──

    #[test]
    fn token_usage_roundtrip() {
        let usage = TokenUsage {
            input_tokens: 500,
            output_tokens: 200,
            ..Default::default()
        };
        let json = serde_json::to_string(&usage).unwrap();
        let back: TokenUsage = serde_json::from_str(&json).unwrap();
        assert_eq!(back.input_tokens, 500);
        assert_eq!(back.output_tokens, 200);
    }

    // ── 13. ChannelActionDto standalone roundtrip ──

    #[test]
    fn channel_action_dto_roundtrip() {
        let action = ChannelActionDto {
            action: "send_voice".into(),
            params: json!({"text": "Hello world", "voice": "clone:Agent1"}),
            context: json!({"chat_id": 12345, "message_id": 67}),
        };
        let json = serde_json::to_string(&action).unwrap();
        let back: ChannelActionDto = serde_json::from_str(&json).unwrap();
        assert_eq!(back.action, "send_voice");
        assert_eq!(back.params["text"], "Hello world");
        assert_eq!(back.params["voice"], "clone:Agent1");
        assert_eq!(back.context["chat_id"], 12345);
        assert_eq!(back.context["message_id"], 67);
    }

    #[test]
    fn channel_action_dto_with_empty_context() {
        let action = ChannelActionDto {
            action: "pin".into(),
            params: json!({}),
            context: json!(null),
        };
        let json = serde_json::to_string(&action).unwrap();
        let back: ChannelActionDto = serde_json::from_str(&json).unwrap();
        assert_eq!(back.action, "pin");
        assert!(back.params.as_object().unwrap().is_empty());
        assert!(back.context.is_null());
    }

    // ── 15. ChannelInbound remaining variants ──

    #[test]
    fn channel_inbound_action_result_roundtrip() {
        let original = ChannelInbound::ActionResult {
            action_id: "act-99".into(),
            success: true,
            error: None,
        };
        let json = serde_json::to_string(&original).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "action_result");
        let back: ChannelInbound = serde_json::from_str(&json).unwrap();
        match back {
            ChannelInbound::ActionResult { action_id, success, error } => {
                assert_eq!(action_id, "act-99");
                assert!(success);
                assert!(error.is_none());
            }
            other => panic!("expected ActionResult, got {:?}", other),
        }
    }

    #[test]
    fn channel_inbound_action_result_with_error() {
        let original = ChannelInbound::ActionResult {
            action_id: "act-fail".into(),
            success: false,
            error: Some("permission denied".into()),
        };
        let json = serde_json::to_string(&original).unwrap();
        let back: ChannelInbound = serde_json::from_str(&json).unwrap();
        match back {
            ChannelInbound::ActionResult { action_id, success, error } => {
                assert_eq!(action_id, "act-fail");
                assert!(!success);
                assert_eq!(error, Some("permission denied".into()));
            }
            other => panic!("expected ActionResult, got {:?}", other),
        }
    }

    #[test]
    fn channel_inbound_pairing_create_roundtrip() {
        let original = ChannelInbound::PairingCreate {
            request_id: "pc-1".into(),
            user_id: "new-user-555".into(),
            display_name: Some("Charlie".into()),
        };
        let json = serde_json::to_string(&original).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "pairing_create");
        let back: ChannelInbound = serde_json::from_str(&json).unwrap();
        match back {
            ChannelInbound::PairingCreate { request_id, user_id, display_name } => {
                assert_eq!(request_id, "pc-1");
                assert_eq!(user_id, "new-user-555");
                assert_eq!(display_name, Some("Charlie".into()));
            }
            other => panic!("expected PairingCreate, got {:?}", other),
        }
    }

    #[test]
    fn channel_inbound_pairing_create_without_display_name() {
        let original = ChannelInbound::PairingCreate {
            request_id: "pc-2".into(),
            user_id: "anon-user".into(),
            display_name: None,
        };
        let json = serde_json::to_string(&original).unwrap();
        let back: ChannelInbound = serde_json::from_str(&json).unwrap();
        match back {
            ChannelInbound::PairingCreate { request_id, user_id, display_name } => {
                assert_eq!(request_id, "pc-2");
                assert_eq!(user_id, "anon-user");
                assert_eq!(display_name, None);
            }
            other => panic!("expected PairingCreate, got {:?}", other),
        }
    }

    #[test]
    fn channel_inbound_pairing_approve_roundtrip() {
        let original = ChannelInbound::PairingApprove {
            request_id: "pa-1".into(),
            code: "ABC123".into(),
        };
        let json = serde_json::to_string(&original).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "pairing_approve");
        let back: ChannelInbound = serde_json::from_str(&json).unwrap();
        match back {
            ChannelInbound::PairingApprove { request_id, code } => {
                assert_eq!(request_id, "pa-1");
                assert_eq!(code, "ABC123");
            }
            other => panic!("expected PairingApprove, got {:?}", other),
        }
    }

    #[test]
    fn channel_inbound_pairing_reject_roundtrip() {
        let original = ChannelInbound::PairingReject {
            request_id: "pr-1".into(),
            code: "XYZ789".into(),
        };
        let json = serde_json::to_string(&original).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "pairing_reject");
        let back: ChannelInbound = serde_json::from_str(&json).unwrap();
        match back {
            ChannelInbound::PairingReject { request_id, code } => {
                assert_eq!(request_id, "pr-1");
                assert_eq!(code, "XYZ789");
            }
            other => panic!("expected PairingReject, got {:?}", other),
        }
    }

    // ── 16. ChannelOutbound remaining variants ──

    #[test]
    fn channel_outbound_phase_roundtrip() {
        let original = ChannelOutbound::Phase {
            request_id: "r-phase".into(),
            phase: "thinking".into(),
            tool_name: Some("searxng_search".into()),
        };
        let json = serde_json::to_string(&original).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "phase");
        let back: ChannelOutbound = serde_json::from_str(&json).unwrap();
        match back {
            ChannelOutbound::Phase { request_id, phase, tool_name } => {
                assert_eq!(request_id, "r-phase");
                assert_eq!(phase, "thinking");
                assert_eq!(tool_name, Some("searxng_search".into()));
            }
            other => panic!("expected Phase, got {:?}", other),
        }
    }

    #[test]
    fn channel_outbound_phase_without_tool_name() {
        let original = ChannelOutbound::Phase {
            request_id: "r-phase2".into(),
            phase: "planning".into(),
            tool_name: None,
        };
        let json = serde_json::to_string(&original).unwrap();
        let back: ChannelOutbound = serde_json::from_str(&json).unwrap();
        match back {
            ChannelOutbound::Phase { request_id, phase, tool_name } => {
                assert_eq!(request_id, "r-phase2");
                assert_eq!(phase, "planning");
                assert_eq!(tool_name, None);
            }
            other => panic!("expected Phase, got {:?}", other),
        }
    }

    #[test]
    fn channel_outbound_pairing_code_roundtrip() {
        let original = ChannelOutbound::PairingCode {
            request_id: "pc-out-1".into(),
            code: "PAIR-42".into(),
        };
        let json = serde_json::to_string(&original).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "pairing_code");
        let back: ChannelOutbound = serde_json::from_str(&json).unwrap();
        match back {
            ChannelOutbound::PairingCode { request_id, code } => {
                assert_eq!(request_id, "pc-out-1");
                assert_eq!(code, "PAIR-42");
            }
            other => panic!("expected PairingCode, got {:?}", other),
        }
    }

    #[test]
    fn channel_outbound_pairing_result_success_roundtrip() {
        let original = ChannelOutbound::PairingResult {
            request_id: "pr-out-1".into(),
            success: true,
            error: None,
        };
        let json = serde_json::to_string(&original).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "pairing_result");
        let back: ChannelOutbound = serde_json::from_str(&json).unwrap();
        match back {
            ChannelOutbound::PairingResult { request_id, success, error } => {
                assert_eq!(request_id, "pr-out-1");
                assert!(success);
                assert!(error.is_none());
            }
            other => panic!("expected PairingResult, got {:?}", other),
        }
    }

    #[test]
    fn channel_outbound_pairing_result_failure_roundtrip() {
        let original = ChannelOutbound::PairingResult {
            request_id: "pr-out-2".into(),
            success: false,
            error: Some("code expired".into()),
        };
        let json = serde_json::to_string(&original).unwrap();
        let back: ChannelOutbound = serde_json::from_str(&json).unwrap();
        match back {
            ChannelOutbound::PairingResult { request_id, success, error } => {
                assert_eq!(request_id, "pr-out-2");
                assert!(!success);
                assert_eq!(error, Some("code expired".into()));
            }
            other => panic!("expected PairingResult, got {:?}", other),
        }
    }

    #[test]
    fn channel_outbound_reload_roundtrip() {
        let json = serde_json::to_string(&ChannelOutbound::Reload).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "reload");
        assert_eq!(v.as_object().unwrap().len(), 1);
        let back: ChannelOutbound = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, ChannelOutbound::Reload));
    }

    #[test]
    fn channel_outbound_config_roundtrip() {
        let original = ChannelOutbound::Config {
            language: "ru".into(),
            owner_id: Some("123456789".into()),
            typing_mode: "thinking".into(),
        };
        let json = serde_json::to_string(&original).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "config");
        assert_eq!(v["language"], "ru");
        assert_eq!(v["owner_id"], "123456789");
        assert_eq!(v["typing_mode"], "thinking");
        let back: ChannelOutbound = serde_json::from_str(&json).unwrap();
        match back {
            ChannelOutbound::Config { language, owner_id, typing_mode } => {
                assert_eq!(language, "ru");
                assert_eq!(owner_id, Some("123456789".into()));
                assert_eq!(typing_mode, "thinking");
            }
            other => panic!("expected Config, got {:?}", other),
        }
    }

    #[test]
    fn channel_outbound_config_defaults() {
        // Deserialize config with missing optional fields — owner_id defaults to None,
        // typing_mode defaults to "instant"
        let json = r#"{"type":"config","language":"en"}"#;
        let back: ChannelOutbound = serde_json::from_str(json).unwrap();
        match back {
            ChannelOutbound::Config { language, owner_id, typing_mode } => {
                assert_eq!(language, "en");
                assert_eq!(owner_id, None);
                assert_eq!(typing_mode, "instant");
            }
            other => panic!("expected Config, got {:?}", other),
        }
    }

    // ── 17. ToolDefinition roundtrip ──

    #[test]
    fn tool_definition_roundtrip() {
        let td = ToolDefinition {
            name: "get_weather".into(),
            description: "Get current weather for a city".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "city": {"type": "string"},
                    "units": {"type": "string", "enum": ["metric", "imperial"]}
                },
                "required": ["city"]
            }),
        };
        let json = serde_json::to_string(&td).unwrap();
        let back: ToolDefinition = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name, "get_weather");
        assert_eq!(back.description, "Get current weather for a city");
        assert_eq!(back.input_schema["properties"]["city"]["type"], "string");
        assert_eq!(back.input_schema["required"][0], "city");
    }

    // ── 18. IncomingMessageDto serde roundtrip ──

    #[test]
    fn incoming_message_dto_roundtrip() {
        let now = Utc::now();
        let dto = IncomingMessageDto {
            user_id: "u-roundtrip".into(),
            display_name: Some("TestUser".into()),
            text: Some("hello from dto".into()),
            attachments: vec![
                MediaAttachment {
                    url: "https://example.com/a.jpg".into(),
                    media_type: MediaType::Image,
                    file_name: Some("a.jpg".into()),
                    mime_type: None,
                    file_size: None,
                },
            ],
            context: json!({"chat_id": 1, "thread_id": 2}),
            timestamp: now,
        };
        let json = serde_json::to_string(&dto).unwrap();
        let back: IncomingMessageDto = serde_json::from_str(&json).unwrap();
        assert_eq!(back.user_id, "u-roundtrip");
        assert_eq!(back.display_name, Some("TestUser".into()));
        assert_eq!(back.text, Some("hello from dto".into()));
        assert_eq!(back.attachments.len(), 1);
        assert_eq!(back.attachments[0].url, "https://example.com/a.jpg");
        assert_eq!(back.context["chat_id"], 1);
        assert_eq!(back.timestamp, now);
    }

    #[test]
    fn incoming_message_dto_defaults_for_missing_optional() {
        // display_name, attachments, context all have #[serde(default)]
        let json = r#"{"user_id":"u1","text":null,"timestamp":"2026-01-01T00:00:00Z"}"#;
        let dto: IncomingMessageDto = serde_json::from_str(json).unwrap();
        assert_eq!(dto.user_id, "u1");
        assert_eq!(dto.display_name, None);
        assert_eq!(dto.text, None);
        assert!(dto.attachments.is_empty());
        assert!(dto.context.is_null()); // serde_json::Value defaults to Null
    }

    #[test]
    fn thinking_block_roundtrip() {
        let tb = ThinkingBlock {
            thinking: "some reasoning".to_string(),
            signature: "sig_abc123".to_string(),
        };
        let json = serde_json::to_string(&tb).unwrap();
        let back: ThinkingBlock = serde_json::from_str(&json).unwrap();
        assert_eq!(back.thinking, "some reasoning");
        assert_eq!(back.signature, "sig_abc123");
    }

    #[test]
    fn llm_response_thinking_blocks_default_empty() {
        let r: LlmResponse = serde_json::from_str(r#"{"content":"hi","tool_calls":[]}"#).unwrap();
        assert!(r.thinking_blocks.is_empty());
    }

    // ── chat_scope extraction (T03 triage Point 5) ──

    fn incoming_with_context(ctx: serde_json::Value) -> IncomingMessage {
        IncomingMessage {
            user_id: "u1".to_string(),
            context: ctx,
            text: Some("hi".to_string()),
            attachments: vec![],
            agent_id: "agent".to_string(),
            channel: "telegram".to_string(),
            timestamp: Utc::now(),
            formatting_prompt: None,
            tool_policy_override: None,
            leaf_message_id: None,
            user_message_id: None,
        }
    }

    #[test]
    fn chat_scope_extracts_telegram_chat_id_as_string() {
        let msg = incoming_with_context(json!({"chat_id": 12345, "message_id": 7}));
        assert_eq!(msg.chat_scope(), Some("12345".to_string()));
    }

    #[test]
    fn chat_scope_different_chat_ids_are_different_scopes() {
        let a = incoming_with_context(json!({"chat_id": 100}));
        let b = incoming_with_context(json!({"chat_id": 200}));
        assert_ne!(a.chat_scope(), b.chat_scope(), "different chat_id must yield different chat_scope");
    }

    #[test]
    fn chat_scope_discord_combines_guild_and_channel() {
        let msg = incoming_with_context(json!({"guild_id": "g1", "channel_id": "c1", "thread_id": "t1"}));
        assert_eq!(msg.chat_scope(), Some("g1:c1".to_string()));
    }

    #[test]
    fn chat_scope_discord_dm_has_no_guild() {
        // Discord DM channels have no guild_id.
        let msg = incoming_with_context(json!({"channel_id": "c1"}));
        assert_eq!(msg.chat_scope(), Some("c1".to_string()));
    }

    #[test]
    fn chat_scope_matrix_room_id() {
        let msg = incoming_with_context(json!({"room_id": "!abc:matrix.org", "event_id": "$xyz"}));
        assert_eq!(msg.chat_scope(), Some("!abc:matrix.org".to_string()));
    }

    #[test]
    fn chat_scope_slack_channel_field() {
        let msg = incoming_with_context(json!({"channel": "C123", "ts": "1.1"}));
        assert_eq!(msg.chat_scope(), Some("C123".to_string()));
    }

    #[test]
    fn chat_scope_none_for_null_context() {
        // Web/UI/cron callers: context is Value::Null.
        let msg = incoming_with_context(serde_json::Value::Null);
        assert_eq!(msg.chat_scope(), None, "null context must degrade to None, never panic");
    }

    #[test]
    fn chat_scope_none_when_no_known_fields_present() {
        // WhatsApp/email: no chat concept, user_id already IS the peer.
        let msg = incoming_with_context(json!({"phone_number_id": "p1", "wa_id": "w1"}));
        assert_eq!(msg.chat_scope(), None);
    }

    #[test]
    fn chat_scope_empty_object_is_none() {
        let msg = incoming_with_context(json!({}));
        assert_eq!(msg.chat_scope(), None);
    }

    #[test]
    fn incoming_message_dto_chat_scope_matches_incoming_message() {
        // The dispatcher computes SessionKey from the DTO (before
        // into_incoming); the engine computes it from IncomingMessage
        // (after). They MUST agree for the same wire payload.
        let ctx = json!({"chat_id": 555});
        let dto = channels::IncomingMessageDto {
            user_id: "u1".to_string(),
            display_name: None,
            text: Some("hi".to_string()),
            attachments: vec![],
            context: ctx.clone(),
            timestamp: Utc::now(),
        };
        let dto_scope = dto.chat_scope();
        let incoming = dto.into_incoming("agent".to_string(), "telegram".to_string(), None);
        assert_eq!(dto_scope, incoming.chat_scope());
        assert_eq!(dto_scope, Some("555".to_string()));
    }
}
