//! Stage C initiative: per-agent plan object CRUD + atomic proposal ops.
use anyhow::Result;
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Proposal {
    pub id: Uuid,
    pub text: String,
    pub status: String, // pending | approved | dismissed
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub acted_at: Option<DateTime<Utc>>,
}

// B-wide daily plan: consumed by the heartbeat-advanced day_plan_tick driver
// (agent/initiative/day_plan.rs) and the initiative gateway handlers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DayIntent {
    #[serde(default)]
    pub session_id: Option<Uuid>,
    pub intent: String,
    pub status: String, // pending | active | done | cancelled
}

#[derive(Debug, Clone)]
pub struct PlanRow {
    // Full row mirror: agent_id/updated_at are decoded for completeness but not
    // read by current consumers (focus + proposals + counter fields are).
    #[allow(dead_code)]
    pub agent_id: String,
    pub current_focus: Option<String>,
    pub proposals: serde_json::Value,
    pub last_proposal_at: Option<DateTime<Utc>>,
    pub proposals_today: i32,
    pub proposal_day: Option<NaiveDate>,
    // day_plan_* fields: consumed by the day_plan_tick driver.
    pub day_plan: serde_json::Value,
    pub day_plan_current: i32,
    pub day_plan_date: Option<NaiveDate>,
    pub day_plan_status: Option<String>,
    // Decoded for completeness but not read by current consumers.
    #[allow(dead_code)]
    pub updated_at: DateTime<Utc>,
}

impl PlanRow {
    pub fn parsed_proposals(&self) -> Vec<Proposal> {
        serde_json::from_value(self.proposals.clone()).unwrap_or_default()
    }
}

pub async fn get_or_create(db: &PgPool, agent_id: &str) -> Result<PlanRow> {
    sqlx::query("INSERT INTO agent_plans (agent_id) VALUES ($1) ON CONFLICT (agent_id) DO NOTHING")
        .bind(agent_id)
        .execute(db)
        .await?;
    let row = sqlx::query_as::<_, (String, Option<String>, serde_json::Value, Option<DateTime<Utc>>, i32, Option<NaiveDate>, serde_json::Value, i32, Option<NaiveDate>, Option<String>, DateTime<Utc>)>(
        "SELECT agent_id, current_focus, proposals, last_proposal_at, proposals_today, proposal_day,
                day_plan, day_plan_current, day_plan_date, day_plan_status, updated_at
         FROM agent_plans WHERE agent_id = $1",
    )
    .bind(agent_id)
    .fetch_one(db)
    .await?;
    Ok(PlanRow {
        agent_id: row.0, current_focus: row.1, proposals: row.2,
        last_proposal_at: row.3, proposals_today: row.4, proposal_day: row.5,
        day_plan: row.6, day_plan_current: row.7, day_plan_date: row.8, day_plan_status: row.9,
        updated_at: row.10,
    })
}

pub async fn set_focus(db: &PgPool, agent_id: &str, focus: &str) -> Result<()> {
    sqlx::query(
        "UPDATE agent_plans SET current_focus = $2, updated_at = now() WHERE agent_id = $1",
    )
    .bind(agent_id)
    .bind(focus)
    .execute(db)
    .await?;
    Ok(())
}

/// Atomically append a proposal iff the daily cap allows. Resets the counter when
/// proposal_day differs from `today`. Returns true iff appended.
pub async fn try_add_proposal(
    db: &PgPool,
    agent_id: &str,
    today: NaiveDate,
    cap: i32,
    proposal: &Proposal,
) -> Result<bool> {
    let p = serde_json::to_value(proposal)?;
    // IS DISTINCT FROM is NULL-safe and guards a freshly-created row
    // (proposal_day NULL). New day OR under cap.
    let res = sqlx::query(
        "UPDATE agent_plans
           SET proposals = proposals || $3::jsonb,
               proposals_today = CASE WHEN proposal_day = $2 THEN proposals_today + 1 ELSE 1 END,
               proposal_day = $2,
               last_proposal_at = now(),
               updated_at = now()
         WHERE agent_id = $1
           AND (proposal_day IS DISTINCT FROM $2 OR proposals_today < $4)",
    )
    .bind(agent_id)
    .bind(today)
    .bind(p)
    .bind(cap)
    .execute(db)
    .await?;
    Ok(res.rows_affected() > 0)
}

/// Atomically flip a proposal pending → new_status. Returns the updated proposal
/// iff it was pending (idempotent no-op otherwise).
pub async fn try_set_proposal_status(
    db: &PgPool,
    agent_id: &str,
    id: Uuid,
    new_status: &str,
) -> Result<Option<Proposal>> {
    // jsonb path update guarded by current status = 'pending'. Uses a subquery to
    // find the array index of the matching pending element.
    let updated = sqlx::query_scalar::<_, serde_json::Value>(
        "WITH idx AS (
           SELECT ord - 1 AS i
           FROM agent_plans, jsonb_array_elements(proposals) WITH ORDINALITY e(val, ord)
           WHERE agent_id = $1 AND val->>'id' = $2::text AND val->>'status' = 'pending'
         )
         UPDATE agent_plans SET
           proposals = jsonb_set(
             jsonb_set(proposals, ARRAY[(SELECT i::text FROM idx), 'status'], to_jsonb($3::text)),
             ARRAY[(SELECT i::text FROM idx), 'acted_at'], to_jsonb(now())
           ),
           updated_at = now()
         WHERE agent_id = $1 AND EXISTS (SELECT 1 FROM idx)
         RETURNING proposals -> (SELECT i FROM idx)::int",
    )
    .bind(agent_id)
    .bind(id)
    .bind(new_status)
    .fetch_optional(db)
    .await?;
    Ok(updated.and_then(|v| serde_json::from_value(v).ok()))
}

/// Transaction variant of [`try_set_proposal_status`] — same guarded flip,
/// executed on the caller's transaction so it commits atomically with sibling
/// writes (Stage C `approve_proposal`: status flip + session + goal in one
/// tx — no "approved without goal" gap).
pub async fn try_set_proposal_status_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    agent_id: &str,
    id: Uuid,
    new_status: &str,
) -> Result<Option<Proposal>> {
    let updated = sqlx::query_scalar::<_, serde_json::Value>(
        "WITH idx AS (
           SELECT ord - 1 AS i
           FROM agent_plans, jsonb_array_elements(proposals) WITH ORDINALITY e(val, ord)
           WHERE agent_id = $1 AND val->>'id' = $2::text AND val->>'status' = 'pending'
         )
         UPDATE agent_plans SET
           proposals = jsonb_set(
             jsonb_set(proposals, ARRAY[(SELECT i::text FROM idx), 'status'], to_jsonb($3::text)),
             ARRAY[(SELECT i::text FROM idx), 'acted_at'], to_jsonb(now())
           ),
           updated_at = now()
         WHERE agent_id = $1 AND EXISTS (SELECT 1 FROM idx)
         RETURNING proposals -> (SELECT i FROM idx)::int",
    )
    .bind(agent_id)
    .bind(id)
    .bind(new_status)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(updated.and_then(|v| serde_json::from_value(v).ok()))
}

// status Option so the "no material" branch writes NULL atomically (review L1).
pub async fn set_day_plan(db: &PgPool, agent_id: &str, intents: &[DayIntent], date: NaiveDate, status: Option<&str>) -> Result<()> {
    sqlx::query(
        "UPDATE agent_plans SET day_plan = $2, day_plan_current = 0, day_plan_date = $3,
           day_plan_status = $4, updated_at = now() WHERE agent_id = $1",
    ).bind(agent_id).bind(serde_json::to_value(intents)?).bind(date).bind(status)
     .execute(db).await?;
    Ok(())
}

pub async fn set_day_plan_status(db: &PgPool, agent_id: &str, status: Option<&str>) -> Result<()> {
    sqlx::query("UPDATE agent_plans SET day_plan_status = $2, updated_at = now() WHERE agent_id = $1")
        .bind(agent_id).bind(status).execute(db).await?;
    Ok(())
}

/// Persist advanced pointer + updated intent statuses (day_plan JSONB).
pub async fn set_day_plan_pointer(db: &PgPool, agent_id: &str, current: i32, intents: &[DayIntent]) -> Result<()> {
    sqlx::query(
        "UPDATE agent_plans SET day_plan = $2, day_plan_current = $3, updated_at = now() WHERE agent_id = $1",
    ).bind(agent_id).bind(serde_json::to_value(intents)?).bind(current).execute(db).await?;
    Ok(())
}

/// CAS: flip pending→approved iff pending AND non-empty AND date matches the button's
/// date (review H2: a stale Telegram button from a prior day must not approve a newer,
/// differently-generated plan). Returns the pending intents iff flipped; None = no-op.
pub async fn try_start_day_plan_approval_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    agent_id: &str,
    date: NaiveDate,
) -> Result<Option<Vec<DayIntent>>> {
    let dp = sqlx::query_scalar::<_, serde_json::Value>(
        "UPDATE agent_plans SET day_plan_status = 'approved', updated_at = now()
         WHERE agent_id = $1 AND day_plan_status = 'pending' AND day_plan_date = $2
           AND jsonb_array_length(day_plan) > 0
         RETURNING day_plan",
    ).bind(agent_id).bind(date).fetch_optional(&mut **tx).await?;
    Ok(dp.and_then(|v| serde_json::from_value(v).ok()))
}

/// CAS dismiss: pending→dismissed iff pending AND date matches (review M4 — atomic,
/// not read-then-write). Returns true iff flipped.
pub async fn try_dismiss_day_plan(db: &PgPool, agent_id: &str, date: NaiveDate) -> Result<bool> {
    let res = sqlx::query(
        "UPDATE agent_plans SET day_plan_status = 'dismissed', updated_at = now()
         WHERE agent_id = $1 AND day_plan_status = 'pending' AND day_plan_date = $2",
    ).bind(agent_id).bind(date).execute(db).await?;
    Ok(res.rows_affected() > 0)
}

/// Write intents-with-session_ids back after materialization (same tx as approval).
pub async fn set_day_plan_intents_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    agent_id: &str,
    intents: &[DayIntent],
) -> Result<()> {
    sqlx::query("UPDATE agent_plans SET day_plan = $2, day_plan_current = 0, updated_at = now() WHERE agent_id = $1")
        .bind(agent_id).bind(serde_json::to_value(intents)?).execute(&mut **tx).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proposal_roundtrips_through_jsonb_value() {
        let p = Proposal {
            id: Uuid::nil(),
            text: "изучить X".into(),
            status: "pending".into(),
            created_at: DateTime::from_timestamp(0, 0).unwrap(),
            acted_at: None,
        };
        let arr = serde_json::json!([p]);
        let back: Vec<Proposal> = serde_json::from_value(arr).unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].status, "pending");
        assert_eq!(back[0].text, "изучить X");
    }

    /// Two concurrent `try_add_proposal` calls against a cap of 1 must only let
    /// ONE through — the `proposals_today < $4` guard has to be race-safe under
    /// Postgres row-level locking (UPDATE ... WHERE serializes the two statements).
    #[sqlx::test(migrations = "../../migrations")]
    async fn concurrent_try_add_proposal_respects_cap(pool: sqlx::PgPool) -> sqlx::Result<()> {
        get_or_create(&pool, "raceA").await.unwrap();
        let today = chrono::Utc::now().date_naive();
        let mk = |t: &str| Proposal {
            id: uuid::Uuid::new_v4(),
            text: t.into(),
            status: "pending".into(),
            created_at: chrono::Utc::now(),
            acted_at: None,
        };
        let (p1, p2) = (mk("g1"), mk("g2"));
        let (r1, r2) = tokio::join!(
            try_add_proposal(&pool, "raceA", today, 1, &p1),
            try_add_proposal(&pool, "raceA", today, 1, &p2)
        );
        assert_eq!([r1.unwrap(), r2.unwrap()].iter().filter(|x| **x).count(), 1);
        let plan = get_or_create(&pool, "raceA").await.unwrap();
        assert_eq!(plan.proposals_today, 1);
        assert_eq!(plan.parsed_proposals().len(), 1);
        Ok(())
    }

    /// Two concurrent `try_set_proposal_status` calls flipping the SAME pending
    /// proposal to `approved` must only let ONE win — the `status = 'pending'`
    /// guard in the CTE has to be race-safe (mirrors the `session_goals::try_cancel_goal`
    /// atomicity contract).
    #[sqlx::test(migrations = "../../migrations")]
    async fn concurrent_approve_flip_wins_once(pool: sqlx::PgPool) -> sqlx::Result<()> {
        get_or_create(&pool, "raceB").await.unwrap();
        let today = chrono::Utc::now().date_naive();
        let id = uuid::Uuid::new_v4();
        try_add_proposal(
            &pool,
            "raceB",
            today,
            1,
            &Proposal {
                id,
                text: "g".into(),
                status: "pending".into(),
                created_at: chrono::Utc::now(),
                acted_at: None,
            },
        )
        .await
        .unwrap();
        let (a, b) = tokio::join!(
            try_set_proposal_status(&pool, "raceB", id, "approved"),
            try_set_proposal_status(&pool, "raceB", id, "approved")
        );
        assert_eq!([a.unwrap(), b.unwrap()].iter().filter(|x| x.is_some()).count(), 1);
        Ok(())
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn day_plan_set_get_roundtrip(pool: sqlx::PgPool) -> sqlx::Result<()> {
        get_or_create(&pool, "dpA").await.unwrap();
        let today = chrono::Utc::now().date_naive();
        let intents = vec![
            DayIntent { session_id: None, intent: "довести X".into(), status: "pending".into() },
            DayIntent { session_id: None, intent: "разобрать Y".into(), status: "pending".into() },
        ];
        set_day_plan(&pool, "dpA", &intents, today, Some("pending")).await.unwrap();
        let p = get_or_create(&pool, "dpA").await.unwrap();
        assert_eq!(p.day_plan_status.as_deref(), Some("pending"));
        assert_eq!(p.day_plan_date, Some(today));
        assert_eq!(p.day_plan_current, 0);
        let parsed: Vec<DayIntent> = serde_json::from_value(p.day_plan.clone()).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].intent, "довести X");
        set_day_plan_status(&pool, "dpA", Some("dismissed")).await.unwrap();
        assert_eq!(get_or_create(&pool, "dpA").await.unwrap().day_plan_status.as_deref(), Some("dismissed"));
        Ok(())
    }

    /// CAS boundary conditions for `try_start_day_plan_approval_tx` (review H2 guard):
    /// wrong date, non-pending status, and empty plan must all no-op (None), while the
    /// happy path flips pending → approved and returns the intents.
    #[sqlx::test(migrations = "../../migrations")]
    async fn try_start_day_plan_approval_tx_cas_boundaries(pool: sqlx::PgPool) -> sqlx::Result<()> {
        let today = chrono::Utc::now().date_naive();
        let yesterday = today.pred_opt().unwrap();
        let intents = vec![
            DayIntent { session_id: None, intent: "x".into(), status: "pending".into() },
            DayIntent { session_id: None, intent: "x".into(), status: "pending".into() },
        ];

        get_or_create(&pool, "capA").await.unwrap();
        set_day_plan(&pool, "capA", &intents, today, Some("pending")).await.unwrap();

        // Wrong date: stale button referencing yesterday must not approve today's plan.
        let mut tx = pool.begin().await.unwrap();
        let r = try_start_day_plan_approval_tx(&mut tx, "capA", yesterday).await.unwrap();
        assert!(r.is_none());
        tx.rollback().await.unwrap();

        // Non-pending: already-approved plan must not be re-flipped.
        set_day_plan_status(&pool, "capA", Some("approved")).await.unwrap();
        let mut tx = pool.begin().await.unwrap();
        let r = try_start_day_plan_approval_tx(&mut tx, "capA", today).await.unwrap();
        assert!(r.is_none());
        tx.rollback().await.unwrap();

        // Empty plan: jsonb_array_length(day_plan) > 0 guard must block approval.
        get_or_create(&pool, "capB").await.unwrap();
        set_day_plan(&pool, "capB", &[], today, Some("pending")).await.unwrap();
        let mut tx = pool.begin().await.unwrap();
        let r = try_start_day_plan_approval_tx(&mut tx, "capB", today).await.unwrap();
        assert!(r.is_none());
        tx.rollback().await.unwrap();

        // Happy path: pending + non-empty + matching date flips to approved.
        set_day_plan(&pool, "capA", &intents, today, Some("pending")).await.unwrap();
        let mut tx = pool.begin().await.unwrap();
        let r = try_start_day_plan_approval_tx(&mut tx, "capA", today).await.unwrap();
        assert_eq!(r.as_ref().map(|v| v.len()), Some(2));
        tx.commit().await.unwrap();
        assert_eq!(get_or_create(&pool, "capA").await.unwrap().day_plan_status.as_deref(), Some("approved"));
        Ok(())
    }

    /// CAS boundary conditions for `try_dismiss_day_plan` (review M4 — atomic flip,
    /// not read-then-write): wrong date is a no-op, correct date flips once, and a
    /// second call against the now-`dismissed` row is idempotently rejected.
    #[sqlx::test(migrations = "../../migrations")]
    async fn try_dismiss_day_plan_cas(pool: sqlx::PgPool) -> sqlx::Result<()> {
        let today = chrono::Utc::now().date_naive();
        let yesterday = today.pred_opt().unwrap();
        let intents = vec![DayIntent { session_id: None, intent: "x".into(), status: "pending".into() }];

        get_or_create(&pool, "disA").await.unwrap();
        set_day_plan(&pool, "disA", &intents, today, Some("pending")).await.unwrap();

        // Wrong date: no-op, status stays pending.
        let flipped = try_dismiss_day_plan(&pool, "disA", yesterday).await.unwrap();
        assert!(!flipped);
        assert_eq!(get_or_create(&pool, "disA").await.unwrap().day_plan_status.as_deref(), Some("pending"));

        // Correct date: flips pending -> dismissed.
        let flipped = try_dismiss_day_plan(&pool, "disA", today).await.unwrap();
        assert!(flipped);
        assert_eq!(get_or_create(&pool, "disA").await.unwrap().day_plan_status.as_deref(), Some("dismissed"));

        // Idempotent: no longer pending, second call is a no-op.
        let flipped = try_dismiss_day_plan(&pool, "disA", today).await.unwrap();
        assert!(!flipped);
        Ok(())
    }
}
