// @generated — do not edit by hand.
// Source of truth: types annotated with #[ts(export)] in crates/hydeclaw-core/.
// Regenerate with: make gen-types

export type ChannelActionDto = { 
/**
 * Action name: "react", "pin", "unpin", "edit", "delete", "reply",
 * "`send_message`", "`send_voice`", etc.
 */
action: string, 
/**
 * Action-specific parameters (e.g. {"emoji": "👍"}, {"text": "..."}).
 */
params: unknown, 
/**
 * Opaque context echoed from the original message (e.g. {"`chat_id"`: 123, "`message_id"`: 42}).
 */
context: unknown, };

export type ChannelInbound = { "type": "message", request_id: string, msg: IncomingMessageDto, } | { "type": "action_result", action_id: string, success: boolean, error: string | null, } | { "type": "access_check", request_id: string, user_id: string, } | { "type": "pairing_create", request_id: string, user_id: string, display_name: string | null, } | { "type": "pairing_approve", request_id: string, code: string, } | { "type": "pairing_reject", request_id: string, code: string, } | { "type": "ping" } | { "type": "ready", adapter_type: string, version: string, 
/**
 * Channel-specific formatting instructions for the LLM system prompt.
 */
formatting_prompt?: string | null, } | { "type": "cancel", request_id: string, };

export type ChannelOutbound = { "type": "chunk", request_id: string, text: string, } | { "type": "phase", request_id: string, phase: string, tool_name: string | null, } | { "type": "done", request_id: string, text: string, } | { "type": "error", request_id: string, message: string, } | { "type": "action", action_id: string, action: ChannelActionDto, } | { "type": "access_result", request_id: string, allowed: boolean, is_owner: boolean, } | { "type": "pairing_code", request_id: string, code: string, } | { "type": "pairing_result", request_id: string, success: boolean, error: string | null, } | { "type": "pong" } | { "type": "reload" } | { "type": "config", 
/**
 * Agent language code (e.g., "ru", "en").
 */
language: string, 
/**
 * Owner user ID string (for showing pairing UI to the right person).
 */
owner_id?: string | null, 
/**
 * Typing indicator mode: "instant", "thinking", "message", "never".
 */
typing_mode: string, };

export type IncomingMessageDto = { user_id: string, 
/**
 * Optional display name for the user (shown in pairing notifications, etc.).
 */
display_name?: string | null, text: string | null, 
/**
 * Media attachments (photos, audio, video, documents).
 */
attachments: Array<MediaAttachment>, 
/**
 * Opaque context from the adapter. Core echoes it back with Done/Error/Action responses.
 */
context: unknown, timestamp: string, };

export type MediaAttachment = { url: string, media_type: MediaType, file_name?: string | null, mime_type?: string | null, file_size?: number | null, };

export type MediaType = "image" | "audio" | "video" | "document";
