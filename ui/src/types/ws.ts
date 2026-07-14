/** Discriminated union of all WebSocket event types from OPEX backend. */

export interface WsSessionUpdated {
  type: "session_updated";
  session_id: string;
  agent: string;
}

export interface WsAgentProcessing {
  type: "agent_processing";
  agent: string;
  status: "start" | "end";
  phase?: string;
  session_id?: string;
  channel?: string;
}

export interface WsApprovalRequested {
  type: "approval_requested";
  approval_id: string;
  agent: string;
  tool: string;
  arguments: Record<string, unknown>;
}

export interface WsLog {
  type: "log";
  level: "ERROR" | "WARN" | "INFO" | "DEBUG";
  target: string;
  message: string;
  timestamp: string;
}

export interface WsCanvasUpdate {
  type: "canvas_update";
  action: string;
  agent: string;
  content_type: string;
  content: string;
  title?: string;
}

export interface WsChannelsChanged {
  type: "channels_changed";
  agent?: string;
}

export interface WsApprovalResolved {
  type: "approval_resolved";
  approval_id: string;
  agent: string;
  status: "approved" | "rejected" | "timeout";
}

export interface WsAuditEvent {
  type: "audit_event";
  event_type: string;
  agent: string;
  details: Record<string, unknown>;
}

export interface WsPong {
  type: "pong";
}

export interface WsNotification {
  type: "notification";
  // `data.type` (the notification's own event kind) is a plain `string` on
  // `NotificationRow` — see the comment above that type in `./api.ts` for the
  // known values, including "initiative_proposal" (Stage C self-proposed
  // goals, routed to `/agents/{agent}/plan` by notification-bell.tsx).
  data: import("./api").NotificationRow;
}

export interface WsNotificationRead {
  type: "notification_read";
  data: { id: string; unread_count: number };
}

export interface WsNotificationsReadAll {
  type: "notifications_read_all";
  data: { unread_count: number };
}

export interface WsNotificationsCleared {
  type: "notifications_cleared";
}

export interface WsFileJobProgress {
  type: "file_job_progress";
  job_id: string;
  handler_id: string;
  session_id: string;
  phase: string;
  pct: number;
  status: string;
}

/** Union of all known WS event types. */
export type WsEvent =
  | WsSessionUpdated
  | WsAgentProcessing
  | WsApprovalRequested
  | WsLog
  | WsCanvasUpdate
  | WsChannelsChanged
  | WsApprovalResolved
  | WsAuditEvent
  | WsNotification
  | WsNotificationRead
  | WsNotificationsReadAll
  | WsNotificationsCleared
  | WsPong
  | WsFileJobProgress;

/** All valid WS event type strings. */
export type WsEventType = WsEvent["type"];

/** Extract the event interface for a given type string. */
export type WsEventOf<T extends WsEventType> = Extract<WsEvent, { type: T }>;
