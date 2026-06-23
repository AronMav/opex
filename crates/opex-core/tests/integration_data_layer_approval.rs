//! Phase 63 DATA-04: structured-error regression for `resolve_approval_strict`.
//!
//! The core 100-task race is exercised in `integration_approval_race.rs`
//! (both the legacy `Result<bool>` wrapper test AND the new strict block).
//! This file covers the HTTP-shape contract: `AlreadyResolved` error must
//! carry the current status, and `NotFound` must carry the requested id,
//! so the HTTP handler in `gateway/handlers/agents/crud.rs` can surface a
//! `409 Conflict` / `404 Not Found` body with correct detail text.

mod support;

use std::time::Duration;

use opex_core::db::approvals::{create_approval, resolve_approval_strict, ApprovalError};
use opex_types::ids::ApprovalId;
use serde_json::json;
use support::TestHarness;
use tokio::time::timeout;
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn strict_first_resolve_succeeds_second_is_already_resolved() {
    timeout(Duration::from_secs(60), async {
        let harness = TestHarness::new().await.expect("PG");
        let pool = harness.pool();

        let approval_id: ApprovalId = create_approval(
            pool,
            "char-agent",
            None,
            "tool_x",
            &json!({}),
            &json!({}),
        )
        .await
        .expect("create");

        resolve_approval_strict(pool, approval_id, "approved", "user-a")
            .await
            .expect("first resolve must succeed");

        let second =
            resolve_approval_strict(pool, approval_id, "approved", "user-b").await;
        match second {
            Err(ApprovalError::AlreadyResolved { id, status }) => {
                assert_eq!(id, approval_id);
                assert_eq!(
                    status, "approved",
                    "current status must echo the first resolver's value"
                );
            }
            other => panic!("expected AlreadyResolved, got {other:?}"),
        }
    })
    .await
    .expect("timeout");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn strict_missing_row_returns_not_found() {
    timeout(Duration::from_secs(60), async {
        let harness = TestHarness::new().await.expect("PG");
        let pool = harness.pool();
        let random_id = ApprovalId::from(Uuid::new_v4());
        let res = resolve_approval_strict(pool, random_id, "approved", "x").await;
        match res {
            Err(ApprovalError::NotFound { id }) => assert_eq!(id, random_id),
            other => panic!("expected NotFound, got {other:?}"),
        }
    })
    .await
    .expect("timeout");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn strict_already_resolved_error_message_includes_status() {
    timeout(Duration::from_secs(60), async {
        let harness = TestHarness::new().await.expect("PG");
        let pool = harness.pool();

        let approval_id: ApprovalId = create_approval(
            pool,
            "char-agent",
            None,
            "tool_x",
            &json!({}),
            &json!({}),
        )
        .await
        .expect("create");
        resolve_approval_strict(pool, approval_id, "rejected", "user-a")
            .await
            .expect("first");

        let err = resolve_approval_strict(pool, approval_id, "approved", "user-b")
            .await
            .unwrap_err();
        let s = err.to_string();
        assert!(
            s.contains("already resolved"),
            "error display must contain 'already resolved'; got {s:?}"
        );
        assert!(
            s.contains("status=rejected"),
            "error display must include the current status; got {s:?}"
        );
    })
    .await
    .expect("timeout");
}
