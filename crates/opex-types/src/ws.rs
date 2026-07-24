//! Global UI WebSocket event bus — single typed source of truth.
//!
//! Mirrors the historical `json!({"type": ...})` wire format 1:1, verified
//! field-by-field against every `ui_event_tx.send(...)` / `ui_event_tx`
//! broadcast call site as of T7 (see `.superpowers/sdd/task-7-report.md`
//! for the full inventory). This module does NOT migrate the send sites —
//! that is Task 8's job. Producing `WsEvent::to_json()` output here must
//! byte-for-byte match what those (still ad-hoc) `json!` sites emit today.
//!
//! Deviations from a naive "one struct per site" reading are documented
//! inline below (fields present at only SOME send sites are `Option` +
//! `skip_serializing_if`; fields no site currently emits are dropped).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-gen", derive(ts_rs::TS))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WsEvent {
    /// `gateway/handlers/notifications.rs::notify()` (line ~307) — persisted
    /// notification, broadcast to every open tab unless muted.
    Notification {
        #[cfg_attr(feature = "ts-gen", ts(type = "import(\"./api\").NotificationRow"))]
        data: serde_json::Value,
    },
    /// `notifications.rs:46` (`notification_read_event`) — cross-tab
    /// read-state reconciliation.
    NotificationRead { data: NotificationReadData },
    /// `notifications.rs:53` (`notifications_read_all_event`)
    NotificationsReadAll { data: NotificationsReadAllData },
    /// `notifications.rs:60` (`notifications_cleared_event`) — no `data` field.
    NotificationsCleared,
    /// Two sites: `agent/pipeline/bootstrap.rs:196` sends the "start" event,
    /// always with `session_id` and `channel`. `agent/engine/stream.rs:74`
    /// (`ProcessingGuard::drop`) sends the "end" event, with `session_id`
    /// only if the guard was constructed with one and `channel` never set.
    /// No current site emits a `phase` field.
    AgentProcessing {
        agent: String,
        /// "start" | "end"
        status: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        channel: Option<String>,
    },
    /// `agent/approval_manager.rs:115` — the tool-name field is `tool_name`
    /// on the wire (NOT `tool`); no `arguments` field is ever sent.
    ApprovalRequested {
        approval_id: String,
        agent: String,
        tool_name: String,
    },
    /// `agent/pipeline/approval.rs:52`
    ApprovalResolved {
        approval_id: String,
        agent: String,
        /// "approved" | "rejected"
        status: String,
    },
    /// Four call sites disagree on which optional field they carry:
    /// `scheduler/mod.rs:1695`, `gateway/handlers/chat/sse.rs:317`, and
    /// `gateway/handlers/channel_ws/reader.rs:196` all send `{agent, channel}`
    /// (no `session_id`); `opex-db/sessions.rs:1758` (`add_participant`)
    /// sends `{agent, session_id}` (no `channel`). No site sends both, so
    /// both are optional here.
    SessionUpdated {
        agent: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        channel: Option<String>,
    },
    /// `gateway/handlers/sessions.rs:577` — full field set verified
    /// verbatim: `agent_name` + `session_id` + `invited_by` (always the
    /// literal `"user"` today) + `participants`.
    AgentJoined {
        agent_name: String,
        session_id: String,
        invited_by: String,
        participants: Vec<String>,
    },
    /// `gateway/handlers/files.rs:809` (`file_job_progress_event`, shared by
    /// the progress callback and the terminal done/failed callback). `pct`
    /// is `i32` on the wire (source `JobProgressBody.pct: i32` / literal
    /// `100`), not `u8`.
    FileJobProgress {
        job_id: String,
        handler_id: String,
        session_id: String,
        phase: String,
        pct: i32,
        status: String,
    },
    /// `gateway/handlers/files.rs:786`
    File {
        url: String,
        #[serde(rename = "mediaType")]
        media_type: String,
        /// Optional display name (e.g. "transcript.txt"); omitted when unknown.
        #[serde(skip_serializing_if = "Option::is_none")]
        filename: Option<String>,
    },
    /// `agent/pipeline/canvas.rs:35` (present/push_data — `content_type` +
    /// `content` + `title` all present, `title` possibly explicit `null`)
    /// vs `:50` (clear — only `type`/`agent`/`action`, the other three keys
    /// are absent entirely). All three optional here so one variant covers
    /// both real shapes.
    CanvasUpdate {
        agent: String,
        action: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        content_type: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        content: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        title: Option<String>,
    },
    /// `gateway/handlers/channel_ws/handshake.rs:101` +
    /// `gateway/handlers/channel_ws/mod.rs:106` — `agent` is always present
    /// on both current sites (required, not `Option`).
    ChannelsChanged { agent: String },
    /// `main.rs:102` (`BroadcastLogLayer::on_event`)
    Log {
        level: String,
        target: String,
        message: String,
        timestamp: String,
    },
    /// Forward wire reserve — NOT dead code. No backend send site constructs
    /// this event yet, but the consumer half is fully wired:
    /// `ui/src/app/(authenticated)/monitor/page.tsx` subscribes to
    /// `"audit_event"` and performs a real live-refresh (`auditRefetch`) on
    /// receipt. Kept so a future backend emitter (e.g. an `audit_queue` WS
    /// notify) lights up the monitor's live audit tail without a frontend
    /// change. Only the producer is pending.
    AuditEvent {
        event_type: String,
        agent: String,
        #[cfg_attr(feature = "ts-gen", ts(type = "Record<string, unknown>"))]
        details: serde_json::Value,
    },
    /// `agent/goal/driver.rs:352` — both the tag (`goal-turn`) and the field
    /// (`sessionId`) are non-snake-case on the wire (pre-existing shape).
    #[serde(rename = "goal-turn")]
    GoalTurn {
        #[serde(rename = "sessionId")]
        session_id: String,
    },
    /// `gateway/handlers/channel_ws/mod.rs:374` (`WsServerMessage::Pong`) —
    /// sent directly (not via `ui_event_tx`) over the same UI WebSocket
    /// connection in reply to a client `{"type":"ping"}`; the frontend
    /// parses it through the same event union, so it belongs on this bus.
    Pong,
}

/// `NotificationRead` payload — `notifications.rs::notification_read_event`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-gen", derive(ts_rs::TS))]
pub struct NotificationReadData {
    pub id: String,
    #[cfg_attr(feature = "ts-gen", ts(type = "number"))]
    pub unread_count: i64,
}

/// `NotificationsReadAll` payload — `notifications.rs::notifications_read_all_event`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-gen", derive(ts_rs::TS))]
pub struct NotificationsReadAllData {
    #[cfg_attr(feature = "ts-gen", ts(type = "number"))]
    pub unread_count: i64,
}

impl WsEvent {
    /// Serialize for the broadcast bus (`ui_event_tx: Sender<String>`).
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}
