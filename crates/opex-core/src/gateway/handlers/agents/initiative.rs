//! Stage C initiative endpoints: view plan, approve/dismiss proposals.
use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use serde_json::json;
use uuid::Uuid;

use crate::gateway::state::AppState;
use super::validate_agent_name;

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/agents/{name}/plan", get(api_get_plan))
        .route("/api/agents/{name}/plan/proposals/{id}/approve", post(api_approve_proposal))
        .route("/api/agents/{name}/plan/proposals/{id}/dismiss", post(api_dismiss_proposal))
        .route("/api/agents/{name}/plan/goals/{session_id}/cancel", post(api_cancel_goal))
        .route("/api/agents/{name}/plan/day/{date}/approve", post(api_approve_day_plan))
        .route("/api/agents/{name}/plan/day/{date}/dismiss", post(api_dismiss_day_plan))
}

async fn api_get_plan(
    State(app): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    if validate_agent_name(&name).is_err() || app.agents.get_engine(&name).await.is_none() {
        return Err((StatusCode::NOT_FOUND, Json(json!({"error": "agent not found"}))));
    }
    let plan = crate::db::agent_plans::get_or_create(&app.infra.db, &name)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))))?;
    let active = crate::db::session_goals::list_active_by_agent_and_origin(&app.infra.db, &name, "initiative")
        .await
        .unwrap_or_default();
    Ok(Json(json!({
        "agent": name,
        "current_focus": plan.current_focus,
        "proposals": plan.parsed_proposals(),
        "active_goals": active.iter().map(|g| json!({"goal": g.goal_text, "turns": g.turn_count, "session_id": g.session_id})).collect::<Vec<_>>(),
    })))
}

async fn api_dismiss_proposal(
    State(app): State<AppState>,
    Path((name, id)): Path<(String, Uuid)>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    if validate_agent_name(&name).is_err() {
        return Err((StatusCode::BAD_REQUEST, Json(json!({"error": "bad name"}))));
    }
    let Some(engine) = app.agents.get_engine(&name).await else {
        return Err((StatusCode::NOT_FOUND, Json(json!({"error": "agent not found"}))));
    };
    match dismiss_proposal(&app.infra.db, &engine, id).await {
        Ok(changed) => Ok(Json(json!({"ok": true, "changed": changed}))),
        Err(ProposalError::BaseAgent) => {
            Err((StatusCode::FORBIDDEN, Json(json!({"error": "initiative is non-base only"}))))
        }
        Err(ProposalError::Db(e)) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e})))),
    }
}

/// Errors from [`approve_proposal`].
#[derive(Debug)]
pub(crate) enum ProposalError {
    /// Initiative (Stage C self-proposed goals) is non-base only (M3 gate).
    BaseAgent,
    /// Any DB failure surfaced from the atomic flip+session+goal transaction.
    Db(String),
}

/// Result of a successful [`approve_proposal`] call.
pub(crate) struct ApproveOutcome {
    pub spawned: bool,
    pub session_id: Option<Uuid>,
}

/// Atomically approve a pending Stage-C proposal: flip status pending→approved,
/// create the session, and seed the initiative goal — all in ONE transaction
/// (L1: no "approved without goal" gap possible). The goal driver is spawned
/// only after commit, with `GoalTarget` resolved from agent CONFIG owner_id
/// (H1) — never from the request. M3: the non-base gate lives inside this
/// function, first, so every caller gets it for free.
pub(crate) async fn approve_proposal(
    db: &sqlx::PgPool,
    engine: &std::sync::Arc<crate::agent::engine::AgentEngine>,
    proposal_id: Uuid,
) -> Result<ApproveOutcome, ProposalError> {
    if engine.cfg().agent.base {
        return Err(ProposalError::BaseAgent);
    }
    let agent_name = engine.cfg().agent.name.clone();
    const INITIATIVE_GOAL_MAX_TURNS: i32 = 20;
    let channel = crate::agent::channel_kind::channel::CRON; // reuse the system channel

    // L1: flip + session + goal in ONE transaction. No "approved without goal".
    let mut tx = db.begin().await.map_err(|e| ProposalError::Db(e.to_string()))?;
    let flipped = crate::db::agent_plans::try_set_proposal_status_tx(&mut tx, &agent_name, proposal_id, "approved")
        .await
        .map_err(|e| ProposalError::Db(e.to_string()))?;
    let Some(proposal) = flipped else {
        tx.rollback().await.ok();
        // Not pending (already acted on, or unknown id) → idempotent no-op, no spawn.
        return Ok(ApproveOutcome { spawned: false, session_id: None });
    };
    let session_id = crate::db::sessions::create_new_session_tx(&mut tx, &agent_name, "system", channel)
        .await
        .map_err(|e| ProposalError::Db(e.to_string()))?;
    crate::db::session_goals::upsert_initiative_goal_tx(&mut tx, session_id, &proposal.text, INITIATIVE_GOAL_MAX_TURNS)
        .await
        .map_err(|e| ProposalError::Db(e.to_string()))?;
    tx.commit().await.map_err(|e| ProposalError::Db(e.to_string()))?;

    // H1: GoalTarget resolved from CONFIG owner_id — never the request. Spawn
    // only after commit so the driver never observes a not-yet-committed goal.
    let owner = engine.cfg().agent.access.as_ref().and_then(|a| a.owner_id.clone());
    let target = crate::agent::initiative::delivery::resolve_owner_target(db, &agent_name, owner.as_deref()).await;
    if let Some(pool) = engine.cfg().goal_pool.clone() {
        let handle = crate::agent::goal::driver::spawn_goal_driver(engine.clone(), session_id, target);
        pool.insert(session_id, handle);
    }
    Ok(ApproveOutcome { spawned: true, session_id: Some(session_id) })
}

/// Dismiss a pending Stage-C proposal (guarded flip pending→dismissed).
/// M3: same non-base gate as [`approve_proposal`], checked first.
pub(crate) async fn dismiss_proposal(
    db: &sqlx::PgPool,
    engine: &std::sync::Arc<crate::agent::engine::AgentEngine>,
    proposal_id: Uuid,
) -> Result<bool, ProposalError> {
    if engine.cfg().agent.base {
        return Err(ProposalError::BaseAgent);
    }
    let agent_name = engine.cfg().agent.name.clone();
    let updated = crate::db::agent_plans::try_set_proposal_status(db, &agent_name, proposal_id, "dismissed")
        .await
        .map_err(|e| ProposalError::Db(e.to_string()))?;
    Ok(updated.is_some())
}

/// Cancel an active standing goal (guarded flip active→cancelled) and stop its
/// driver, if any. M3: same non-base gate as [`approve_proposal`].
pub(crate) async fn cancel_goal(
    db: &sqlx::PgPool,
    engine: &std::sync::Arc<crate::agent::engine::AgentEngine>,
    session_id: Uuid,
) -> Result<bool, ProposalError> {
    if engine.cfg().agent.base {
        return Err(ProposalError::BaseAgent);
    }
    let agent_name = engine.cfg().agent.name.clone();
    let cancelled = crate::db::session_goals::try_cancel_goal(db, session_id, &agent_name)
        .await
        .map_err(|e| ProposalError::Db(e.to_string()))?;
    if cancelled
        && let Some(pool) = engine.cfg().goal_pool.clone()
    {
        crate::agent::goal::pool::stop(&pool, session_id);
    }
    Ok(cancelled)
}

async fn api_cancel_goal(
    State(app): State<AppState>,
    Path((name, session_id)): Path<(String, Uuid)>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    if validate_agent_name(&name).is_err() {
        return Err((StatusCode::BAD_REQUEST, Json(json!({"error": "bad name"}))));
    }
    let Some(engine) = app.agents.get_engine(&name).await else {
        return Err((StatusCode::NOT_FOUND, Json(json!({"error": "agent not found"}))));
    };
    match cancel_goal(&app.infra.db, &engine, session_id).await {
        Ok(cancelled) => Ok(Json(json!({"ok": true, "cancelled": cancelled}))),
        Err(ProposalError::BaseAgent) => {
            Err((StatusCode::FORBIDDEN, Json(json!({"error": "initiative is non-base only"}))))
        }
        Err(ProposalError::Db(e)) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e})))),
    }
}

async fn api_approve_proposal(
    State(app): State<AppState>,
    Path((name, id)): Path<(String, Uuid)>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    if validate_agent_name(&name).is_err() {
        return Err((StatusCode::BAD_REQUEST, Json(json!({"error": "bad name"}))));
    }
    // get_engine returns Option<Arc<AgentEngine>> (gateway/clusters/agent_core.rs)
    // — the engine directly, not an AgentHandle.
    let Some(engine) = app.agents.get_engine(&name).await else {
        return Err((StatusCode::NOT_FOUND, Json(json!({"error": "agent not found"}))));
    };
    match approve_proposal(&app.infra.db, &engine, id).await {
        Ok(outcome) => Ok(Json(json!({
            "ok": true,
            "spawned": outcome.spawned,
            "session_id": outcome.session_id,
        }))),
        Err(ProposalError::BaseAgent) => {
            Err((StatusCode::FORBIDDEN, Json(json!({"error": "initiative is non-base only"}))))
        }
        Err(ProposalError::Db(e)) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e})))),
    }
}

/// Shared tx body: CAS pending→approved (iff non-empty AND date matches — review H2),
/// create N sessions+goals (marked day_plan_managed so crash-redrive skips them — review H1),
/// write session_ids back. Returns count materialized (0 = CAS no-op).
pub(crate) async fn materialize_day_plan_tx(
    db: &sqlx::PgPool,
    agent_name: &str,
    date: chrono::NaiveDate,
) -> Result<usize, ProposalError> {
    const INITIATIVE_GOAL_MAX_TURNS: i32 = 20;
    let channel = crate::agent::channel_kind::channel::CRON;
    let mut tx = db.begin().await.map_err(|e| ProposalError::Db(e.to_string()))?;
    let Some(pending) = crate::db::agent_plans::try_start_day_plan_approval_tx(&mut tx, agent_name, date)
        .await
        .map_err(|e| ProposalError::Db(e.to_string()))?
    else {
        tx.rollback().await.ok();
        return Ok(0); // not pending / empty / wrong date → idempotent no-op
    };
    let mut materialized = Vec::with_capacity(pending.len());
    for it in pending {
        let sid = crate::db::sessions::create_new_session_tx(&mut tx, agent_name, "system", channel)
            .await
            .map_err(|e| ProposalError::Db(e.to_string()))?;
        crate::db::session_goals::upsert_initiative_goal_tx(&mut tx, sid, &it.intent, INITIATIVE_GOAL_MAX_TURNS)
            .await
            .map_err(|e| ProposalError::Db(e.to_string()))?;
        crate::db::session_goals::set_day_plan_managed_tx(&mut tx, sid, true)
            .await
            .map_err(|e| ProposalError::Db(e.to_string()))?;
        materialized.push(crate::db::agent_plans::DayIntent {
            session_id: Some(sid),
            intent: it.intent,
            status: "active".into(),
        });
    }
    crate::db::agent_plans::set_day_plan_intents_tx(&mut tx, agent_name, &materialized)
        .await
        .map_err(|e| ProposalError::Db(e.to_string()))?;
    tx.commit().await.map_err(|e| ProposalError::Db(e.to_string()))?;
    Ok(materialized.len())
}

/// Approve the owner-pending day plan (M3: same non-base gate as [`approve_proposal`]).
/// Materializes via [`materialize_day_plan_tx`]; returns true iff any goals were created.
pub(crate) async fn approve_day_plan(
    db: &sqlx::PgPool,
    engine: &std::sync::Arc<crate::agent::engine::AgentEngine>,
    date: chrono::NaiveDate,
) -> Result<bool, ProposalError> {
    if engine.cfg().agent.base {
        return Err(ProposalError::BaseAgent);
    }
    let agent_name = engine.cfg().agent.name.clone();
    let n = materialize_day_plan_tx(db, &agent_name, date).await?;
    Ok(n > 0)
}

/// Dismiss the owner-pending day plan (M3: same non-base gate as [`approve_proposal`]).
pub(crate) async fn dismiss_day_plan(
    db: &sqlx::PgPool,
    engine: &std::sync::Arc<crate::agent::engine::AgentEngine>,
    date: chrono::NaiveDate,
) -> Result<(), ProposalError> {
    if engine.cfg().agent.base {
        return Err(ProposalError::BaseAgent);
    }
    let agent_name = engine.cfg().agent.name.clone();
    // Atomic CAS (review M4): flips only if still pending AND date matches.
    crate::db::agent_plans::try_dismiss_day_plan(db, &agent_name, date)
        .await
        .map_err(|e| ProposalError::Db(e.to_string()))?;
    Ok(())
}

async fn api_approve_day_plan(
    State(app): State<AppState>,
    Path((name, date)): Path<(String, chrono::NaiveDate)>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    if validate_agent_name(&name).is_err() {
        return Err((StatusCode::BAD_REQUEST, Json(json!({"error": "bad name"}))));
    }
    let Some(engine) = app.agents.get_engine(&name).await else {
        return Err((StatusCode::NOT_FOUND, Json(json!({"error": "agent not found"}))));
    };
    match approve_day_plan(&app.infra.db, &engine, date).await {
        Ok(materialized) => Ok(Json(json!({"ok": true, "materialized": materialized}))),
        Err(ProposalError::BaseAgent) => {
            Err((StatusCode::FORBIDDEN, Json(json!({"error": "initiative is non-base only"}))))
        }
        Err(ProposalError::Db(e)) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e})))),
    }
}

async fn api_dismiss_day_plan(
    State(app): State<AppState>,
    Path((name, date)): Path<(String, chrono::NaiveDate)>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    if validate_agent_name(&name).is_err() {
        return Err((StatusCode::BAD_REQUEST, Json(json!({"error": "bad name"}))));
    }
    let Some(engine) = app.agents.get_engine(&name).await else {
        return Err((StatusCode::NOT_FOUND, Json(json!({"error": "agent not found"}))));
    };
    match dismiss_day_plan(&app.infra.db, &engine, date).await {
        Ok(()) => Ok(Json(json!({"ok": true}))),
        Err(ProposalError::BaseAgent) => {
            Err((StatusCode::FORBIDDEN, Json(json!({"error": "initiative is non-base only"}))))
        }
        Err(ProposalError::Db(e)) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e})))),
    }
}

#[cfg(test)]
mod tests {
    /// Approve materializes N session_goals in one CAS-guarded transaction; a
    /// second (concurrent double-click) call is an idempotent no-op, and a
    /// stale-date button (review H2) never approves a different day's plan.
    #[sqlx::test(migrations = "../../migrations")]
    async fn approve_day_plan_cas_materializes_once(pool: sqlx::PgPool) -> sqlx::Result<()> {
        crate::db::agent_plans::get_or_create(&pool, "DP").await.unwrap();
        let today = chrono::Utc::now().date_naive();
        let intents = vec![
            crate::db::agent_plans::DayIntent { session_id: None, intent: "a".into(), status: "pending".into() },
            crate::db::agent_plans::DayIntent { session_id: None, intent: "b".into(), status: "pending".into() },
        ];
        crate::db::agent_plans::set_day_plan(&pool, "DP", &intents, today, Some("pending")).await.unwrap();
        // First approval materializes 2 goals + flips approved.
        let n = super::materialize_day_plan_tx(&pool, "DP", today).await.unwrap();
        assert_eq!(n, 2);
        let plan = crate::db::agent_plans::get_or_create(&pool, "DP").await.unwrap();
        assert_eq!(plan.day_plan_status.as_deref(), Some("approved"));
        let parsed: Vec<crate::db::agent_plans::DayIntent> = serde_json::from_value(plan.day_plan.clone()).unwrap();
        assert!(parsed.iter().all(|i| i.session_id.is_some() && i.status == "active"));
        // Second (concurrent double-click) → CAS no-op, 0 new.
        let n2 = super::materialize_day_plan_tx(&pool, "DP", today).await.unwrap();
        assert_eq!(n2, 0);
        // Stale-date button → CAS no-op (review H2).
        let yesterday = today.pred_opt().unwrap();
        crate::db::agent_plans::set_day_plan(&pool, "DP", &intents, today, Some("pending")).await.unwrap(); // reset to pending, date=today
        assert_eq!(
            super::materialize_day_plan_tx(&pool, "DP", yesterday).await.unwrap(),
            0,
            "wrong date → no-op"
        );
        Ok(())
    }
}
