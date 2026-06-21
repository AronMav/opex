//! Standing-goal storage (table `session_goals`).

use anyhow::Result;
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalRow {
    pub session_id: Uuid,
    pub goal_text: String,
    pub status: String,
    pub turn_count: i32,
    pub max_turns: i32,
    pub subgoals: Vec<String>,
    pub last_verdict: Option<String>,
    pub consecutive_judge_failures: i32,
}

impl GoalRow {
    pub fn is_running(&self) -> bool {
        self.status == "active"
    }
    pub fn budget_left(&self) -> bool {
        self.turn_count < self.max_turns
    }
}

/// Column tuple returned by the `get` query (factored out to satisfy clippy::type_complexity).
type GoalRowTuple = (String, String, i32, i32, serde_json::Value, Option<String>, i32);

pub async fn get(db: &PgPool, session_id: Uuid) -> Result<Option<GoalRow>> {
    let row: Option<GoalRowTuple> = sqlx::query_as(
        "SELECT goal_text, status, turn_count, max_turns, subgoals, last_verdict, consecutive_judge_failures
         FROM session_goals WHERE session_id = $1",
    )
    .bind(session_id)
    .fetch_optional(db)
    .await?;
    Ok(row.map(|(goal_text, status, turn_count, max_turns, subgoals, last_verdict, cjf)| GoalRow {
        session_id,
        goal_text,
        status,
        turn_count,
        max_turns,
        subgoals: serde_json::from_value(subgoals).unwrap_or_default(),
        last_verdict,
        consecutive_judge_failures: cjf,
    }))
}

pub async fn upsert(db: &PgPool, session_id: Uuid, goal_text: &str, max_turns: i32) -> Result<()> {
    sqlx::query(
        "INSERT INTO session_goals (session_id, goal_text, status, turn_count, max_turns)
         VALUES ($1, $2, 'active', 0, $3)
         ON CONFLICT (session_id) DO UPDATE SET goal_text = EXCLUDED.goal_text,
           status = 'active', turn_count = 0, max_turns = EXCLUDED.max_turns,
           last_verdict = NULL, consecutive_judge_failures = 0, updated_at = now()",
    )
    .bind(session_id)
    .bind(goal_text)
    .bind(max_turns)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn set_status(db: &PgPool, session_id: Uuid, status: &str) -> Result<()> {
    sqlx::query("UPDATE session_goals SET status = $2, updated_at = now() WHERE session_id = $1")
        .bind(session_id)
        .bind(status)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn bump_turn(db: &PgPool, session_id: Uuid) -> Result<()> {
    sqlx::query("UPDATE session_goals SET turn_count = turn_count + 1, updated_at = now() WHERE session_id = $1")
        .bind(session_id)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn set_subgoals(db: &PgPool, session_id: Uuid, subgoals: &[String]) -> Result<()> {
    sqlx::query("UPDATE session_goals SET subgoals = $2, updated_at = now() WHERE session_id = $1")
        .bind(session_id)
        .bind(serde_json::to_value(subgoals)?)
        .execute(db)
        .await?;
    Ok(())
}

/// Record a judge verdict; reset the failure counter on a clean parse, increment on parse failure.
pub async fn record_verdict(db: &PgPool, session_id: Uuid, verdict: &str, judge_failed: bool) -> Result<()> {
    sqlx::query(
        "UPDATE session_goals SET last_verdict = $2,
           consecutive_judge_failures = CASE WHEN $3 THEN consecutive_judge_failures + 1 ELSE 0 END,
           updated_at = now() WHERE session_id = $1",
    )
    .bind(session_id)
    .bind(verdict)
    .bind(judge_failed)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn clear(db: &PgPool, session_id: Uuid) -> Result<()> {
    sqlx::query("DELETE FROM session_goals WHERE session_id = $1")
        .bind(session_id)
        .execute(db)
        .await?;
    Ok(())
}

/// A crashed autonomous (cron) goal eligible for re-drive on startup.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)] // consumed by resume_autonomous_goals() — wired in the next Phase-1 slice
pub struct RedrivableGoal {
    pub session_id: Uuid,
    pub agent_id: String,
}

/// List autonomous (origin='cron') goals whose owning session was interrupted by
/// a crash and is eligible for re-drive: still `active`, within the retry budget
/// (`retry_count < max_retries`), past any backoff gate, and newer than
/// `staleness_secs`.
///
/// CRITICAL scope boundary: `origin='cron' AND run_status='interrupted'` ensures
/// a `/goal` attached to a live interactive chat session (`origin='goal'`) is
/// NEVER selected — it must not be auto-continued into a human's conversation.
#[allow(dead_code)] // consumed by resume_autonomous_goals() — wired in the next Phase-1 slice
pub async fn list_redrivable(
    db: &PgPool,
    staleness_secs: i64,
    max_retries: i32,
) -> Result<Vec<RedrivableGoal>> {
    let rows: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT g.session_id, s.agent_id
         FROM session_goals g
         JOIN sessions s ON s.id = g.session_id
         WHERE g.status = 'active'
           AND g.origin = 'cron'
           AND s.run_status = 'interrupted'
           AND s.retry_count < $2
           AND (g.next_redrive_at IS NULL OR g.next_redrive_at <= now())
           AND s.last_message_at > now() - ($1 * interval '1 second')
         ORDER BY s.last_message_at",
    )
    .bind(staleness_secs)
    .bind(max_retries)
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(session_id, agent_id)| RedrivableGoal { session_id, agent_id })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(status: &str, turns: i32, max: i32) -> GoalRow {
        GoalRow {
            session_id: Uuid::nil(),
            goal_text: "g".into(),
            status: status.into(),
            turn_count: turns,
            max_turns: max,
            subgoals: vec![],
            last_verdict: None,
            consecutive_judge_failures: 0,
        }
    }

    #[test]
    fn is_running_and_budget() {
        assert!(row("active", 0, 20).is_running());
        assert!(!row("paused", 0, 20).is_running());
        assert!(row("active", 19, 20).budget_left());
        assert!(!row("active", 20, 20).budget_left());
    }

    async fn seed_session(pool: &PgPool) -> Uuid {
        let sid = Uuid::new_v4();
        sqlx::query("INSERT INTO sessions (id, agent_id, user_id, channel) VALUES ($1, 'Test', 'u', 'telegram')")
            .bind(sid)
            .execute(pool)
            .await
            .unwrap();
        sid
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn upsert_get_bump_status_roundtrip(pool: PgPool) -> sqlx::Result<()> {
        let sid = seed_session(&pool).await;
        upsert(&pool, sid, "refactor api", 5).await.unwrap();
        let g = get(&pool, sid).await.unwrap().unwrap();
        assert_eq!(g.goal_text, "refactor api");
        assert_eq!(g.max_turns, 5);
        assert!(g.is_running());
        bump_turn(&pool, sid).await.unwrap();
        assert_eq!(get(&pool, sid).await.unwrap().unwrap().turn_count, 1);
        set_subgoals(&pool, sid, &["a".into(), "b".into()]).await.unwrap();
        assert_eq!(get(&pool, sid).await.unwrap().unwrap().subgoals, vec!["a", "b"]);
        record_verdict(&pool, sid, "continue", true).await.unwrap();
        assert_eq!(get(&pool, sid).await.unwrap().unwrap().consecutive_judge_failures, 1);
        record_verdict(&pool, sid, "continue", false).await.unwrap();
        assert_eq!(get(&pool, sid).await.unwrap().unwrap().consecutive_judge_failures, 0);
        set_status(&pool, sid, "done").await.unwrap();
        assert_eq!(get(&pool, sid).await.unwrap().unwrap().status, "done");
        clear(&pool, sid).await.unwrap();
        assert!(get(&pool, sid).await.unwrap().is_none());
        Ok(())
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn upsert_resets_verdict_and_failures(pool: PgPool) -> sqlx::Result<()> {
        let sid = seed_session(&pool).await;
        upsert(&pool, sid, "g1", 5).await.unwrap();
        record_verdict(&pool, sid, "continue", true).await.unwrap();
        bump_turn(&pool, sid).await.unwrap();
        let before = get(&pool, sid).await.unwrap().unwrap();
        assert_eq!(before.consecutive_judge_failures, 1);
        assert_eq!(before.last_verdict.as_deref(), Some("continue"));
        // Re-setting the goal must start fresh counters.
        upsert(&pool, sid, "g2", 9).await.unwrap();
        let after = get(&pool, sid).await.unwrap().unwrap();
        assert_eq!(after.goal_text, "g2");
        assert_eq!(after.turn_count, 0);
        assert_eq!(after.max_turns, 9);
        assert_eq!(after.consecutive_judge_failures, 0);
        assert_eq!(after.last_verdict, None);
        Ok(())
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn list_redrivable_selects_only_crashed_cron_goals(pool: PgPool) -> sqlx::Result<()> {
        async fn seed(pool: &PgPool, run_status: &str, origin: &str, retry: i32, age_secs: i64) -> Uuid {
            let sid = Uuid::new_v4();
            sqlx::query(
                "INSERT INTO sessions (id, agent_id, user_id, channel, run_status, retry_count, last_message_at)
                 VALUES ($1, 'Agent', 'u', 'CRON', $2, $3, now() - ($4 * interval '1 second'))",
            )
            .bind(sid)
            .bind(run_status)
            .bind(retry)
            .bind(age_secs)
            .execute(pool)
            .await
            .unwrap();
            sqlx::query(
                "INSERT INTO session_goals (session_id, goal_text, status, origin)
                 VALUES ($1, 'g', 'active', $2)",
            )
            .bind(sid)
            .bind(origin)
            .execute(pool)
            .await
            .unwrap();
            sid
        }

        let want = seed(&pool, "interrupted", "cron", 0, 60).await; // eligible
        let _interactive = seed(&pool, "interrupted", "goal", 0, 60).await; // origin=goal → excluded
        let _done = seed(&pool, "done", "cron", 0, 60).await; // not crashed → excluded
        let _exhausted = seed(&pool, "interrupted", "cron", 3, 60).await; // retry_count >= max → excluded
        let _stale = seed(&pool, "interrupted", "cron", 0, 100_000).await; // older than window → excluded

        let got = list_redrivable(&pool, 21_600, 3).await.unwrap(); // 6h window, max_retries = 3
        let ids: Vec<Uuid> = got.iter().map(|r| r.session_id).collect();
        assert_eq!(ids, vec![want], "only the crashed cron goal within budget+window is redrivable");
        assert_eq!(got[0].agent_id, "Agent");
        Ok(())
    }
}
