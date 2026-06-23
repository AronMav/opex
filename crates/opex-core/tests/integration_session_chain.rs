//! Integration tests for `get_session_chain` — the recursive CTE that returns
//! the full ancestor chain for a session, ordered root-first (highest depth
//! first).
//!
//! Gated to Linux x86_64 because testcontainers requires Docker (matches the
//! pattern used by `integration_session_run_status.rs`).

#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use opex_core::db::sessions::{create_chain_session, get_session_chain};
use sqlx::PgPool;
use uuid::Uuid;

// ── Helpers ────────────────────────────────────────────────────────────────

async fn insert_root_session(pool: &PgPool, agent_id: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO sessions (id, agent_id, user_id, channel) VALUES ($1, $2, 'u', 'ui')",
    )
    .bind(id)
    .bind(agent_id)
    .execute(pool)
    .await
    .expect("insert root session");
    id
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[sqlx::test(migrations = "../../migrations")]
async fn chain_of_three_is_ordered_root_first(pool: PgPool) {
    let agent = format!("chain-test-{}", Uuid::new_v4());
    let a = insert_root_session(&pool, &agent).await;
    let b = create_chain_session(&pool, a, &agent, "u", "ui", None)
        .await
        .unwrap();
    let c = create_chain_session(&pool, b, &agent, "u", "ui", None)
        .await
        .unwrap();

    let chain = get_session_chain(&pool, c).await.unwrap();
    assert_eq!(chain.len(), 3, "expected 3 entries in chain");
    assert_eq!(chain[0].id, a, "root (A) should be first");
    assert_eq!(chain[1].id, b, "middle (B) should be second");
    assert_eq!(chain[2].id, c, "current (C) should be last");
    assert_eq!(chain[0].depth, 2, "root A has depth=2");
    assert_eq!(chain[2].depth, 0, "current C has depth=0");
}

#[sqlx::test(migrations = "../../migrations")]
async fn single_session_no_parent_returns_self(pool: PgPool) {
    let agent = format!("chain-solo-{}", Uuid::new_v4());
    let a = insert_root_session(&pool, &agent).await;

    let chain = get_session_chain(&pool, a).await.unwrap();
    assert_eq!(chain.len(), 1, "single session should return exactly 1 entry");
    assert_eq!(chain[0].id, a, "the single entry must be the session itself");
    assert!(
        chain[0].parent_session_id.is_none(),
        "root session must have no parent"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn nonexistent_session_returns_empty(pool: PgPool) {
    let random_id = Uuid::new_v4();
    let chain = get_session_chain(&pool, random_id)
        .await
        .expect("query must not fail for unknown id");
    assert!(
        chain.is_empty(),
        "nonexistent session must yield an empty chain, got {} entries",
        chain.len()
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn depth_values_are_correct(pool: PgPool) {
    let agent = format!("chain-depth-{}", Uuid::new_v4());
    let a = insert_root_session(&pool, &agent).await;
    let b = create_chain_session(&pool, a, &agent, "u", "ui", None)
        .await
        .unwrap();
    let c = create_chain_session(&pool, b, &agent, "u", "ui", None)
        .await
        .unwrap();

    let chain = get_session_chain(&pool, c).await.unwrap();
    assert_eq!(chain.len(), 3, "expected 3 entries");

    // Find each entry by id to be position-order independent.
    let entry_a = chain.iter().find(|e| e.id == a).expect("A in chain");
    let entry_b = chain.iter().find(|e| e.id == b).expect("B in chain");
    let entry_c = chain.iter().find(|e| e.id == c).expect("C in chain");

    assert_eq!(entry_a.depth, 2, "root A must have depth=2");
    assert_eq!(entry_b.depth, 1, "middle B must have depth=1");
    assert_eq!(entry_c.depth, 0, "leaf C must have depth=0");
}

#[sqlx::test(migrations = "../../migrations")]
async fn parent_session_id_links_are_preserved(pool: PgPool) {
    let agent = format!("chain-links-{}", Uuid::new_v4());
    let a = insert_root_session(&pool, &agent).await;
    let b = create_chain_session(&pool, a, &agent, "u", "ui", None)
        .await
        .unwrap();
    let c = create_chain_session(&pool, b, &agent, "u", "ui", None)
        .await
        .unwrap();

    let chain = get_session_chain(&pool, c).await.unwrap();
    assert_eq!(chain.len(), 3, "expected 3 entries");

    let entry_a = chain.iter().find(|e| e.id == a).expect("A in chain");
    let entry_b = chain.iter().find(|e| e.id == b).expect("B in chain");
    let entry_c = chain.iter().find(|e| e.id == c).expect("C in chain");

    assert!(
        entry_a.parent_session_id.is_none(),
        "root A must have no parent"
    );
    assert_eq!(
        entry_b.parent_session_id,
        Some(a),
        "B.parent_session_id must point to A"
    );
    assert_eq!(
        entry_c.parent_session_id,
        Some(b),
        "C.parent_session_id must point to B"
    );
}
