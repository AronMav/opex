// ui/src/types/ws.ts — thin re-export over the ts-rs codegen (see crates/opex-types/src/ws.rs).
// Do NOT hand-edit shapes here; fix consumers instead. Regenerate the source with `make gen-types`.

export type { WsEvent } from "./ws.generated";
import type { WsEvent } from "./ws.generated";

export type WsEventType = WsEvent["type"];
export type WsEventOf<T extends WsEventType> = Extract<WsEvent, { type: T }>;

// Back-compat aliases (historical hand-written interface names) — kept so
// existing consumers importing these names keep compiling. Field shapes now
// come from the generated union (wire truth), not from hand-written interfaces.
export type WsSessionUpdated = WsEventOf<"session_updated">;
export type WsAgentProcessing = WsEventOf<"agent_processing">;
export type WsApprovalRequested = WsEventOf<"approval_requested">;
export type WsApprovalResolved = WsEventOf<"approval_resolved">;
export type WsLog = WsEventOf<"log">;
export type WsCanvasUpdate = WsEventOf<"canvas_update">;
export type WsChannelsChanged = WsEventOf<"channels_changed">;
export type WsAuditEvent = WsEventOf<"audit_event">;
export type WsNotification = WsEventOf<"notification">;
export type WsNotificationRead = WsEventOf<"notification_read">;
export type WsNotificationsReadAll = WsEventOf<"notifications_read_all">;
export type WsNotificationsCleared = WsEventOf<"notifications_cleared">;
export type WsFileJobProgress = WsEventOf<"file_job_progress">;
export type WsPong = WsEventOf<"pong">;

// New variants introduced by the generated union (no legacy alias existed before).
export type WsAgentJoined = WsEventOf<"agent_joined">;
export type WsGoalTurn = WsEventOf<"goal-turn">;
export type WsFile = WsEventOf<"file">;
