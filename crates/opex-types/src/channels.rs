//! Channel WS protocol types — Core ↔ Channel adapter (Telegram/Discord/etc.)
//! over WebSocket loopback.
//!
//! Source of truth for the channel wire protocol. Codegen'd to TypeScript
//! via ts-rs (dest = "channels", → channels/src/types.generated.ts).
//!
//! Wire format invariant: ChannelInbound/Outbound are Serde-tagged enums
//! (`#[serde(tag = "type")]`); ts-rs preserves the same shape.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{default_typing_mode, IncomingMessage};

// ── Media attachments ──

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-gen", derive(ts_rs::TS))]
#[serde(rename_all = "lowercase")]
pub enum MediaType {
    Image,
    Audio,
    Video,
    Document,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-gen", derive(ts_rs::TS))]
pub struct MediaAttachment {
    pub url: String,
    pub media_type: MediaType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    // ts-rs v12 renders bare `u64` as `number`, but `Option<u64>` as
    // `bigint | null`. Channel adapter drivers (discord/slack/telegram) source
    // file size as a JS `number`, so we override the TS surface here to
    // `number | null` for adapter compatibility while keeping Rust-side u64
    // capacity. JS Number.MAX_SAFE_INTEGER ≈ 9 PB — safe for file sizes;
    // sources >9 PB would need `Option<String>` with a custom serde adapter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-gen", ts(type = "number | null"))]
    pub file_size: Option<u64>,
}

// ── Channel Connector Protocol (Core ↔ Adapter over WebSocket) ──

/// Serializable version of `IncomingMessage` for transport over WebSocket.
/// The `context` field is opaque to core — set by the adapter (e.g. `chat_id`, `message_id`)
/// and echoed back unchanged in replies/actions.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-gen", derive(ts_rs::TS))]
pub struct IncomingMessageDto {
    pub user_id: String,
    /// Optional display name for the user (shown in pairing notifications, etc.).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Media attachments (photos, audio, video, documents).
    #[serde(default)]
    pub attachments: Vec<MediaAttachment>,
    /// Opaque context from the adapter. Core echoes it back with Done/Error/Action responses.
    #[serde(default)]
    #[cfg_attr(feature = "ts-gen", ts(type = "unknown"))]
    pub context: serde_json::Value,
    pub timestamp: DateTime<Utc>,
}

impl IncomingMessageDto {
    /// Per-chat/group/thread disambiguator from the adapter's opaque
    /// `context` — see [`crate::context_chat_scope`]. Used by the channel WS
    /// dispatcher to compute `SessionKey` BEFORE `into_incoming` consumes
    /// `self` (T03 triage Point 5).
    #[must_use]
    pub fn chat_scope(&self) -> Option<String> {
        crate::context_chat_scope(&self.context)
    }

    /// Convert to the internal `IncomingMessage` used by the engine.
    #[must_use]
    pub fn into_incoming(self, agent_id: String, channel: String, formatting_prompt: Option<String>) -> IncomingMessage {
        IncomingMessage {
            user_id: self.user_id,
            context: self.context,
            text: self.text,
            attachments: self.attachments,
            agent_id,
            channel,
            timestamp: self.timestamp,
            formatting_prompt,
            tool_policy_override: None,
            leaf_message_id: None,
            user_message_id: None,
        }
    }
}

/// Generic channel action for transport over WebSocket.
/// Channel-agnostic: `action` is a string name, `params` and `context` are opaque JSON.
/// The adapter interprets `action`/`params` and uses `context` to know where to send.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-gen", derive(ts_rs::TS))]
pub struct ChannelActionDto {
    /// Action name: "react", "pin", "unpin", "edit", "delete", "reply",
    /// "`send_message`", "`send_voice`", etc.
    pub action: String,
    /// Action-specific parameters (e.g. {"emoji": "👍"}, {"text": "..."}).
    #[cfg_attr(feature = "ts-gen", ts(type = "unknown"))]
    pub params: serde_json::Value,
    /// Opaque context echoed from the original message (e.g. {"`chat_id"`: 123, "`message_id"`: 42}).
    #[cfg_attr(feature = "ts-gen", ts(type = "unknown"))]
    pub context: serde_json::Value,
}

/// # Channel adapter handshake protocol
///
/// On WebSocket connect, the adapter MUST send `Ready { adapter_type, version, formatting_prompt? }` FIRST.
/// Core replies with `Config { language, owner_id?, typing_mode }`. The adapter MUST wait for the `Config`
/// message before sending any `Message` events — otherwise the agent has no language preference and may format
/// replies incorrectly.
///
/// ## Handshake sequence (adapter ⇄ core)
///
/// 1. Adapter → `Ready { adapter_type, version, formatting_prompt? }`
/// 2. Core → `Config { language, owner_id?, typing_mode }`
/// 3. Adapter may then send: `Message`, `AccessCheck`, `PairingCreate` (any time)
/// 4. Core sends in response: `Chunk`, `Phase`, `Done`, `Error`, `Action` (streaming/result)
/// 5. Either side may send: `Ping` / `Pong` (heartbeat)
/// 6. Either side may send: `Cancel(request_id)` to abort an in-flight message
///
/// Core also sends `Reload` to force agent re-discovery when configuration changes.
///
/// Messages from channel adapter to core.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-gen", derive(ts_rs::TS))]
#[serde(tag = "type")]
pub enum ChannelInbound {
    /// New message from a channel user.
    #[serde(rename = "message")]
    Message {
        request_id: String,
        msg: IncomingMessageDto,
    },
    /// Result of executing a channel action (react, pin, edit, etc.).
    #[serde(rename = "action_result")]
    ActionResult {
        action_id: String,
        success: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// Check if a user is allowed to interact with the agent.
    #[serde(rename = "access_check")]
    AccessCheck {
        request_id: String,
        user_id: String,
    },
    /// Create a pairing code for an unauthorized user.
    #[serde(rename = "pairing_create")]
    PairingCreate {
        request_id: String,
        user_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        display_name: Option<String>,
    },
    /// Approve a pending pairing by code (owner command).
    #[serde(rename = "pairing_approve")]
    PairingApprove {
        request_id: String,
        code: String,
    },
    /// Reject a pending pairing by code (owner command).
    #[serde(rename = "pairing_reject")]
    PairingReject {
        request_id: String,
        code: String,
    },
    /// Keepalive ping.
    #[serde(rename = "ping")]
    Ping,
    /// Adapter announces readiness after connection.
    #[serde(rename = "ready")]
    Ready {
        adapter_type: String,
        version: String,
        /// Channel-specific formatting instructions for the LLM system prompt.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        formatting_prompt: Option<String>,
    },
    /// Cancel an in-flight request (e.g. /stop command).
    #[serde(rename = "cancel")]
    Cancel {
        request_id: String,
    },
}

/// Messages from core to channel adapter.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-gen", derive(ts_rs::TS))]
#[serde(tag = "type")]
pub enum ChannelOutbound {
    /// Streaming text chunk for a request.
    #[serde(rename = "chunk")]
    Chunk {
        request_id: String,
        text: String,
    },
    /// Processing phase update (for status indicators like reactions).
    #[serde(rename = "phase")]
    Phase {
        request_id: String,
        phase: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        tool_name: Option<String>,
    },
    /// Final response complete.
    #[serde(rename = "done")]
    Done {
        request_id: String,
        text: String,
    },
    /// Error processing the request.
    #[serde(rename = "error")]
    Error {
        request_id: String,
        message: String,
    },
    /// Channel action request (react, pin, edit, delete, reply, send, etc.).
    /// The `action` name and `params` are channel-agnostic strings/JSON.
    /// The `context` is the same opaque value received from the adapter's message.
    #[serde(rename = "action")]
    Action {
        action_id: String,
        action: ChannelActionDto,
    },
    /// Response to an access check.
    #[serde(rename = "access_result")]
    AccessResult {
        request_id: String,
        allowed: bool,
        is_owner: bool,
    },
    /// Pairing code for an unauthorized user.
    #[serde(rename = "pairing_code")]
    PairingCode {
        request_id: String,
        code: String,
    },
    /// Result of a pairing approve/reject operation.
    #[serde(rename = "pairing_result")]
    PairingResult {
        request_id: String,
        success: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// Pong response to keepalive.
    #[serde(rename = "pong")]
    Pong,
    /// Forward wire reserve — NOT dead code. No core send site currently
    /// constructs this, but the consumer half is wired: the channel adapter
    /// handles it (`channels/src/session.ts` performs a clean session
    /// teardown; `bridge.ts` acknowledges the type). Intended for "core asks
    /// the adapter to re-discover agents after a config change (create /
    /// update / delete)". Kept so a future emitter lights it up without a
    /// frontend change. Only the producer is pending.
    #[serde(rename = "reload")]
    Reload,
    /// Channel configuration sent by core after adapter Ready.
    /// Contains only non-secret info (language, `owner_id` for access control UI).
    /// Channel secrets (`bot_token`, `api_url`) are read by the adapter from its own env.
    #[serde(rename = "config")]
    Config {
        /// Agent language code (e.g., "ru", "en").
        language: String,
        /// Owner user ID string (for showing pairing UI to the right person).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        owner_id: Option<String>,
        /// Typing indicator mode: "instant", "thinking", "message", "never".
        #[serde(default = "default_typing_mode")]
        typing_mode: String,
    },
}
