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

/// Bootstrap a cron-owned goal: supersede any prior ACTIVE goal for the same
/// cron job (so a re-firing job never has two live drivers), then insert/refresh
/// the `origin='cron'` goal for `session_id`. One transaction.
pub async fn upsert_cron_goal(
    db: &PgPool,
    session_id: Uuid,
    cron_job_id: Uuid,
    goal_text: &str,
    max_turns: i32,
) -> Result<()> {
    let mut tx = db.begin().await?;
    // Supersede this job's prior in-flight goal(s) on other sessions.
    sqlx::query(
        "UPDATE session_goals SET status = 'cleared', updated_at = now()
         WHERE cron_job_id = $1 AND status = 'active' AND session_id <> $2",
    )
    .bind(cron_job_id)
    .bind(session_id)
    .execute(&mut *tx)
    .await?;
    // Insert/refresh the new run's goal.
    sqlx::query(
        "INSERT INTO session_goals (session_id, goal_text, status, turn_count, max_turns, origin, cron_job_id)
         VALUES ($1, $2, 'active', 0, $3, 'cron', $4)
         ON CONFLICT (session_id) DO UPDATE SET goal_text = EXCLUDED.goal_text,
           status = 'active', turn_count = 0, max_turns = EXCLUDED.max_turns,
           origin = 'cron', cron_job_id = EXCLUDED.cron_job_id,
           last_verdict = NULL, consecutive_judge_failures = 0, updated_at = now()",
    )
    .bind(session_id)
    .bind(goal_text)
    .bind(max_turns)
    .bind(cron_job_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Bootstrap an initiative-owned goal (Stage C: self-proposed, human-approved).
/// Single-statement insert/refresh with `origin='initiative'` — unlike
/// `upsert_cron_goal`, initiative goals are not scoped to a recurring job
/// identity, so there is no prior-run to supersede.
///
/// Transaction-only: the sole caller is `approve_proposal` (Phase 2A L1),
/// which flips the proposal status, creates the session, and seeds this goal
/// in ONE transaction so "approved without a goal" can never happen.
pub async fn upsert_initiative_goal_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    session_id: Uuid,
    goal_text: &str,
    max_turns: i32,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO session_goals (session_id, goal_text, status, turn_count, max_turns, origin)
         VALUES ($1, $2, 'active', 0, $3, 'initiative')
         ON CONFLICT (session_id) DO UPDATE SET goal_text = EXCLUDED.goal_text,
           status = 'active', turn_count = 0, max_turns = EXCLUDED.max_turns,
           origin = 'initiative',
           last_verdict = NULL, consecutive_judge_failures = 0, updated_at = now()",
    )
    .bind(session_id)
    .bind(goal_text)
    .bind(max_turns)
    .execute(&mut **tx)
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

/// Atomically cancel an active goal (guarded flip, mirrors [`crate::db::agent_plans::try_set_proposal_status`]).
/// Returns whether a row was flipped (i.e. it was `active`) — idempotent no-op otherwise.
/// Consumed by `cancel_goal` (Phase 2A), wired to `POST /api/agents/{name}/plan/goals/{session_id}/cancel`.
pub async fn try_cancel_goal(db: &PgPool, session_id: Uuid) -> Result<bool> {
    let row: Option<(Uuid,)> = sqlx::query_as(
        "UPDATE session_goals SET status='cancelled', updated_at=now()
         WHERE session_id=$1 AND status='active' RETURNING session_id",
    )
    .bind(session_id)
    .fetch_optional(db)
    .await?;
    Ok(row.is_some())
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
pub struct RedrivableGoal {
    pub session_id: Uuid,
    pub agent_id: String,
}

/// List autonomous (origin='cron') goals whose owning session crashed and is
/// eligible for re-drive: still `active`, within the retry budget
/// (`retry_count < max_retries`), past any backoff gate, and newer than
/// `staleness_secs`.
///
/// `run_status IN ('interrupted','done')` covers BOTH crash shapes for a cron
/// goal: `interrupted` = crashed mid-turn; `done` = crashed BETWEEN turns (the
/// last turn finalized, but the in-memory driver was lost). The `origin='cron'`
/// filter keeps this safe — a `/goal` on a live interactive session
/// (`origin='goal'`) is NEVER selected, so a human's `done` chat is never
/// auto-continued.
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
           AND s.run_status IN ('interrupted', 'done')
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

/// Active goals for an agent by origin (join through sessions.agent_id).
/// Used by the initiative context block to surface running self-initiated goals.
/// GoalRow has NO FromRow derive (manual tuple decode, mirroring `get()`), and
/// `subgoals` is JSONB → decode explicitly. Select session_id too (list needs it).
pub async fn list_active_by_agent_and_origin(
    db: &PgPool,
    agent_id: &str,
    origin: &str,
) -> Result<Vec<GoalRow>> {
    type Row = (Uuid, String, String, i32, i32, serde_json::Value, Option<String>, i32);
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT g.session_id, g.goal_text, g.status, g.turn_count, g.max_turns,
                g.subgoals, g.last_verdict, g.consecutive_judge_failures
         FROM session_goals g
         JOIN sessions s ON s.id = g.session_id
         WHERE s.agent_id = $1 AND g.origin = $2 AND g.status = 'active'
         ORDER BY g.created_at DESC",
    )
    .bind(agent_id)
    .bind(origin)
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(session_id, goal_text, status, turn_count, max_turns, subgoals, last_verdict, cjf)| GoalRow {
            session_id,
            goal_text,
            status,
            turn_count,
            max_turns,
            subgoals: serde_json::from_value(subgoals).unwrap_or_default(),
            last_verdict,
            consecutive_judge_failures: cjf,
        })
        .collect())
}

/// Set the resumer backoff gate: this goal will not be re-driven again until
/// `secs` from now. Applied after each re-drive attempt so a crash-looping goal
/// backs off instead of being retried on every boot.
pub async fn set_next_redrive_at(db: &PgPool, session_id: Uuid, secs: i64) -> Result<()> {
    sqlx::query(
        "UPDATE session_goals SET next_redrive_at = now() + ($2 * interval '1 second'), updated_at = now()
         WHERE session_id = $1",
    )
    .bind(session_id)
    .bind(secs)
    .execute(db)
    .await?;
    Ok(())
}

/// An interrupted interactive (`origin='goal'`) goal whose owner should be
/// nudged to `/goal resume`. These are NEVER auto-redriven — that would
/// silently continue a human's live chat — so the resumer notifies instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterruptedInteractiveGoal {
    pub session_id: Uuid,
    pub agent_id: String,
    pub user_id: String,
    pub goal_text: String,
    /// Originating channel (e.g. "telegram"); "ui"/web for non-channel sessions.
    pub channel: String,
    /// Persisted channel chat_id for channel-push; `None` for web sessions.
    pub chat_id: Option<i64>,
}

/// List interactive (`origin='goal'`) goals still `active` within the staleness
/// window. At a cold boot every in-memory goal driver is gone, so any still-active
/// interactive goal has a lost driver. Unlike cron goals (`list_redrivable`),
/// these must NOT be auto-continued; the caller notifies the owner to
/// `/goal resume` and pauses the goal (so it is not re-notified on the next boot).
pub async fn list_interrupted_interactive_goals(
    db: &PgPool,
    staleness_secs: i64,
) -> Result<Vec<InterruptedInteractiveGoal>> {
    let rows: Vec<(Uuid, String, String, String, String, Option<i64>)> = sqlx::query_as(
        "SELECT g.session_id, s.agent_id, s.user_id, g.goal_text, s.channel, s.chat_id
         FROM session_goals g
         JOIN sessions s ON s.id = g.session_id
         WHERE g.status = 'active'
           AND g.origin = 'goal'
           AND s.last_message_at > now() - ($1 * interval '1 second')
         ORDER BY s.last_message_at",
    )
    .bind(staleness_secs)
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(session_id, agent_id, user_id, goal_text, channel, chat_id)| InterruptedInteractiveGoal {
            session_id,
            agent_id,
            user_id,
            goal_text,
            channel,
            chat_id,
        })
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

        let want_interrupted = seed(&pool, "interrupted", "cron", 0, 60).await; // mid-turn crash
        let want_done = seed(&pool, "done", "cron", 0, 60).await; // crashed BETWEEN turns (driver lost)
        let _interactive = seed(&pool, "interrupted", "goal", 0, 60).await; // origin=goal → excluded
        let _exhausted = seed(&pool, "interrupted", "cron", 3, 60).await; // retry_count >= max → excluded
        let _stale = seed(&pool, "interrupted", "cron", 0, 100_000).await; // older than window → excluded

        let got = list_redrivable(&pool, 21_600, 3).await.unwrap(); // 6h window, max_retries = 3
        let mut ids: Vec<Uuid> = got.iter().map(|r| r.session_id).collect();
        ids.sort();
        let mut expected = vec![want_interrupted, want_done];
        expected.sort();
        assert_eq!(
            ids, expected,
            "both interrupted (mid-turn) and done (between-turns) cron goals are redrivable"
        );
        Ok(())
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn upsert_cron_goal_supersedes_prior_active_for_same_job(pool: PgPool) -> sqlx::Result<()> {
        let job = Uuid::new_v4();
        let s1 = seed_session(&pool).await;
        let s2 = seed_session(&pool).await;

        upsert_cron_goal(&pool, s1, job, "g1", 5).await.unwrap();
        assert_eq!(get(&pool, s1).await.unwrap().unwrap().status, "active", "first cron goal active");

        // Same job re-fires on a fresh session → the prior in-flight goal is superseded.
        upsert_cron_goal(&pool, s2, job, "g2", 5).await.unwrap();
        assert_eq!(get(&pool, s1).await.unwrap().unwrap().status, "cleared", "prior cron goal superseded");
        assert_eq!(get(&pool, s2).await.unwrap().unwrap().status, "active", "new cron goal active");

        // A different job must NOT supersede this job's active goal.
        let other_job = Uuid::new_v4();
        let s3 = seed_session(&pool).await;
        upsert_cron_goal(&pool, s3, other_job, "g3", 5).await.unwrap();
        assert_eq!(get(&pool, s2).await.unwrap().unwrap().status, "active", "different job does not supersede");
        Ok(())
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn set_next_redrive_at_gates_list_redrivable(pool: PgPool) -> sqlx::Result<()> {
        let sid = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO sessions (id, agent_id, user_id, channel, run_status, retry_count, last_message_at)
             VALUES ($1, 'Agent', 'u', 'CRON', 'interrupted', 0, now())",
        )
        .bind(sid)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO session_goals (session_id, goal_text, status, origin) VALUES ($1, 'g', 'active', 'cron')",
        )
        .bind(sid)
        .execute(&pool)
        .await
        .unwrap();

        // Eligible before any backoff gate.
        assert_eq!(list_redrivable(&pool, 21_600, 3).await.unwrap().len(), 1, "eligible before backoff");

        // After a future backoff gate, the resumer must skip it.
        set_next_redrive_at(&pool, sid, 3_600).await.unwrap();
        assert!(
            list_redrivable(&pool, 21_600, 3).await.unwrap().is_empty(),
            "future next_redrive_at gates the row out of the redrivable set"
        );
        Ok(())
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn list_interrupted_interactive_goals_selects_only_active_origin_goal(pool: PgPool) -> sqlx::Result<()> {
        async fn seed(pool: &PgPool, status: &str, origin: &str, age_secs: i64, goal: &str, user: &str) -> Uuid {
            let sid = Uuid::new_v4();
            sqlx::query(
                "INSERT INTO sessions (id, agent_id, user_id, channel, last_message_at)
                 VALUES ($1, 'Agent', $2, 'telegram', now() - ($3 * interval '1 second'))",
            )
            .bind(sid)
            .bind(user)
            .bind(age_secs)
            .execute(pool)
            .await
            .unwrap();
            sqlx::query(
                "INSERT INTO session_goals (session_id, goal_text, status, origin) VALUES ($1, $2, $3, $4)",
            )
            .bind(sid)
            .bind(goal)
            .bind(status)
            .bind(origin)
            .execute(pool)
            .await
            .unwrap();
            sid
        }

        let want = seed(&pool, "active", "goal", 60, "fix the bug", "alice").await; // eligible
        let _cron = seed(&pool, "active", "cron", 60, "cron task", "bob").await; // origin=cron → excluded
        let _paused = seed(&pool, "paused", "goal", 60, "paused goal", "carol").await; // already notified/paused → excluded
        let _done = seed(&pool, "done", "goal", 60, "done goal", "dave").await; // completed → excluded
        let _stale = seed(&pool, "active", "goal", 100_000, "old goal", "erin").await; // outside window → excluded

        // Persist a chat_id on the eligible session so the channel-push path is exercised.
        sqlx::query("UPDATE sessions SET chat_id = 99001 WHERE id = $1")
            .bind(want)
            .execute(&pool)
            .await
            .unwrap();

        let got = list_interrupted_interactive_goals(&pool, 21_600).await.unwrap(); // 6h window
        let ids: Vec<Uuid> = got.iter().map(|r| r.session_id).collect();
        assert_eq!(ids, vec![want], "only the active interactive goal within the window is listed");
        assert_eq!(got[0].goal_text, "fix the bug");
        assert_eq!(got[0].user_id, "alice");
        assert_eq!(got[0].agent_id, "Agent");
        assert_eq!(got[0].channel, "telegram", "channel surfaced for push routing");
        assert_eq!(got[0].chat_id, Some(99001), "persisted chat_id surfaced for channel-push");
        Ok(())
    }
}
