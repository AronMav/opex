//! T6 integration test for `ToolCallId(String)` newtype migration.
//!
//! Verifies:
//!   1. `ToolCallId` round-trips through the `messages.tool_call_id` column
//!      via the `sqlx::Type` derive (no explicit `.0` plumbing required).
//!   2. The newtype accepts arbitrary provider-supplied formats unchanged
//!      (OpenAI `"call_..."`, Anthropic `"toolu_..."`, etc.) — wrapping a
//!      `String` is identity-preserving.
//!
//! See:
//!   - crates/hydeclaw-types/src/ids.rs (`ToolCallId` definition)
//!   - docs/superpowers/specs/2026-05-07-s2-identity-first-stream-objects-design.md (T6)

mod support;

use std::time::Duration;

use hydeclaw_types::ids::ToolCallId;
use support::TestHarness;
use tokio::time::timeout;
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_tool_call_id_roundtrips_through_messages_table() {
    timeout(Duration::from_secs(60), async {
        let harness = TestHarness::new().await.expect("ephemeral PG must come up");
        let pool = harness.pool();

        let session_id = Uuid::new_v4();
        let tool_call_id = ToolCallId::new("call_abc123");

        // Create session row first (FK dependency for messages.session_id).
        // sessions.user_id and sessions.channel are NOT NULL — bind synthetic values.
        sqlx::query(
            "INSERT INTO sessions (id, agent_id, user_id, channel) \
             VALUES ($1, $2, $3, $4)",
        )
        .bind(session_id)
        .bind("Arty")
        .bind("test-user")
        .bind("web")
        .execute(pool)
        .await
        .expect("session insert must succeed");

        // Insert a tool message via direct SQL using ToolCallId via sqlx::Type.
        sqlx::query(
            "INSERT INTO messages (id, session_id, agent_id, role, content, tool_call_id) \
             VALUES ($1, $2, $3, 'tool', 'test result', $4)",
        )
        .bind(Uuid::new_v4())
        .bind(session_id)
        .bind("Arty")
        .bind(&tool_call_id)
        .execute(pool)
        .await
        .expect("message insert must succeed");

        // Read back via direct query — sqlx::Type derive should let us decode
        // straight to ToolCallId.
        let row: (Option<ToolCallId>,) = sqlx::query_as(
            "SELECT tool_call_id FROM messages \
             WHERE session_id = $1 AND role = 'tool' LIMIT 1",
        )
        .bind(session_id)
        .fetch_one(pool)
        .await
        .expect("row must round-trip");

        assert_eq!(
            Some(tool_call_id),
            row.0,
            "ToolCallId roundtrip through DB must preserve identity"
        );
    })
    .await
    .expect("tool_call_id round-trip exceeded 60s wall timeout");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_tool_call_id_accepts_arbitrary_provider_format() {
    timeout(Duration::from_secs(60), async {
        let harness = TestHarness::new().await.expect("ephemeral PG must come up");
        let pool = harness.pool();

        let session_id = Uuid::new_v4();
        // sessions.user_id and sessions.channel are NOT NULL — bind synthetic values.
        sqlx::query(
            "INSERT INTO sessions (id, agent_id, user_id, channel) \
             VALUES ($1, $2, $3, $4)",
        )
        .bind(session_id)
        .bind("Arty")
        .bind("test-user")
        .bind("web")
        .execute(pool)
        .await
        .expect("session insert must succeed");

        // Newtype wraps String, so any string the provider hands us must
        // round-trip unchanged. No validation, no parsing.
        for raw in [
            "call_abc123",
            "toolu_01ABC",
            "tool_xyz_999",
            "anything_goes",
        ] {
            let id = ToolCallId::new(raw);
            sqlx::query(
                "INSERT INTO messages (id, session_id, agent_id, role, content, tool_call_id) \
                 VALUES ($1, $2, $3, 'tool', 'test', $4)",
            )
            .bind(Uuid::new_v4())
            .bind(session_id)
            .bind("Arty")
            .bind(&id)
            .execute(pool)
            .await
            .expect("message insert with arbitrary provider id must succeed");

            // Confirm identity preserved at the wrapper level.
            assert_eq!(id.as_str(), raw);
        }
    })
    .await
    .expect("tool_call_id provider-format test exceeded 60s wall timeout");
}
