//! Issue #7 regression: when an LLM call is aborted mid-stream and a
//! `partial_text` row is persisted, the subsequent assistant-error row must
//! chain to the partial (not to the user message), so the two rows form a
//! linear thread under m012 message-branching (user → partial → error)
//! instead of becoming siblings under the same user parent.
//!
//! This test exercises the DB-level contract directly:
//!   1. Insert a user message → `user_id`
//!   2. Insert an assistant partial with `parent_message_id = user_id` →
//!      `partial_id` (simulating `persist_partial_if_any`)
//!   3. Insert the subsequent assistant error with
//!      `parent_message_id = partial_id` (the Issue #7 fix)
//!   4. Assert the parent chain: user → partial → error (linear, no
//!      sibling under user).
//!
//! Gated to linux x86_64 (same as `migration_026.rs`) because the test
//! harness spins up a live `pgvector/pgvector:pg17` container.

#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

mod support;

use std::time::Duration;

use opex_core::db::sessions::{insert_assistant_partial, save_message_ex};
use support::TestHarness;
use tokio::time::timeout;
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn aborted_llm_call_produces_linear_chain_not_siblings() {
    timeout(Duration::from_secs(60), async {
        let harness = TestHarness::new().await.expect("PG");
        let pool = harness.pool();

        // Minimal session row. agent_id/user_id/channel are NOT NULL
        // (see migrations/001_init.sql); every other column has defaults.
        let session_id: Uuid = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO sessions (id, agent_id, user_id, channel) \
             VALUES ($1, $2, $3, $4)",
        )
        .bind(session_id)
        .bind("test-agent")
        .bind("test-user")
        .bind("test")
        .execute(pool)
        .await
        .expect("insert session");

        // 1. User message (simulating engine's first save_message_ex).
        let user_id =
            save_message_ex(pool, session_id, "user", "hello", None, None, None, None, None)
                .await
                .expect("save user");

        // 2. Partial row (simulating persist_partial_if_any returning the
        //    new id). `parent_message_id = user_id`.
        let partial_id = insert_assistant_partial(
            pool,
            session_id,
            Some("test-agent"),
            "partial reply that was cut short",
            Some("inactivity"),
            Some(user_id),
        )
        .await
        .expect("insert partial");

        // 3. Subsequent error message MUST hang off the partial, not the
        //    user — this is the Issue #7 fix. `parent_message_id = partial_id`.
        let err_id = save_message_ex(
            pool,
            session_id,
            "assistant",
            "Error: stream timed out.",
            None,
            None,
            Some("test-agent"),
            None,
            Some(partial_id),
        )
        .await
        .expect("save error");

        // 4. Verify the chain.
        let partial_parent: Option<Uuid> = sqlx::query_scalar(
            "SELECT parent_message_id FROM messages WHERE id = $1",
        )
        .bind(partial_id)
        .fetch_one(pool)
        .await
        .expect("fetch partial parent");
        assert_eq!(
            partial_parent,
            Some(user_id),
            "partial must be child of user message"
        );

        let err_parent: Option<Uuid> = sqlx::query_scalar(
            "SELECT parent_message_id FROM messages WHERE id = $1",
        )
        .bind(err_id)
        .fetch_one(pool)
        .await
        .expect("fetch error parent");
        assert_eq!(
            err_parent,
            Some(partial_id),
            "error row must chain to partial (not to user) — Issue #7"
        );

        // Extra guard: user must have exactly ONE child (the partial) —
        // no siblings at the user level. If the bug regressed, the error
        // would also be a child of user, producing 2 children.
        let children_of_user: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::bigint FROM messages WHERE parent_message_id = $1",
        )
        .bind(user_id)
        .fetch_one(pool)
        .await
        .expect("count user children");
        assert_eq!(
            children_of_user, 1,
            "user message must have exactly 1 direct child (the partial); found {children_of_user}"
        );

        // And partial must have exactly ONE child (the error).
        let children_of_partial: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::bigint FROM messages WHERE parent_message_id = $1",
        )
        .bind(partial_id)
        .fetch_one(pool)
        .await
        .expect("count partial children");
        assert_eq!(
            children_of_partial, 1,
            "partial must have exactly 1 child (the error); found {children_of_partial}"
        );

        // And the partial row carries `status = 'aborted'` + the abort
        // reason we passed in.
        let (status, abort_reason): (String, Option<String>) = sqlx::query_as(
            "SELECT status, abort_reason FROM messages WHERE id = $1",
        )
        .bind(partial_id)
        .fetch_one(pool)
        .await
        .expect("fetch partial status");
        assert_eq!(status, "aborted");
        assert_eq!(abort_reason.as_deref(), Some("inactivity"));
    })
    .await
    .expect("timeout");
}
