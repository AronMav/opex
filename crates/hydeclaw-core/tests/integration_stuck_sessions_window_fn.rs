//! Phase 63 DATA-02: characterization test for find_stuck_sessions rewrite.
//!
//! CONTEXT.md correction #2: the rewrite target is find_stuck_sessions
//! (the correlated-subquery N+1 at src/db/sessions.rs:360), NOT
//! find_stale_running_sessions_per_agent (which is already single-query;
//! the helper was renamed in the 2026-05-13 session-lifecycle root-fix).
//!
//! Identical-results contract: the new window-function SQL produces the
//! same (session_id, agent_id) set as the legacy correlated-subquery SQL
//! on a mixed-branch fixture.

mod support;

use std::collections::HashSet;
use std::time::Duration;

use hydeclaw_core::db::sessions::find_stuck_sessions;
use sqlx::PgPool;
use support::TestHarness;
use tokio::time::timeout;
use uuid::Uuid;

// Legacy SQL copied verbatim from src/db/sessions.rs:360 (pre-rewrite).
// Used by Test 2 to assert the new implementation is byte-identical on
// the characterization fixture. When the rewrite lands, this constant
// serves as the contract pin — if the test fails, the rewrite diverged.
const LEGACY_FIND_STUCK_SESSIONS_SQL: &str =
    "SELECT s.id, s.agent_id FROM sessions s \
     WHERE s.retry_count < $2 \
       AND COALESCE(s.activity_at, s.last_message_at) < NOW() - make_interval(secs => $1) \
       AND ( \
         (s.run_status = 'running' \
          AND (SELECT role FROM messages WHERE session_id = s.id ORDER BY created_at DESC LIMIT 1) = 'user') \
         OR \
         (s.run_status = 'done' \
          AND EXISTS ( \
            SELECT 1 FROM messages m \
            WHERE m.session_id = s.id AND m.role = 'assistant' \
              AND (m.content IS NULL OR m.content = '') \
              AND (m.tool_calls IS NULL OR m.tool_calls = '[]'::jsonb) \
              AND m.id = (SELECT id FROM messages WHERE session_id = s.id ORDER BY created_at DESC LIMIT 1) \
          )) \
       )";

/// Seed the characterization fixture: 4 buckets × 50 sessions each.
/// Returns the UUIDs of buckets (a) and (b) — the sessions that SHOULD
/// be returned by find_stuck_sessions(stale_secs=120, max_retries=3).
async fn seed_mixed_fixture(pool: &PgPool) -> (Vec<Uuid>, Vec<Uuid>) {
    let mut expected_stuck_running = Vec::with_capacity(50);
    let mut expected_stuck_done = Vec::with_capacity(50);

    // Bucket (a): stuck-running — 50 sessions, run_status='running',
    // activity_at 5 min ago, retry_count=0, last message role='user'.
    for i in 0..50 {
        let id = insert_session_with_last_msg(
            pool,
            "running",
            "5 minutes",
            0,
            "user",
            &format!("user msg {i}"),
            None,
        )
        .await;
        expected_stuck_running.push(id);
    }

    // Bucket (b): stuck-done-empty — 50 sessions, run_status='done',
    // activity_at 5 min ago, retry_count=0, last message is assistant
    // with empty content AND empty tool_calls.
    for _i in 0..50 {
        let id = insert_session_with_last_msg(
            pool,
            "done",
            "5 minutes",
            0,
            "assistant",
            "", // empty content
            Some(serde_json::json!([])), // empty tool_calls
        )
        .await;
        expected_stuck_done.push(id);
    }

    // Bucket (c): not stuck — activity_at just now, run_status='running'.
    for i in 0..50 {
        insert_session_with_last_msg(
            pool,
            "running",
            "1 second",
            0,
            "user",
            &format!("fresh msg {i}"),
            None,
        )
        .await;
    }

    // Bucket (d): over retry_count — run_status='running', user-last,
    // old activity — but retry_count=5 which exceeds max_retries=3.
    for i in 0..50 {
        insert_session_with_last_msg(
            pool,
            "running",
            "5 minutes",
            5,
            "user",
            &format!("over-retry msg {i}"),
            None,
        )
        .await;
    }

    (expected_stuck_running, expected_stuck_done)
}

/// Helper: insert one session and one trailing message. Returns the session UUID.
async fn insert_session_with_last_msg(
    pool: &PgPool,
    run_status: &str,
    activity_ago: &str,
    retry_count: i32,
    role: &str,
    content: &str,
    tool_calls: Option<serde_json::Value>,
) -> Uuid {
    let session_id = Uuid::new_v4();
    let activity_sql = format!("NOW() - INTERVAL '{activity_ago}'");
    let insert_session = format!(
        "INSERT INTO sessions (id, agent_id, user_id, channel, run_status, \
         activity_at, last_message_at, retry_count) \
         VALUES ($1, 'chr-agent', 'chr-user', 'web', $2, {a}, {a}, $3)",
        a = activity_sql
    );
    sqlx::query(&insert_session)
        .bind(session_id)
        .bind(run_status)
        .bind(retry_count)
        .execute(pool)
        .await
        .expect("insert session");

    // Insert the trailing message slightly after the session activity_at.
    let insert_msg_sql = format!(
        "INSERT INTO messages (session_id, agent_id, role, content, tool_calls, created_at) \
         VALUES ($1, 'chr-agent', $2, $3, $4, {a})",
        a = activity_sql
    );
    sqlx::query(&insert_msg_sql)
        .bind(session_id)
        .bind(role)
        .bind(content)
        .bind(tool_calls)
        .execute(pool)
        .await
        .expect("insert message");

    session_id
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn characterization_mixed_fixture_returns_100_stuck() {
    timeout(Duration::from_secs(120), async {
        let harness = TestHarness::new().await.expect("ephemeral PG");
        let pool = harness.pool();

        let (running, done) = seed_mixed_fixture(pool).await;
        assert_eq!(running.len(), 50);
        assert_eq!(done.len(), 50);

        let result = find_stuck_sessions(pool, 120, 3)
            .await
            .expect("find_stuck_sessions");

        let result_ids: HashSet<Uuid> = result.iter().map(|(id, _)| *id).collect();
        let expected: HashSet<Uuid> =
            running.iter().chain(done.iter()).copied().collect();

        assert_eq!(
            result_ids.len(),
            100,
            "expected exactly 100 stuck sessions; got {}",
            result_ids.len()
        );
        assert_eq!(
            result_ids, expected,
            "result set must equal the seeded stuck-running ∪ stuck-done buckets"
        );
    })
    .await
    .expect("timeout");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn identical_results_contract_legacy_vs_rewrite() {
    timeout(Duration::from_secs(120), async {
        let harness = TestHarness::new().await.expect("ephemeral PG");
        let pool = harness.pool();

        let (_running, _done) = seed_mixed_fixture(pool).await;

        // Run the LEGACY (pre-rewrite) SQL inline.
        let legacy: Vec<(Uuid, String)> = sqlx::query_as(LEGACY_FIND_STUCK_SESSIONS_SQL)
            .bind(120.0_f64)
            .bind(3_i32)
            .fetch_all(pool)
            .await
            .expect("legacy SQL");

        // Run the NEW (post-rewrite) implementation via the public function.
        let rewrite = find_stuck_sessions(pool, 120, 3)
            .await
            .expect("rewrite");

        let legacy_set: HashSet<(Uuid, String)> = legacy.into_iter().collect();
        let rewrite_set: HashSet<(Uuid, String)> = rewrite.into_iter().collect();

        assert_eq!(
            legacy_set,
            rewrite_set,
            "window-function rewrite MUST return the identical set of \
             (session_id, agent_id) pairs as the legacy correlated-subquery SQL. \
             Diff: legacy\\rewrite = {:?}; rewrite\\legacy = {:?}",
            legacy_set.difference(&rewrite_set).collect::<Vec<_>>(),
            rewrite_set.difference(&legacy_set).collect::<Vec<_>>()
        );
    })
    .await
    .expect("timeout");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn empty_fixture_returns_empty() {
    timeout(Duration::from_secs(60), async {
        let harness = TestHarness::new().await.expect("ephemeral PG");
        let pool = harness.pool();
        let result = find_stuck_sessions(pool, 120, 3).await.expect("empty");
        assert!(result.is_empty(), "no sessions → empty result");
    })
    .await
    .expect("timeout");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn session_with_assistant_reply_is_not_stuck() {
    timeout(Duration::from_secs(60), async {
        let harness = TestHarness::new().await.expect("ephemeral PG");
        let pool = harness.pool();
        // One session, old activity, last message is a non-empty assistant
        // → neither branch matches, must NOT appear in result.
        let session_id = insert_session_with_last_msg(
            pool,
            "running",
            "5 minutes",
            0,
            "assistant",
            "Hello, world!",
            None,
        )
        .await;

        let result = find_stuck_sessions(pool, 120, 3).await.expect("find");
        let ids: HashSet<Uuid> = result.iter().map(|(id, _)| *id).collect();
        assert!(
            !ids.contains(&session_id),
            "session whose latest message is a non-empty assistant MUST NOT be flagged as stuck"
        );
    })
    .await
    .expect("timeout");
}
