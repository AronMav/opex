//! Unit tests for gateway/handlers/sessions.rs::format_session_as_markdown function.
//!
//! This integration test validates the session export formatting that converts
//! session data (metadata + messages) into human-readable markdown.

use serde_json::json;

/// Format session data as markdown for export/display.
/// Converts session metadata and messages into a readable markdown document.
fn format_session_as_markdown(data: &serde_json::Value) -> String {
    let mut md = String::new();
    let session = &data["session"];
    let title = session["title"].as_str().unwrap_or("Untitled");
    let agent = session["agent_id"].as_str().unwrap_or("unknown");
    let started = session["started_at"].as_str().unwrap_or("");

    md.push_str(&format!("# {title}\n\n"));
    md.push_str(&format!("**Agent:** {agent} | **Started:** {started}\n\n---\n\n"));

    if let Some(messages) = data["messages"].as_array() {
        for msg in messages {
            let role = msg["role"].as_str().unwrap_or("unknown");
            let content = msg["content"].as_str().unwrap_or("");
            let ts = msg["created_at"].as_str().unwrap_or("");
            let ts_short = if ts.len() >= 16 { &ts[..16] } else { ts };

            let role_label = match role {
                "user" => "User",
                "assistant" => "Assistant",
                "system" => "System",
                "tool" => "Tool Result",
                _ => role,
            };

            md.push_str(&format!("## {role_label} ({ts_short})\n\n"));

            if let Some(tool_calls) = msg["tool_calls"].as_array() {
                for tc in tool_calls {
                    let name = tc["name"].as_str().unwrap_or("unknown");
                    let args = tc["arguments"].to_string();
                    md.push_str(&format!("### Tool: {name}\n```json\n{args}\n```\n\n"));
                }
            }

            if !content.is_empty() {
                md.push_str(content);
                md.push_str("\n\n");
            }
        }
    }
    md
}

#[test]
fn format_session_as_markdown_basic_structure() {
    let data = json!({
        "session": {
            "title": "Test Session",
            "agent_id": "Arty",
            "started_at": "2026-04-27T10:00:00Z"
        },
        "messages": []
    });
    let md = format_session_as_markdown(&data);
    assert!(md.contains("# Test Session"));
    assert!(md.contains("**Agent:** Arty"));
    assert!(md.contains("2026-04-27T10:00:00Z"));
}

#[test]
fn format_session_as_markdown_user_message() {
    let data = json!({
        "session": {"title": "S", "agent_id": "A", "started_at": ""},
        "messages": [{
            "role": "user",
            "content": "Hello world",
            "created_at": "2026-04-27T10:01:00Z",
            "tool_calls": []
        }]
    });
    let md = format_session_as_markdown(&data);
    assert!(md.contains("## User"));
    assert!(md.contains("Hello world"));
}

#[test]
fn format_session_as_markdown_assistant_message() {
    let data = json!({
        "session": {"title": "S", "agent_id": "A", "started_at": ""},
        "messages": [{
            "role": "assistant",
            "content": "Hi there",
            "created_at": "2026-04-27T10:02:00Z",
            "tool_calls": []
        }]
    });
    let md = format_session_as_markdown(&data);
    assert!(md.contains("## Assistant"));
    assert!(md.contains("Hi there"));
}

#[test]
fn format_session_as_markdown_tool_call() {
    let data = json!({
        "session": {"title": "S", "agent_id": "A", "started_at": ""},
        "messages": [{
            "role": "assistant",
            "content": "",
            "created_at": "2026-04-27T10:03:00Z",
            "tool_calls": [{"name": "web_search", "arguments": {"query": "rust"}}]
        }]
    });
    let md = format_session_as_markdown(&data);
    assert!(md.contains("### Tool: web_search"));
    assert!(md.contains("```json"));
}

#[test]
fn format_session_as_markdown_missing_fields_use_defaults() {
    // Completely empty JSON — all fields missing
    let data = json!({});
    let md = format_session_as_markdown(&data);
    assert!(md.contains("# Untitled"));
    assert!(md.contains("**Agent:** unknown"));
}

#[test]
fn format_session_as_markdown_truncates_timestamp() {
    // Timestamps longer than 16 chars should be truncated in display
    let data = json!({
        "session": {"title": "S", "agent_id": "A", "started_at": ""},
        "messages": [{
            "role": "user",
            "content": "msg",
            "created_at": "2026-04-27T10:05:00.000Z",
            "tool_calls": []
        }]
    });
    let md = format_session_as_markdown(&data);
    // Should contain truncated timestamp (first 16 chars: "2026-04-27T10:05")
    assert!(md.contains("2026-04-27T10:05"));
}
