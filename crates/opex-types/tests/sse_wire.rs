//! S6.5 round-trip fixture generation. Writes JSON fixtures that the
//! UI test consumes — proves Rust serde output matches the ts-rs
//! codegen TS shape.
//!
//! Run: `cargo test -p opex-types --test sse_wire`
//! Fixtures path: ../../ui/src/__tests__/fixtures/sse/

use opex_types::approvals::ApprovalAction;
use opex_types::ids::{ApprovalId, MessageId, ParallelBatchId, ToolCallId};
use opex_types::sse::{
    DataSessionIdPayload, MetricCard, MetricTrend, RichCardData, SseEvent,
    SyncStatus, TableCard, UsagePayload,
};
use std::path::PathBuf;
use uuid::Uuid;

fn fixtures_dir() -> PathBuf {
    let manifest = env!("CARGO_MANIFEST_DIR");
    PathBuf::from(manifest).join("../../ui/src/__tests__/fixtures/sse")
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
fn sse_data_session_id_fixture() {
    let ev = SseEvent::DataSessionId {
        data: DataSessionIdPayload {
            session_id: "sess-abc-123".to_string(),
            context_limit: Some(8000),
        },
        transient: true,
    };
    write_fixture("data-session-id", &ev);
}

#[test]
fn sse_start_fixture() {
    let ev = SseEvent::Start {
        message_id: MessageId::from(Uuid::nil()),
        agent_name: "Opex".to_string(),
    };
    write_fixture("start", &ev);
}

#[test]
fn sse_step_start_fixture() {
    let ev = SseEvent::StepStart {
        step_id: "step_2".to_string(),
        message_id: MessageId::from(Uuid::nil()),
        agent_name: "Opex".to_string(),
    };
    write_fixture("step-start", &ev);
}

#[test]
fn sse_text_start_fixture() {
    let ev = SseEvent::TextStart {
        id: "text-1".to_string(),
        agent_name: "Opex".to_string(),
    };
    write_fixture("text-start", &ev);
}

#[test]
fn sse_text_delta_fixture() {
    let ev = SseEvent::TextDelta {
        id: "text-1".to_string(),
        delta: "Hello, world".to_string(),
    };
    write_fixture("text-delta", &ev);
}

#[test]
fn sse_text_end_fixture() {
    let ev = SseEvent::TextEnd {
        id: "text-1".to_string(),
    };
    write_fixture("text-end", &ev);
}

#[test]
fn sse_tool_input_start_fixture() {
    let ev = SseEvent::ToolInputStart {
        tool_call_id: ToolCallId::from("tc-abc-1".to_string()),
        tool_name: "code_exec".to_string(),
        agent_name: "Opex".to_string(),
        parallel_batch_id: Some(ParallelBatchId::from(Uuid::nil())),
    };
    write_fixture("tool-input-start", &ev);
}

#[test]
fn sse_tool_input_delta_fixture() {
    let ev = SseEvent::ToolInputDelta {
        tool_call_id: ToolCallId::from("tc-abc-1".to_string()),
        input_text_delta: "{\"cmd\": \"ls\"}".to_string(),
    };
    write_fixture("tool-input-delta", &ev);
}

#[test]
fn sse_tool_input_available_fixture() {
    let ev = SseEvent::ToolInputAvailable {
        tool_call_id: ToolCallId::from("tc-abc-1".to_string()),
        tool_name: "code_exec".to_string(),
        input: serde_json::json!({"cmd": "ls"}),
        parallel_batch_id: None,
    };
    write_fixture("tool-input-available", &ev);
}

#[test]
fn sse_tool_output_available_fixture() {
    let ev = SseEvent::ToolOutputAvailable {
        tool_call_id: ToolCallId::from("tc-abc-1".to_string()),
        output: "file1.txt\nfile2.txt".to_string(),
    };
    write_fixture("tool-output-available", &ev);
}

#[test]
fn sse_file_fixture() {
    let ev = SseEvent::File {
        url: "/uploads/x.png".to_string(),
        media_type: "image/png".to_string(),
        filename: None,
    };
    write_fixture("file", &ev);
}

#[test]
fn sse_rich_card_table_fixture() {
    let ev = SseEvent::RichCard(RichCardData::Table(TableCard {
        title: Some("Users".to_string()),
        columns: vec!["id".to_string(), "name".to_string()],
        rows: vec![
            vec![serde_json::json!(1), serde_json::json!("Alice")],
            vec![serde_json::json!(2), serde_json::json!("Bob")],
        ],
    }));
    write_fixture("rich-card-table", &ev);
}

#[test]
fn sse_rich_card_metric_fixture() {
    let ev = SseEvent::RichCard(RichCardData::Metric(MetricCard {
        title: Some("Latency".to_string()),
        value: Some("42 ms".to_string()),
        label: Some("p50".to_string()),
        trend: Some(MetricTrend::Down),
    }));
    write_fixture("rich-card-metric", &ev);
}

#[test]
fn sse_rich_card_other_fallback_fixture() {
    let ev = SseEvent::RichCard(RichCardData::Other {
        card_type: "experimental_chart".to_string(),
        data: serde_json::json!({"foo": 1, "bar": [2, 3]}),
    });
    write_fixture("rich-card-other", &ev);
}

#[test]
fn sse_tool_approval_needed_fixture() {
    let ev = SseEvent::ToolApprovalNeeded {
        approval_id: ApprovalId::from(Uuid::nil()),
        tool_name: "workspace_write".to_string(),
        tool_input: serde_json::json!({"path": "/x.txt", "content": "hello"}),
        timeout_ms: 300_000_u64,
    };
    write_fixture("tool-approval-needed", &ev);
}

#[test]
fn sse_tool_approval_resolved_fixture() {
    let ev = SseEvent::ToolApprovalResolved {
        approval_id: ApprovalId::from(Uuid::nil()),
        action: ApprovalAction::Approved,
        modified_input: Some(serde_json::json!({"path": "/y.txt"})),
    };
    write_fixture("tool-approval-resolved", &ev);
}

#[test]
fn sse_finish_fixture() {
    let ev = SseEvent::Finish {
        agent_name: "Opex".to_string(),
    };
    write_fixture("finish", &ev);
}

#[test]
fn sse_error_fixture() {
    let ev = SseEvent::Error {
        error_text: "Provider timeout".to_string(),
    };
    write_fixture("error", &ev);
}

#[test]
fn sse_reconnecting_fixture() {
    let ev = SseEvent::Reconnecting {
        attempt: 2,
        delay_ms: 1500_u64,
    };
    write_fixture("reconnecting", &ev);
}

#[test]
fn sse_sync_finished_fixture() {
    let ev = SseEvent::Sync {
        content: "Final assistant response.".to_string(),
        tool_calls: vec![serde_json::json!({
            "toolCallId": "tc-1",
            "toolName": "code_exec",
            "output": "ok"
        })],
        status: SyncStatus::Finished,
        error: None,
    };
    write_fixture("sync-finished", &ev);
}

#[test]
fn sse_sync_interrupted_fixture() {
    let ev = SseEvent::Sync {
        content: "Partial response before interruption.".to_string(),
        tool_calls: vec![],
        status: SyncStatus::Interrupted,
        error: Some("stream lost: core restarted".to_string()),
    };
    write_fixture("sync-interrupted", &ev);
}

#[test]
fn sse_usage_fixture() {
    let ev = SseEvent::Usage(UsagePayload {
        input_tokens: 100,
        output_tokens: 50,
        agent_name: "Opex".to_string(),
        cache_read_tokens: Some(20),
        cache_creation_tokens: Some(5),
        reasoning_tokens: Some(3),
    });
    write_fixture("usage", &ev);
}

#[test]
fn sse_sync_error_fixture() {
    let ev = SseEvent::Sync {
        content: "Error occurred mid-stream.".to_string(),
        tool_calls: vec![],
        status: SyncStatus::Error,
        error: Some("LLM provider returned 500".to_string()),
    };
    write_fixture("sync-error", &ev);
}

#[test]
fn sse_sync_running_fixture() {
    let ev = SseEvent::Sync {
        content: "Stream still running.".to_string(),
        tool_calls: vec![],
        status: SyncStatus::Running,
        error: None,
    };
    write_fixture("sync-running", &ev);
}

#[test]
fn sse_sync_begin_fixture() {
    let ev = SseEvent::SyncBegin {
        boundary_message_id: Some(Uuid::nil()),
        run_status: SyncStatus::Running,
        truncated: false,
    };
    write_fixture("sync-begin", &ev);
}

#[test]
fn sse_sync_begin_empty_fixture() {
    let ev = SseEvent::SyncBegin {
        boundary_message_id: None,
        run_status: SyncStatus::Finished,
        truncated: false,
    };
    write_fixture("sync-begin-empty", &ev);
}

#[test]
fn sse_sync_end_fixture() {
    let ev = SseEvent::SyncEnd { last_seq: Some(41) };
    write_fixture("sync-end", &ev);
}

#[test]
fn sse_sync_end_empty_fixture() {
    let ev = SseEvent::SyncEnd { last_seq: None };
    write_fixture("sync-end-empty", &ev);
}

#[test]
fn sse_rich_card_metric_trend_up_fixture() {
    let ev = SseEvent::RichCard(RichCardData::Metric(MetricCard {
        title: Some("Throughput".to_string()),
        value: Some("1.2k req/s".to_string()),
        label: Some("p99".to_string()),
        trend: Some(MetricTrend::Up),
    }));
    write_fixture("rich-card-metric-up", &ev);
}

#[test]
fn sse_rich_card_metric_trend_flat_fixture() {
    let ev = SseEvent::RichCard(RichCardData::Metric(MetricCard {
        title: Some("Memory".to_string()),
        value: Some("512 MB".to_string()),
        label: Some("RSS".to_string()),
        trend: Some(MetricTrend::Flat),
    }));
    write_fixture("rich-card-metric-flat", &ev);
}
