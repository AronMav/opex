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

pub async fn get(db: &PgPool, session_id: Uuid) -> Result<Option<GoalRow>> {
    let row: Option<(String, String, i32, i32, serde_json::Value, Option<String>, i32)> = sqlx::query_as(
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
           status = 'active', turn_count = 0, max_turns = EXCLUDED.max_turns, updated_at = now()",
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
}
