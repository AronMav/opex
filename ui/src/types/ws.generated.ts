// @generated — do not edit by hand.
// Source of truth: types annotated with #[ts(export)] in crates/opex-core/.
// Regenerate with: make gen-types

export type NotificationReadData = { id: string, unread_count: number, };

export type NotificationsReadAllData = { unread_count: number, };

export type WsEvent = { "type": "notification", data: import("./api").NotificationRow, } | { "type": "notification_read", data: NotificationReadData, } | { "type": "notifications_read_all", data: NotificationsReadAllData, } | { "type": "notifications_cleared" } | { "type": "agent_processing", agent: string, 
/**
 * "start" | "end"
 */
status: string, session_id: string | null, channel: string | null, } | { "type": "approval_requested", approval_id: string, agent: string, tool_name: string, } | { "type": "approval_resolved", approval_id: string, agent: string, 
/**
 * "approved" | "rejected"
 */
status: string, } | { "type": "session_updated", agent: string, session_id: string | null, channel: string | null, } | { "type": "agent_joined", agent_name: string, session_id: string, invited_by: string, participants: Array<string>, } | { "type": "file_job_progress", job_id: string, handler_id: string, session_id: string, phase: string, pct: number, status: string, } | { "type": "file", url: string, mediaType: string, 
/**
 * Optional display name (e.g. "transcript.txt"); omitted when unknown.
 */
filename: string | null, } | { "type": "canvas_update", agent: string, action: string, content_type: string | null, content: string | null, title: string | null, } | { "type": "channels_changed", agent: string, } | { "type": "log", level: string, target: string, message: string, timestamp: string, } | { "type": "audit_event", event_type: string, agent: string, details: Record<string, unknown>, } | { "type": "goal-turn", sessionId: string, } | { "type": "pong" };
