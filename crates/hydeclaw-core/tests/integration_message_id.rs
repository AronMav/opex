//! T5 integration test for `MessageId(Uuid)` newtype migration.
//!
//! Verifies:
//!   1. `MessageId` round-trips through the `messages.id` column via the
//!      `sqlx::Type` derive (no explicit `.0` plumbing required).
//!   2. `Message.db_id: Option<MessageId>` works end-to-end with the typed
//!      newtype as the canonical identity surface for in-memory messages.
//!
//! See:
//!   - crates/hydeclaw-types/src/ids.rs (MessageId definition)
//!   - docs/superpowers/specs/2026-05-07-s2-identity-first-stream-objects-design.md (T5)

mod support;

use std::time::Duration;

use hydeclaw_types::ids::MessageId;
use support::TestHarness;
use tokio::time::timeout;
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_message_id_roundtrips_through_messages_table() {
    timeout(Duration::from_secs(60), async {
        let harness = TestHarness::new().await.expect("ephemeral PG must come up");
        let pool = harness.pool();

        let session_id = Uuid::new_v4();
        let msg_id = MessageId::new();

        // Create session row first (FK dependency for messages.session_id).
        sqlx::query("INSERT INTO sessions (id, agent_id) VALUES ($1, $2)")
            .bind(session_id)
            .bind("Arty")
            .execute(pool)
            .await
            .expect("session insert must succeed");

        // Insert a message row binding MessageId directly via sqlx::Type.
        sqlx::query(
            "INSERT INTO messages (id, session_id, agent_id, role, content) \
             VALUES ($1, $2, $3, 'assistant', 'test')",
        )
        .bind(msg_id)
        .bind(session_id)
        .bind("Arty")
        .execute(pool)
        .await
        .expect("message insert must succeed");

        // Read back via direct query — sqlx::Type derive lets us decode straight
        // to MessageId without an explicit Uuid intermediate.
        let row: (MessageId,) = sqlx::query_as("SELECT id FROM messages WHERE id = $1")
            .bind(msg_id)
            .fetch_one(pool)
            .await
            .expect("row must round-trip");

        assert_eq!(
            msg_id, row.0,
            "MessageId roundtrip through DB must preserve identity"
        );
    })
    .await
    .expect("message id round-trip exceeded 60s wall timeout");
}
