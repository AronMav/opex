use axum::{
    Router,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Json,
};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use crate::db::file_scenarios;
use crate::gateway::AppState;
use crate::gateway::clusters::InfraServices;

// ── Request body ─────────────────────────────────────────────────────────────

fn default_priority() -> i32 {
    100
}
fn default_enabled() -> bool {
    true
}

#[derive(Debug, Deserialize)]
pub(crate) struct CreateScenarioBody {
    pub match_type: String,
    pub executor: String,
    pub action_ref: String,
    pub label: String,
    #[serde(default)]
    pub is_default: bool,
    #[serde(default = "default_priority")]
    pub priority: i32,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

// ── Routes ───────────────────────────────────────────────────────────────────

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/file-scenarios",
            get(api_list_file_scenarios).post(api_create_file_scenario),
        )
        .route("/api/file-scenarios/{id}", get(api_get_file_scenario))
}

// ── Handlers ─────────────────────────────────────────────────────────────────

pub(crate) async fn api_list_file_scenarios(
    State(infra): State<InfraServices>,
) -> impl IntoResponse {
    match file_scenarios::list(&infra.db).await {
        Ok(rows) => (StatusCode::OK, Json(json!({ "scenarios": rows }))).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

pub(crate) async fn api_create_file_scenario(
    State(infra): State<InfraServices>,
    Json(body): Json<CreateScenarioBody>,
) -> impl IntoResponse {
    // Phase-4 allowlist gate: executor=tool + is_default=true rows must have
    // an action_ref that is both a constant member and operator-enabled.
    let enabled_allowlist = crate::agent::fse::get_enabled_allowlist(&infra.db).await;
    if let Err(msg) = crate::agent::fse::validate_binding_write(
        &body.executor,
        &body.action_ref,
        body.is_default,
        &enabled_allowlist,
    ) {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": msg.to_string() }))).into_response();
    }

    // `scope` is intentionally not settable via this endpoint: defaults to 'global'
    // per spec §9. Per-agent scope is future work.
    match file_scenarios::create(
        &infra.db,
        &body.match_type,
        &body.executor,
        &body.action_ref,
        &body.label,
        body.is_default,
        body.priority,
        body.enabled,
        "ui",
    )
    .await
    {
        Ok(id) => {
            // Audit: fire-and-forget, does not affect the response.
            // agent_id empty: this is an operator/UI HTTP write with no per-request agent scope;
            // actor=Some("ui") carries attribution.
            crate::db::audit::audit_spawn(
                infra.db.clone(),
                String::new(),
                crate::db::audit::event_types::FSE_BINDING_CREATED,
                Some("ui".into()),
                json!({
                    "scenario_id": id.to_string(),
                    "match_type": body.match_type,
                    "executor": body.executor,
                    "action_ref": body.action_ref,
                    "is_default": body.is_default,
                }),
            );

            match file_scenarios::get_by_id(&infra.db, id).await {
                Ok(Some(row)) => (StatusCode::CREATED, Json(row)).into_response(),
                Ok(None) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": "created row not found" })),
                )
                    .into_response(),
                Err(e) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": e.to_string() })),
                )
                    .into_response(),
            }
        }
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("unique") || msg.contains("duplicate") {
                (
                    StatusCode::CONFLICT,
                    Json(
                        json!({ "error": "a binding for this match_type + action_ref already exists" }),
                    ),
                )
                    .into_response()
            } else {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": msg })),
                )
                    .into_response()
            }
        }
    }
}

pub(crate) async fn api_get_file_scenario(
    State(infra): State<InfraServices>,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    match file_scenarios::get_by_id(&infra.db, id).await {
        Ok(Some(row)) => (StatusCode::OK, Json(row)).into_response(),
        Ok(None) => {
            (StatusCode::NOT_FOUND, Json(json!({ "error": "not found" }))).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::CreateScenarioBody;

    #[test]
    fn create_body_deserializes_with_defaults() {
        let body: CreateScenarioBody = serde_json::from_value(serde_json::json!({
            "match_type": "application/pdf",
            "executor": "tool",
            "action_ref": "extract_document",
            "label": "Extract PDF"
        }))
        .unwrap();
        assert_eq!(body.match_type, "application/pdf");
        assert_eq!(body.executor, "tool");
        assert!(!body.is_default, "is_default defaults to false");
        assert_eq!(body.priority, 100, "priority defaults to 100");
        assert!(body.enabled, "enabled defaults to true");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn create_then_get_roundtrips(pool: sqlx::PgPool) {
        let id = crate::db::file_scenarios::create(
            &pool,
            "audio/*",
            "tool",
            "transcribe",
            "Transcribe audio",
            true,
            100,
            true,
            "ui",
        )
        .await
        .unwrap();

        let fetched = crate::db::file_scenarios::get_by_id(&pool, id)
            .await
            .unwrap()
            .expect("row exists");
        assert_eq!(fetched.action_ref, "transcribe");
        assert!(fetched.is_default);
    }

    // Unit test of the validator layer that the create handler calls. A handler-level
    // integration test (HTTP -> 400 + empty DB) is deferred to Phase 9's e2e regression suite.
    #[sqlx::test(migrations = "../../migrations")]
    async fn create_default_tool_outside_allowlist_is_rejected_by_validator(pool: sqlx::PgPool) {
        // executor=tool + is_default=true + action_ref=code_exec must be rejected
        // by validate_binding_write even when the allowlist is full.
        let enabled = crate::agent::fse::get_enabled_allowlist(&pool).await;
        let result = crate::agent::fse::validate_binding_write("tool", "code_exec", true, &enabled);
        assert!(result.is_err(), "code_exec default must be rejected by validator");

        // DB must also have no row persisted (validator would prevent reaching DB).
        let rows = crate::db::file_scenarios::list(&pool).await.unwrap();
        assert!(
            rows.iter().all(|r| r.action_ref != "code_exec"),
            "no code_exec row should exist"
        );
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn get_by_id_returns_404_sentinel_for_unknown(pool: sqlx::PgPool) {
        let missing = crate::db::file_scenarios::get_by_id(&pool, uuid::Uuid::new_v4())
            .await
            .unwrap();
        assert!(missing.is_none(), "unknown id must return None");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn list_returns_seeded_and_inserted_rows(pool: sqlx::PgPool) {
        // Insert one binding directly (Phase-1 schema columns; id is not
        // auto-generated by the DB so we supply gen_random_uuid()).
        sqlx::query(
            "INSERT INTO file_scenarios \
             (id, match_type, executor, action_ref, label, is_default, priority, enabled, scope, created_by) \
             VALUES (gen_random_uuid(),'image/*','tool','describe','Describe image',true,100,true,'global','ui')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let rows = crate::db::file_scenarios::list(&pool).await.unwrap();
        assert!(
            rows.iter().any(|r| r.match_type == "image/*" && r.action_ref == "describe"),
            "inserted image/* describe binding must be listed: {rows:?}"
        );
    }
}
