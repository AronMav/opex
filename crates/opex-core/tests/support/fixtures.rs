//! Phase 63 Data Layer — shared fixture helpers for Wave 1 tests.
//!
//! These helpers are consumed by:
//!   - Plan 02: integration_data_layer_indexes.rs (DATA-01, DATA-05)
//!   - Plan 03: integration_stuck_sessions_window_fn.rs (DATA-02)
//!   - Plan 04: integration_batch_insert_chunking.rs (DATA-03)
//!   - Plan 05: integration_data_layer_approval.rs (DATA-04 strict variant)
//!
//! All helpers are idempotent on a freshly-migrated schema — they INSERT,
//! they do not TRUNCATE.

use anyhow::Result;
use sqlx::PgPool;
use uuid::Uuid;

/// Seed N sessions with varying (agent_id, user_id, channel) tuples, all with
/// recent `last_message_at`. Returns the generated session UUIDs in insertion order.
///
/// DATA-01: used by the migration-timing fixture (50k rows).
pub async fn seed_sessions(pool: &PgPool, n: usize) -> Result<Vec<Uuid>> {
    let mut ids = Vec::with_capacity(n);
    // Batch INSERT in chunks of 1000 rows to keep the parameter count
    // under the PG 65535 limit (4 binds per row => 16k rows max, we stay well below).
    let chunk_size = 1000usize;
    for chunk_start in (0..n).step_by(chunk_size) {
        let chunk_end = (chunk_start + chunk_size).min(n);
        let chunk_len = chunk_end - chunk_start;

        // 4 binds per row, NOW() literal for last_message_at
        // (use a stable fixed-clock expression to avoid clock drift inside the loop).
        let mut sql = String::from(
            "INSERT INTO sessions (id, agent_id, user_id, channel, last_message_at) VALUES ",
        );
        let mut ph: Vec<String> = Vec::with_capacity(chunk_len);
        for i in 0..chunk_len {
            let base = i * 4;
            ph.push(format!(
                "(${}, ${}, ${}, ${}, NOW())",
                base + 1,
                base + 2,
                base + 3,
                base + 4
            ));
        }
        sql.push_str(&ph.join(", "));

        let mut q = sqlx::query(&sql);
        for i in 0..chunk_len {
            let idx = chunk_start + i;
            let id = Uuid::new_v4();
            ids.push(id);
            let agent_id = format!("agent-{}", idx % 16);
            let user_id = format!("user-{}", idx % 128);
            let channel = match idx % 4 {
                0 => "telegram",
                1 => "web",
                2 => "discord",
                _ => "matrix",
            };
            q = q.bind(id).bind(agent_id).bind(user_id).bind(channel);
        }
        q.execute(pool).await?;
    }
    Ok(ids)
}

/// Seed the notifications table with `n_total` rows; exactly `n_unread`
/// of them have `read = FALSE`, the rest have `read = TRUE`. Rows are
/// inserted in reverse-creation order (oldest first) so the partial
/// index's `ORDER BY created_at DESC` has something meaningful to sort.
///
/// DATA-05: used by the EXPLAIN JSON partial-index assertion.
pub async fn seed_notifications(pool: &PgPool, n_total: usize, n_unread: usize) -> Result<()> {
    assert!(n_unread <= n_total, "n_unread must be <= n_total");
    let chunk_size = 1000usize;
    for chunk_start in (0..n_total).step_by(chunk_size) {
        let chunk_end = (chunk_start + chunk_size).min(n_total);
        let chunk_len = chunk_end - chunk_start;

        let mut sql =
            String::from("INSERT INTO notifications (type, title, body, read) VALUES ");
        let mut ph: Vec<String> = Vec::with_capacity(chunk_len);
        for i in 0..chunk_len {
            let base = i * 4;
            ph.push(format!(
                "(${}, ${}, ${}, ${})",
                base + 1,
                base + 2,
                base + 3,
                base + 4
            ));
        }
        sql.push_str(&ph.join(", "));

        let mut q = sqlx::query(&sql);
        for i in 0..chunk_len {
            let idx = chunk_start + i;
            let ntype = "system_info";
            let title = format!("notif-{idx}");
            let body = "seed body";
            let read: bool = idx >= n_unread; // first n_unread rows are unread
            q = q.bind(ntype).bind(title).bind(body).bind(read);
        }
        q.execute(pool).await?;
    }
    Ok(())
}

/// Seed N "stuck" sessions — run_status='running', last_message_at aged >120s,
/// retry_count=0, each with one user-role message as the final message.
/// DATA-02: used by the window-function rewrite characterization test to
/// exercise the `s.run_status = 'running' AND last.role = 'user'` branch.
pub async fn seed_stuck_sessions(pool: &PgPool, n: usize) -> Result<Vec<Uuid>> {
    let mut ids = Vec::with_capacity(n);
    for i in 0..n {
        let session_id = Uuid::new_v4();
        ids.push(session_id);
        sqlx::query(
            r#"
            INSERT INTO sessions (id, agent_id, user_id, channel, run_status,
                                  activity_at, last_message_at, retry_count)
            VALUES ($1, 'stuck-agent', 'user-stuck', 'web', 'running',
                    NOW() - INTERVAL '5 minutes',
                    NOW() - INTERVAL '5 minutes', 0)
            "#,
        )
        .bind(session_id)
        .execute(pool)
        .await?;
        // Insert a single user-role message as the final message.
        sqlx::query(
            r#"
            INSERT INTO messages (session_id, agent_id, role, content, created_at)
            VALUES ($1, 'stuck-agent', 'user', $2, NOW() - INTERVAL '5 minutes')
            "#,
        )
        .bind(session_id)
        .bind(format!("stuck message {i}"))
        .execute(pool)
        .await?;
    }
    Ok(ids)
}
