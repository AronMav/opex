use axum::{
    Router,
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::{get, post, put},
};
use serde::Deserialize;
use serde_json::json;
use sqlx::Row;

use super::super::AppState;
use crate::gateway::clusters::{AgentCore, InfraServices};

include!("cron_dto_structs.rs");

fn cron_job_to_dto(j: &crate::scheduler::ScheduledJob) -> CronJobDto {
    CronJobDto {
        id: j.id.to_string(),
        name: j.name.clone(),
        agent: j.agent_id.clone(),
        cron: j.cron_expr.clone(),
        timezone: j.timezone.clone(),
        task: j.task_message.clone(),
        enabled: j.enabled,
        silent: j.silent,
        announce_to: j.announce_to.clone(),
        jitter_secs: j.jitter_secs,
        run_once: j.run_once,
        run_at: j.run_at.map(|t| t.to_rfc3339()),
        created_at: j.created_at.to_rfc3339(),
        last_run: j.last_run_at.map(|t| t.to_rfc3339()),
        next_run: if j.enabled && !j.run_once {
            crate::scheduler::compute_next_run(&j.cron_expr, &j.timezone)
        } else {
            None
        },
        tool_policy: j.tool_policy.clone(),
    }
}

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/cron", get(api_list_cron).post(api_create_cron))
        .route("/api/cron/{id}", put(api_update_cron).delete(api_delete_cron))
        .route("/api/cron/{id}/run", post(api_run_cron))
        .route("/api/cron/{id}/runs", get(api_cron_runs))
        .route("/api/cron/runs", get(api_cron_runs_all))
}

// ── Cron Jobs API ──

pub(crate) async fn api_list_cron(State(infra): State<InfraServices>) -> impl IntoResponse {
    let rows = sqlx::query_as::<_, crate::scheduler::ScheduledJob>(
        "SELECT id, agent_id, name, cron_expr, timezone, task_message, enabled, created_at, last_run_at, silent, announce_to, jitter_secs, run_once, run_at, tool_policy \
         FROM scheduled_jobs ORDER BY created_at DESC",
    )
    .fetch_all(&infra.db)
    .await;

    match rows {
        Ok(jobs) => {
            let list: Vec<CronJobDto> = jobs.iter().map(cron_job_to_dto).collect();
            Json(json!({ "jobs": list })).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct CreateCronRequest {
    name: String,
    agent: String,
    #[serde(default)]
    cron: String,
    timezone: Option<String>,
    task: String,
    announce_to: Option<serde_json::Value>,
    silent: Option<bool>,
    #[serde(default)]
    jitter_secs: i32,
    #[serde(default)]
    run_once: bool,
    run_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    tool_policy: Option<serde_json::Value>,
}

pub(crate) async fn api_create_cron(
    State(infra): State<InfraServices>,
    State(agents): State<AgentCore>,
    Json(req): Json<CreateCronRequest>,
) -> impl IntoResponse {
    if req.name.is_empty() || req.agent.is_empty() || req.task.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "name, agent, task are required"})),
        )
            .into_response();
    }
    if req.run_once && req.run_at.is_none() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "run_once requires run_at"})),
        )
            .into_response();
    }
    if !req.run_once && req.cron.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "recurring job requires cron expression"})),
        )
            .into_response();
    }

    // Validate tool names in policy (prevent path traversal / invalid names)
    if let Some(ref policy_json) = req.tool_policy
        && let Some(obj) = policy_json.as_object() {
            let valid_name = regex::Regex::new(r"^[a-zA-Z0-9_-]+$").unwrap();
            for key in &["allow", "deny"] {
                if let Some(arr) = obj.get(*key).and_then(|v| v.as_array()) {
                    for item in arr {
                        if let Some(name) = item.as_str()
                            && !valid_name.is_match(name) {
                                return (
                                    StatusCode::BAD_REQUEST,
                                    Json(serde_json::json!({"error": format!("invalid tool name: {}", name)})),
                                ).into_response();
                            }
                    }
                }
            }
        }

    let timezone = req.timezone.unwrap_or_else(|| "UTC".to_string());

    let silent = req.silent.unwrap_or(false);
    let result = sqlx::query_scalar::<_, uuid::Uuid>(
        "INSERT INTO scheduled_jobs (agent_id, name, cron_expr, timezone, task_message, announce_to, silent, jitter_secs, run_once, run_at, tool_policy) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) RETURNING id",
    )
    .bind(&req.agent)
    .bind(&req.name)
    .bind(&req.cron)
    .bind(&timezone)
    .bind(&req.task)
    .bind(&req.announce_to)
    .bind(silent)
    .bind(req.jitter_secs)
    .bind(req.run_once)
    .bind(req.run_at)
    .bind(&req.tool_policy)
    .fetch_one(&infra.db)
    .await;

    match result {
        Ok(id) => {
            // Schedule the job if agent engine exists
            if let Some(engine) = agents.get_engine(&req.agent).await
                && let Err(e) = agents
                    .scheduler
                    .add_dynamic_job(
                        id,
                        &req.cron,
                        &timezone,
                        req.task,
                        req.agent,
                        engine.clone(),
                        infra.db.clone(),
                        req.announce_to,
                        silent,
                        req.jitter_secs,
                        req.run_once,
                        req.run_at,
                        req.tool_policy,
                    )
                    .await
                {
                    tracing::warn!(error = %e, "failed to schedule new cron job");
                }
            Json(json!({"ok": true, "id": id})).into_response()
        }
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("unique") || msg.contains("duplicate") {
                return (StatusCode::CONFLICT, Json(json!({"error": "a job with this name already exists for this agent"}))).into_response();
            }
            (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": msg}))).into_response()
        }
    }
}

/// Helper: deserialize a field that can be absent, null, or a value.
/// absent → None (keep current), null → Some(None) (clear), value → Some(Some(v))
fn deserialize_nullable_field<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    Ok(Some(Option::deserialize(deserializer)?))
}

#[derive(Debug, Deserialize)]
pub(crate) struct UpdateCronRequest {
    name: Option<String>,
    cron: Option<String>,
    timezone: Option<String>,
    task: Option<String>,
    enabled: Option<bool>,
    /// None = not provided (keep current), Some(None) = explicit null (clear), Some(Some(v)) = new value
    #[serde(default, deserialize_with = "deserialize_nullable_field")]
    announce_to: Option<Option<serde_json::Value>>,
    silent: Option<bool>,
    jitter_secs: Option<i32>,
    run_once: Option<bool>,
    #[serde(default, deserialize_with = "deserialize_nullable_field")]
    run_at: Option<Option<chrono::DateTime<chrono::Utc>>>,
    /// None = not provided (keep current), Some(None) = explicit null (clear), Some(Some(v)) = new value
    #[serde(default, deserialize_with = "deserialize_nullable_field")]
    tool_policy: Option<Option<serde_json::Value>>,
}

pub(crate) async fn api_update_cron(
    State(infra): State<InfraServices>,
    State(agents): State<AgentCore>,
    axum::extract::Path(id): axum::extract::Path<uuid::Uuid>,
    Json(req): Json<UpdateCronRequest>,
) -> impl IntoResponse {
    // Fetch current job
    let current = sqlx::query_as::<_, crate::scheduler::ScheduledJob>(
        "SELECT id, agent_id, name, cron_expr, timezone, task_message, enabled, created_at, last_run_at, silent, announce_to, jitter_secs, run_once, run_at, tool_policy \
         FROM scheduled_jobs WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&infra.db)
    .await;

    let current = match current {
        Ok(Some(j)) => j,
        Ok(None) => {
            return (StatusCode::NOT_FOUND, Json(json!({"error": "job not found"}))).into_response()
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": e.to_string()})),
            )
                .into_response()
        }
    };

    let name = req.name.unwrap_or(current.name);
    let cron_expr = req.cron.unwrap_or(current.cron_expr);
    let timezone = req.timezone.unwrap_or(current.timezone);
    let task_message = req.task.unwrap_or(current.task_message);
    let enabled = req.enabled.unwrap_or(current.enabled);
    let silent = req.silent.unwrap_or(current.silent);
    let jitter_secs = req.jitter_secs.unwrap_or(current.jitter_secs);
    let run_once = req.run_once.unwrap_or(current.run_once);
    let run_at = match req.run_at {
        Some(None) => None,                     // explicit null → clear
        Some(Some(v)) => Some(v),               // new value
        None => current.run_at,                 // not provided → keep current
    };
    // Option<Option<Value>>: None = absent (keep), Some(None) = null (clear), Some(Some(v)) = new
    let announce_to = match req.announce_to {
        Some(None) => None,                     // explicit null → clear
        Some(Some(v)) => Some(v),               // new value
        None => current.announce_to,            // not provided → keep current
    };
    let tool_policy = match req.tool_policy {
        Some(None) => None,                     // explicit null → clear
        Some(Some(v)) => Some(v),               // new value
        None => current.tool_policy,            // not provided → keep current
    };

    // Validate tool names in policy (prevent path traversal / invalid names)
    if let Some(ref policy_json) = tool_policy
        && let Some(obj) = policy_json.as_object() {
            let valid_name = regex::Regex::new(r"^[a-zA-Z0-9_-]+$").unwrap();
            for key in &["allow", "deny"] {
                if let Some(arr) = obj.get(*key).and_then(|v| v.as_array()) {
                    for item in arr {
                        if let Some(name) = item.as_str()
                            && !valid_name.is_match(name) {
                                return (
                                    StatusCode::BAD_REQUEST,
                                    Json(serde_json::json!({"error": format!("invalid tool name: {}", name)})),
                                ).into_response();
                            }
                    }
                }
            }
        }

    let result = sqlx::query(
        "UPDATE scheduled_jobs SET name = $2, cron_expr = $3, timezone = $4, task_message = $5, enabled = $6, \
         silent = $7, announce_to = $8, jitter_secs = $9, run_once = $10, run_at = $11, tool_policy = $12 WHERE id = $1",
    )
    .bind(id)
    .bind(&name)
    .bind(&cron_expr)
    .bind(&timezone)
    .bind(&task_message)
    .bind(enabled)
    .bind(silent)
    .bind(&announce_to)
    .bind(jitter_secs)
    .bind(run_once)
    .bind(run_at)
    .bind(&tool_policy)
    .execute(&infra.db)
    .await;

    match result {
        Ok(_) => {
            // Remove old scheduler entry and re-add if enabled
            agents.scheduler.remove_dynamic_job(id).await.ok();
            if enabled
                && let Some(engine) = agents.get_engine(&current.agent_id).await {
                    agents
                        .scheduler
                        .add_dynamic_job(
                            id,
                            &cron_expr,
                            &timezone,
                            task_message,
                            current.agent_id,
                            engine.clone(),
                            infra.db.clone(),
                            announce_to,
                            silent,
                            jitter_secs,
                            run_once,
                            run_at,
                            tool_policy,
                        )
                        .await
                        .ok();
                }
            Json(json!({"ok": true})).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

pub(crate) async fn api_delete_cron(
    State(infra): State<InfraServices>,
    State(agents): State<AgentCore>,
    axum::extract::Path(id): axum::extract::Path<uuid::Uuid>,
) -> impl IntoResponse {
    // Remove from scheduler
    agents.scheduler.remove_dynamic_job(id).await.ok();

    let result = sqlx::query("DELETE FROM scheduled_jobs WHERE id = $1")
        .bind(id)
        .execute(&infra.db)
        .await;

    match result {
        Ok(r) if r.rows_affected() > 0 => Json(json!({"ok": true})).into_response(),
        Ok(_) => (StatusCode::NOT_FOUND, Json(json!({"error": "job not found"}))).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

pub(crate) async fn api_run_cron(
    State(infra): State<InfraServices>,
    State(agents): State<AgentCore>,
    axum::extract::Path(id): axum::extract::Path<uuid::Uuid>,
) -> impl IntoResponse {
    let job = sqlx::query_as::<_, crate::scheduler::ScheduledJob>(
        "SELECT id, agent_id, name, cron_expr, timezone, task_message, enabled, created_at, last_run_at, silent, announce_to, jitter_secs, run_once, run_at, tool_policy \
         FROM scheduled_jobs WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&infra.db)
    .await;

    let job = match job {
        Ok(Some(j)) => j,
        Ok(None) => {
            return (StatusCode::NOT_FOUND, Json(json!({"error": "job not found"}))).into_response()
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": e.to_string()})),
            )
                .into_response()
        }
    };

    let engine = match agents.get_engine(&job.agent_id).await {
        Some(e) => e,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "agent not found"})),
            )
                .into_response()
        }
    };

    let db = infra.db.clone();
    let task_message = job.task_message.clone();
    let agent_id = job.agent_id.clone();
    let announce_to = job.announce_to.clone();
    let tool_policy = job.tool_policy.clone();

    // Run asynchronously
    tokio::spawn(async move {
        let msg = hydeclaw_types::IncomingMessage {
            user_id: "system".to_string(),
            text: Some(task_message),
            attachments: vec![],
            agent_id: agent_id.clone(),
            channel: crate::agent::channel_kind::channel::CRON.to_string(),
            context: announce_to.unwrap_or(serde_json::Value::Null),
            timestamp: chrono::Utc::now(),
            formatting_prompt: None,
            tool_policy_override: tool_policy,
            leaf_message_id: None,
        };

        // Record cron run start
        let run_id: Option<uuid::Uuid> = sqlx::query_scalar(
            "INSERT INTO cron_runs (job_id, agent_id) VALUES ($1, $2) RETURNING id",
        )
        .bind(id)
        .bind(&agent_id)
        .fetch_optional(&db)
        .await
        .ok()
        .flatten();

        match engine.handle_isolated(&msg).await {
            Ok(reply) => {
                sqlx::query("UPDATE scheduled_jobs SET last_run_at = now() WHERE id = $1")
                    .bind(id)
                    .execute(&db)
                    .await
                    .ok();
                if let Some(rid) = run_id {
                    let preview = reply.chars().take(500).collect::<String>();
                    sqlx::query(
                        "UPDATE cron_runs SET status = 'success', finished_at = now(), \
                         response_preview = $2 WHERE id = $1",
                    )
                    .bind(rid)
                    .bind(&preview)
                    .execute(&db)
                    .await
                    .ok();
                }
                tracing::info!(job_id = %id, agent = %agent_id, "manual cron run completed");
            }
            Err(e) => {
                if let Some(rid) = run_id {
                    sqlx::query(
                        "UPDATE cron_runs SET status = 'error', finished_at = now(), \
                         error = $2 WHERE id = $1",
                    )
                    .bind(rid)
                    .bind(format!("{e:#}"))
                    .execute(&db)
                    .await
                    .ok();
                }
                tracing::error!(job_id = %id, agent = %agent_id, error = %e, "manual cron run failed");
            }
        }
    });

    Json(json!({"ok": true, "message": "job started"})).into_response()
}

// ── Cron Runs API ──

#[derive(Debug, Deserialize)]
pub(crate) struct CronRunsQuery {
    limit: Option<i64>,
    days: Option<i64>,
}

pub(crate) async fn api_cron_runs(
    State(infra): State<InfraServices>,
    axum::extract::Path(id): axum::extract::Path<uuid::Uuid>,
    Query(q): Query<CronRunsQuery>,
) -> impl IntoResponse {
    let limit = q.limit.unwrap_or(20).min(100);
    let rows = sqlx::query(
        "SELECT id, job_id, agent_id, started_at, finished_at, status, error, response_preview \
         FROM cron_runs WHERE job_id = $1 ORDER BY started_at DESC LIMIT $2",
    )
    .bind(id)
    .bind(limit)
    .fetch_all(&infra.db)
    .await;

    match rows {
        Ok(rows) => {
            let runs: Vec<CronRunDto> = rows
                .iter()
                .map(|r| CronRunDto {
                    id: r.get::<uuid::Uuid, _>("id").to_string(),
                    job_id: r.get::<uuid::Uuid, _>("job_id").to_string(),
                    job_name: None,
                    agent_id: r.get::<String, _>("agent_id"),
                    started_at: r.get::<chrono::DateTime<chrono::Utc>, _>("started_at").to_rfc3339(),
                    finished_at: r.get::<Option<chrono::DateTime<chrono::Utc>>, _>("finished_at").map(|d| d.to_rfc3339()),
                    status: r.get::<String, _>("status"),
                    error: r.get::<Option<String>, _>("error"),
                    response_preview: r.get::<Option<String>, _>("response_preview"),
                })
                .collect();
            Json(json!({ "runs": runs })).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
    }
}

pub(crate) async fn api_cron_runs_all(
    State(infra): State<InfraServices>,
    Query(q): Query<CronRunsQuery>,
) -> impl IntoResponse {
    let limit = q.limit.unwrap_or(50).min(200);
    let days = q.days.unwrap_or(7).min(90);
    let rows = sqlx::query(
        "SELECT r.id, r.job_id, r.agent_id, r.started_at, r.finished_at, r.status, r.error, r.response_preview, \
                j.name as job_name \
         FROM cron_runs r LEFT JOIN scheduled_jobs j ON r.job_id = j.id \
         WHERE r.started_at > now() - make_interval(days => $1) \
         ORDER BY r.started_at DESC LIMIT $2",
    )
    .bind(days as i32)
    .bind(limit)
    .fetch_all(&infra.db)
    .await;

    match rows {
        Ok(rows) => {
            let runs: Vec<CronRunDto> = rows
                .iter()
                .map(|r| CronRunDto {
                    id: r.get::<uuid::Uuid, _>("id").to_string(),
                    job_id: r.get::<uuid::Uuid, _>("job_id").to_string(),
                    job_name: r.get::<Option<String>, _>("job_name"),
                    agent_id: r.get::<String, _>("agent_id"),
                    started_at: r.get::<chrono::DateTime<chrono::Utc>, _>("started_at").to_rfc3339(),
                    finished_at: r.get::<Option<chrono::DateTime<chrono::Utc>>, _>("finished_at").map(|d| d.to_rfc3339()),
                    status: r.get::<String, _>("status"),
                    error: r.get::<Option<String>, _>("error"),
                    response_preview: r.get::<Option<String>, _>("response_preview"),
                })
                .collect();
            Json(json!({ "runs": runs })).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
    }
}

#[cfg(test)]
mod cron_tests {
    #[test]
    fn create_cron_request_accepts_tool_policy() {
        let json = serde_json::json!({
            "name": "daily-summary",
            "agent": "main",
            "task": "summarize today",
            "cron": "0 9 * * *",
            "tool_policy": {"allow": ["memory_search"], "deny": []}
        });
        let req: super::CreateCronRequest = serde_json::from_value(json).unwrap();
        assert!(req.tool_policy.is_some());
    }

    #[test]
    fn create_cron_request_without_tool_policy() {
        let json = serde_json::json!({
            "name": "test",
            "agent": "main",
            "task": "do something",
            "cron": "0 * * * *"
        });
        let req: super::CreateCronRequest = serde_json::from_value(json).unwrap();
        assert!(req.tool_policy.is_none());
    }
}
