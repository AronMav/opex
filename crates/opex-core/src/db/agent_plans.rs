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
    let row = sqlx::query_as::<_, (String, Option<String>, serde_json::Value, Option<DateTime<Utc>>, i32, Option<NaiveDate>, DateTime<Utc>)>(
        "SELECT agent_id, current_focus, proposals, last_proposal_at, proposals_today, proposal_day, updated_at
         FROM agent_plans WHERE agent_id = $1",
    )
    .bind(agent_id)
    .fetch_one(db)
    .await?;
    Ok(PlanRow {
        agent_id: row.0, current_focus: row.1, proposals: row.2,
        last_proposal_at: row.3, proposals_today: row.4, proposal_day: row.5, updated_at: row.6,
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
}
