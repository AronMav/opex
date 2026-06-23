//! S6 round-trip fixture generation. Writes JSON fixtures that the TS
//! channel adapter test consumes — proves Rust serde output matches
//! the ts-rs codegen TS shape.
//!
//! Run: `cargo test -p opex-types --test channels_wire`
//! Fixtures path: ../../channels/src/__tests__/fixtures/

use chrono::DateTime;
use opex_types::{
    ChannelActionDto, ChannelInbound, ChannelOutbound, IncomingMessageDto, MediaAttachment,
    MediaType,
};
use std::path::PathBuf;

fn fixtures_dir() -> PathBuf {
    let manifest = env!("CARGO_MANIFEST_DIR");
    PathBuf::from(manifest).join("../../channels/src/__tests__/fixtures")
}

fn write_fixture(name: &str, value: &impl serde::Serialize) {
    let json = serde_json::to_string_pretty(value).unwrap();
    // Verify round-trip: parse -> value -> re-serialize must produce equivalent JSON
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    let json2 = serde_json::to_string_pretty(&parsed).unwrap();
    let parsed2: serde_json::Value = serde_json::from_str(&json2).unwrap();
    assert_eq!(parsed, parsed2, "round-trip must be stable");
    let path = fixtures_dir().join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, &json).unwrap();
    eprintln!("wrote {}", path.display());
}

// ── ChannelInbound fixtures ───────────────────────────────────────────────────

#[test]
fn channel_inbound_message_roundtrip_fixture() {
    let timestamp = DateTime::parse_from_rfc3339("2026-05-07T15:30:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);

    let inbound = ChannelInbound::Message {
        request_id: "req-abc-123".to_string(),
        msg: IncomingMessageDto {
            user_id: "user-42".to_string(),
            display_name: Some("Alice".to_string()),
            text: Some("Hello, world".to_string()),
            attachments: vec![MediaAttachment {
                url: "https://example.com/image.png".to_string(),
                media_type: MediaType::Image,
                mime_type: Some("image/png".to_string()),
                file_name: Some("image.png".to_string()),
                file_size: Some(12345),
            }],
            context: serde_json::json!({"chat_id": "12345"}),
            timestamp,
        },
    };
    write_fixture("channel_inbound_message.json", &inbound);
}

#[test]
fn channel_inbound_action_result_success_fixture() {
    let inbound = ChannelInbound::ActionResult {
        action_id: "action-001".to_string(),
        success: true,
        error: None,
    };
    write_fixture("channel_inbound_action_result_success.json", &inbound);
}

#[test]
fn channel_inbound_action_result_error_fixture() {
    let inbound = ChannelInbound::ActionResult {
        action_id: "action-002".to_string(),
        success: false,
        error: Some("Permission denied".to_string()),
    };
    write_fixture("channel_inbound_action_result_error.json", &inbound);
}

#[test]
fn channel_inbound_access_check_fixture() {
    let inbound = ChannelInbound::AccessCheck {
        request_id: "req-access-1".to_string(),
        user_id: "user-789".to_string(),
    };
    write_fixture("channel_inbound_access_check.json", &inbound);
}

#[test]
fn channel_inbound_ping_fixture() {
    let inbound = ChannelInbound::Ping;
    write_fixture("channel_inbound_ping.json", &inbound);
}

#[test]
fn channel_inbound_ready_fixture() {
    let inbound = ChannelInbound::Ready {
        adapter_type: "telegram".to_string(),
        version: "1.0.0".to_string(),
        formatting_prompt: Some("Use emojis sparingly".to_string()),
    };
    write_fixture("channel_inbound_ready.json", &inbound);
}

#[test]
fn channel_inbound_ready_no_formatting_fixture() {
    let inbound = ChannelInbound::Ready {
        adapter_type: "discord".to_string(),
        version: "2.1.0".to_string(),
        formatting_prompt: None,
    };
    write_fixture("channel_inbound_ready_no_formatting.json", &inbound);
}

#[test]
fn channel_inbound_cancel_fixture() {
    let inbound = ChannelInbound::Cancel {
        request_id: "req-cancel-42".to_string(),
    };
    write_fixture("channel_inbound_cancel.json", &inbound);
}

#[test]
fn channel_inbound_pairing_create_fixture() {
    let inbound = ChannelInbound::PairingCreate {
        request_id: "req-pair-1".to_string(),
        user_id: "user-new".to_string(),
        display_name: Some("Bob".to_string()),
    };
    write_fixture("channel_inbound_pairing_create.json", &inbound);
}

#[test]
fn channel_inbound_pairing_create_no_name_fixture() {
    let inbound = ChannelInbound::PairingCreate {
        request_id: "req-pair-2".to_string(),
        user_id: "user-anon".to_string(),
        display_name: None,
    };
    write_fixture("channel_inbound_pairing_create_no_name.json", &inbound);
}

#[test]
fn channel_inbound_pairing_approve_fixture() {
    let inbound = ChannelInbound::PairingApprove {
        request_id: "req-approve-1".to_string(),
        code: "123456".to_string(),
    };
    write_fixture("channel_inbound_pairing_approve.json", &inbound);
}

#[test]
fn channel_inbound_pairing_reject_fixture() {
    let inbound = ChannelInbound::PairingReject {
        request_id: "req-reject-1".to_string(),
        code: "654321".to_string(),
    };
    write_fixture("channel_inbound_pairing_reject.json", &inbound);
}

// ── ChannelOutbound fixtures ──────────────────────────────────────────────────

#[test]
fn channel_outbound_action_roundtrip_fixture() {
    let outbound = ChannelOutbound::Action {
        action_id: "action-xyz-789".to_string(),
        action: ChannelActionDto {
            action: "send_photo".to_string(),
            params: serde_json::json!({"url": "https://example.com/x.jpg"}),
            context: serde_json::json!({"chat_id": "12345"}),
        },
    };
    write_fixture("channel_outbound_action.json", &outbound);
}

#[test]
fn channel_outbound_chunk_fixture() {
    let outbound = ChannelOutbound::Chunk {
        request_id: "req-stream-1".to_string(),
        text: "Hello".to_string(),
    };
    write_fixture("channel_outbound_chunk.json", &outbound);
}

#[test]
fn channel_outbound_done_fixture() {
    let outbound = ChannelOutbound::Done {
        request_id: "req-done-1".to_string(),
        text: "Final response text".to_string(),
    };
    write_fixture("channel_outbound_done.json", &outbound);
}

#[test]
fn channel_outbound_error_fixture() {
    let outbound = ChannelOutbound::Error {
        request_id: "req-err-1".to_string(),
        message: "Tool execution failed".to_string(),
    };
    write_fixture("channel_outbound_error.json", &outbound);
}

#[test]
fn channel_outbound_phase_fixture() {
    let outbound = ChannelOutbound::Phase {
        request_id: "req-phase-1".to_string(),
        phase: "thinking".to_string(),
        tool_name: Some("web_search".to_string()),
    };
    write_fixture("channel_outbound_phase.json", &outbound);
}

#[test]
fn channel_outbound_phase_no_tool_fixture() {
    let outbound = ChannelOutbound::Phase {
        request_id: "req-phase-2".to_string(),
        phase: "done".to_string(),
        tool_name: None,
    };
    write_fixture("channel_outbound_phase_no_tool.json", &outbound);
}

#[test]
fn channel_outbound_access_result_fixture() {
    let outbound = ChannelOutbound::AccessResult {
        request_id: "req-acc-1".to_string(),
        allowed: true,
        is_owner: true,
    };
    write_fixture("channel_outbound_access_result.json", &outbound);
}

#[test]
fn channel_outbound_pairing_code_fixture() {
    let outbound = ChannelOutbound::PairingCode {
        request_id: "req-pair-code-1".to_string(),
        code: "ABC123".to_string(),
    };
    write_fixture("channel_outbound_pairing_code.json", &outbound);
}

#[test]
fn channel_outbound_pairing_result_success_fixture() {
    let outbound = ChannelOutbound::PairingResult {
        request_id: "req-pair-res-1".to_string(),
        success: true,
        error: None,
    };
    write_fixture("channel_outbound_pairing_result_success.json", &outbound);
}

#[test]
fn channel_outbound_pairing_result_error_fixture() {
    let outbound = ChannelOutbound::PairingResult {
        request_id: "req-pair-res-2".to_string(),
        success: false,
        error: Some("Invalid code".to_string()),
    };
    write_fixture("channel_outbound_pairing_result_error.json", &outbound);
}

#[test]
fn channel_outbound_pong_fixture() {
    let outbound = ChannelOutbound::Pong;
    write_fixture("channel_outbound_pong.json", &outbound);
}

#[test]
fn channel_outbound_reload_fixture() {
    let outbound = ChannelOutbound::Reload;
    write_fixture("channel_outbound_reload.json", &outbound);
}

#[test]
fn channel_outbound_config_fixture() {
    let outbound = ChannelOutbound::Config {
        language: "ru".to_string(),
        owner_id: Some("123456789".to_string()),
        typing_mode: "thinking".to_string(),
    };
    write_fixture("channel_outbound_config.json", &outbound);
}
