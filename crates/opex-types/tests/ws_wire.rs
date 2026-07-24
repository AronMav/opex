//! T7 round-trip fixture generation. Writes JSON fixtures that the
//! UI test consumes — proves Rust serde output matches the ts-rs
//! codegen TS shape for the global UI WebSocket event bus.
//!
//! Run: `cargo test -p opex-types --test ws_wire`
//! Fixtures path: ../../ui/src/__tests__/fixtures/ws/

use opex_types::ws::{NotificationReadData, NotificationsReadAllData, WsEvent};
use std::path::PathBuf;

fn fixtures_dir() -> PathBuf {
    let manifest = env!("CARGO_MANIFEST_DIR");
    PathBuf::from(manifest).join("../../ui/src/__tests__/fixtures/ws")
}

fn write_fixture<T: serde::Serialize + serde::de::DeserializeOwned>(name: &str, value: &T) {
    let json = serde_json::to_string_pretty(value).unwrap();
    let parsed: T = serde_json::from_str(&json).unwrap();
    let json2 = serde_json::to_string_pretty(&parsed).unwrap();
    assert_eq!(json, json2, "round-trip Rust→JSON→Rust must be stable");

    let path = fixtures_dir().join(format!("{name}.json"));
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, &json).unwrap();
    eprintln!("wrote {}", path.display());
}

#[test]
fn ws_notification_fixture() {
    let ev = WsEvent::Notification {
        data: serde_json::json!({
            "id": "11111111-1111-1111-1111-111111111111",
            "type": "tool_approval",
            "title": "Tool Approval Required",
            "body": "Agent Opex wants to run workspace_write",
            "data": {},
            "read": false,
            "created_at": "2026-07-16T12:00:00Z",
        }),
    };
    write_fixture("notification", &ev);
}

#[test]
fn ws_notification_read_fixture() {
    let ev = WsEvent::NotificationRead {
        data: NotificationReadData {
            id: "11111111-1111-1111-1111-111111111111".to_string(),
            unread_count: 3,
        },
    };
    write_fixture("notification_read", &ev);
}

#[test]
fn ws_notifications_read_all_fixture() {
    let ev = WsEvent::NotificationsReadAll {
        data: NotificationsReadAllData { unread_count: 0 },
    };
    write_fixture("notifications_read_all", &ev);
}

#[test]
fn ws_notifications_cleared_fixture() {
    let ev = WsEvent::NotificationsCleared;
    write_fixture("notifications_cleared", &ev);
}

#[test]
fn ws_agent_processing_fixture() {
    let ev = WsEvent::AgentProcessing {
        agent: "Opex".to_string(),
        status: "start".to_string(),
        session_id: Some("22222222-2222-2222-2222-222222222222".to_string()),
        channel: Some("web".to_string()),
    };
    write_fixture("agent_processing", &ev);
}

#[test]
fn ws_approval_requested_fixture() {
    let ev = WsEvent::ApprovalRequested {
        approval_id: "33333333-3333-3333-3333-333333333333".to_string(),
        agent: "Opex".to_string(),
        tool_name: "workspace_write".to_string(),
    };
    write_fixture("approval_requested", &ev);
}

#[test]
fn ws_approval_resolved_fixture() {
    let ev = WsEvent::ApprovalResolved {
        approval_id: "33333333-3333-3333-3333-333333333333".to_string(),
        agent: "Opex".to_string(),
        status: "approved".to_string(),
    };
    write_fixture("approval_resolved", &ev);
}

#[test]
fn ws_session_updated_fixture() {
    let ev = WsEvent::SessionUpdated {
        agent: "Opex".to_string(),
        session_id: None,
        channel: Some("cron".to_string()),
    };
    write_fixture("session_updated", &ev);
}

#[test]
fn ws_agent_joined_fixture() {
    let ev = WsEvent::AgentJoined {
        agent_name: "Helper".to_string(),
        session_id: "44444444-4444-4444-4444-444444444444".to_string(),
        invited_by: "user".to_string(),
        participants: vec!["Opex".to_string(), "Helper".to_string()],
    };
    write_fixture("agent_joined", &ev);
}

#[test]
fn ws_file_job_progress_fixture() {
    let ev = WsEvent::FileJobProgress {
        job_id: "55555555-5555-5555-5555-555555555555".to_string(),
        handler_id: "summarize_video".to_string(),
        session_id: "66666666-6666-6666-6666-666666666666".to_string(),
        phase: "transcribing".to_string(),
        pct: 42,
        status: "processing".to_string(),
    };
    write_fixture("file_job_progress", &ev);
}

#[test]
fn ws_file_fixture() {
    let ev = WsEvent::File {
        url: "/api/uploads/77777777-7777-7777-7777-777777777777?sig=abc&exp=123".to_string(),
        media_type: "image/png".to_string(),
        filename: None,
    };
    write_fixture("file", &ev);
}

#[test]
fn ws_canvas_update_fixture() {
    let ev = WsEvent::CanvasUpdate {
        agent: "Opex".to_string(),
        action: "present".to_string(),
        content_type: Some("markdown".to_string()),
        content: Some("# Hello canvas".to_string()),
        title: Some("Report".to_string()),
    };
    write_fixture("canvas_update", &ev);
}

#[test]
fn ws_channels_changed_fixture() {
    let ev = WsEvent::ChannelsChanged { agent: "Opex".to_string() };
    write_fixture("channels_changed", &ev);
}

#[test]
fn ws_log_fixture() {
    let ev = WsEvent::Log {
        level: "INFO".to_string(),
        target: "opex_core::agent".to_string(),
        message: "agent started".to_string(),
        timestamp: "2026-07-16T12:00:00+00:00".to_string(),
    };
    write_fixture("log", &ev);
}

#[test]
fn ws_audit_event_fixture() {
    let ev = WsEvent::AuditEvent {
        event_type: "approval_requested".to_string(),
        agent: "Opex".to_string(),
        details: serde_json::json!({"tool": "workspace_write", "approval_id": "x"}),
    };
    write_fixture("audit_event", &ev);
}

#[test]
fn ws_goal_turn_fixture() {
    let ev = WsEvent::GoalTurn {
        session_id: "88888888-8888-8888-8888-888888888888".to_string(),
    };
    let json = serde_json::to_string(&ev).unwrap();
    assert!(json.contains("\"goal-turn\""), "{json}");
    assert!(json.contains("\"sessionId\""), "{json}");
    write_fixture("goal-turn", &ev);
}

#[test]
fn ws_pong_fixture() {
    let ev = WsEvent::Pong;
    write_fixture("pong", &ev);
}
