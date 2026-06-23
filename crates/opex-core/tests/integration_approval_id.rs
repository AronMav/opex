//! T4 integration test for `ApprovalId(Uuid)` newtype migration.
//!
//! Verifies:
//!   1. `db::approvals::create_approval` returns `ApprovalId` (not `Uuid`)
//!   2. `ApprovalId` round-trips through the `pending_approvals.id` column
//!      via the `sqlx::Type` derive (no explicit `.0` plumbing required)
//!
//! See:
//!   - crates/opex-types/src/ids.rs (ApprovalId definition)
//!   - docs/superpowers/specs/2026-05-07-s2-identity-first-stream-objects-design.md (T4)

mod support;

use std::time::Duration;

use opex_core::db::approvals::create_approval;
use opex_types::ids::ApprovalId;
use serde_json::json;
use support::TestHarness;
use tokio::time::timeout;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_approval_id_roundtrips_through_pending_approvals_table() {
    timeout(Duration::from_secs(60), async {
        let harness = TestHarness::new().await.expect("ephemeral PG must come up");
        let pool = harness.pool();

        // Insert via the public create_approval (now returns ApprovalId after T4)
        let id: ApprovalId = create_approval(
            pool,
            "Arty",
            None,
            "test_tool",
            &json!({"arg": "value"}),
            &json!({"context": "test"}),
        )
        .await
        .expect("create_approval must succeed against migrated schema");

        // Read back via direct query — sqlx::Type derive lets us decode straight
        // to ApprovalId without an explicit Uuid intermediate.
        let row: (ApprovalId,) =
            sqlx::query_as("SELECT id FROM pending_approvals WHERE id = $1")
                .bind(id)
                .fetch_one(pool)
                .await
                .expect("row must round-trip");

        assert_eq!(
            id, row.0,
            "ApprovalId roundtrip through DB must preserve identity"
        );
    })
    .await
    .expect("approval id round-trip exceeded 60s wall timeout");
}
