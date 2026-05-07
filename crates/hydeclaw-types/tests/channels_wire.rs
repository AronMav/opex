//! S6 round-trip fixture generation. Writes JSON fixtures that the TS
//! channel adapter test consumes — proves Rust serde output matches
//! the ts-rs codegen TS shape.
//!
//! Run: `cargo test -p hydeclaw-types --test channels_wire`
//! Fixtures path: ../../channels/src/__tests__/fixtures/

use chrono::DateTime;
use hydeclaw_types::{
    ChannelActionDto, ChannelInbound, ChannelOutbound, IncomingMessageDto, MediaAttachment,
    MediaType,
};
use std::path::PathBuf;

fn fixtures_dir() -> PathBuf {
    let manifest = env!("CARGO_MANIFEST_DIR");
    PathBuf::from(manifest).join("../../channels/src/__tests__/fixtures")
}

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

    let json = serde_json::to_string_pretty(&inbound).unwrap();

    // Verify deterministic Serde output: round-trip back to Rust
    let parsed: ChannelInbound = serde_json::from_str(&json).unwrap();
    let json2 = serde_json::to_string_pretty(&parsed).unwrap();
    assert_eq!(json, json2, "Rust roundtrip must be stable");

    // Write fixture for TS test
    let path = fixtures_dir().join("channel_inbound_message.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, &json).unwrap();
    eprintln!("wrote {}", path.display());
}

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

    let json = serde_json::to_string_pretty(&outbound).unwrap();
    let parsed: ChannelOutbound = serde_json::from_str(&json).unwrap();
    let json2 = serde_json::to_string_pretty(&parsed).unwrap();
    assert_eq!(json, json2);

    let path = fixtures_dir().join("channel_outbound_action.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, &json).unwrap();
    eprintln!("wrote {}", path.display());
}
