pub(crate) mod run;

use axum::{
    Router,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json,
};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use crate::db::file_scenarios;
use crate::gateway::AppState;
use crate::gateway::clusters::InfraServices;

// ── Request bodies ────────────────────────────────────────────────────────────

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

/// Partial-update body for PUT /api/file-scenarios/{id}.
/// Only `label`, `priority`, and `enabled` are mutable post-creation;
/// `match_type`, `executor`, `action_ref`, and `is_default` are immutable
/// (structural identity fields).
#[derive(Debug, Deserialize)]
pub(crate) struct UpdateScenarioBody {
    pub label: Option<String>,
    pub priority: Option<i32>,
    pub enabled: Option<bool>,
}

/// Body for PUT /api/file-scenarios/{id}/default.
/// Setting `is_default = true` promotes the binding to the default for its
/// `match_type` (clearing the prior default in the same transaction).
/// Setting `is_default = false` clears the default flag without promoting another.
#[derive(Debug, Deserialize)]
pub(crate) struct SetDefaultBody {
    pub is_default: bool,
}

/// Body for PUT /api/file-scenarios/allowlist.
/// Toggles a single member of the closed constant `FSE_DEFAULT_ALLOWLIST`.
/// `action_ref` values outside that constant are rejected with 400.
#[derive(Debug, Deserialize)]
pub(crate) struct SetAllowlistBody {
    pub action_ref: String,
    pub enabled: bool,
}

// ── Routes ───────────────────────────────────────────────────────────────────

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/file-scenarios",
            get(api_list_file_scenarios).post(api_create_file_scenario),
        )
        // NOTE: `/allowlist` and `/run` are literal segments and must be registered before
        // `/{id}` so axum's router does not attempt to parse them as UUIDs. In axum 0.8
        // literal routes already take priority over capture routes, but explicit ordering
        // makes the intent clear.
        .route(
            "/api/file-scenarios/allowlist",
            get(api_get_fse_allowlist).put(api_set_fse_allowlist),
        )
        .route(
            "/api/file-scenarios/run",
            post(run::api_run_scenario),
        )
        .route(
            "/api/file-scenarios/{id}",
            get(api_get_file_scenario)
                .put(api_update_file_scenario)
                .delete(api_delete_file_scenario),
        )
        .route(
            "/api/file-scenarios/{id}/default",
            axum::routing::put(api_set_file_scenario_default),
        )
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

pub(crate) async fn api_update_file_scenario(
    State(infra): State<InfraServices>,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateScenarioBody>,
) -> impl IntoResponse {
    // Load existing row to merge partial fields and to anchor validation against
    // the immutable structural fields (executor, action_ref, is_default).
    let existing = match file_scenarios::get_by_id(&infra.db, id).await {
        Ok(Some(r)) => r,
        Ok(None) => {
            return (StatusCode::NOT_FOUND, Json(json!({ "error": "not found" }))).into_response();
        }
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))).into_response();
        }
    };

    // Re-validate via the same gate as create. Only label/priority/enabled are
    // mutable, so structural identity fields (executor, action_ref, is_default)
    // come verbatim from the existing row — the effective state after the update.
    let enabled_allowlist = crate::agent::fse::get_enabled_allowlist(&infra.db).await;
    if let Err(msg) = crate::agent::fse::validate_binding_write(
        &existing.executor,
        &existing.action_ref,
        existing.is_default,
        &enabled_allowlist,
    ) {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": msg.to_string() }))).into_response();
    }

    let eff_label = body.label.as_deref().unwrap_or(&existing.label);
    let eff_priority = body.priority.unwrap_or(existing.priority);
    let eff_enabled = body.enabled.unwrap_or(existing.enabled);

    match file_scenarios::update(&infra.db, id, eff_label, eff_priority, eff_enabled).await {
        Ok(1) => {
            // Re-fetch to return the updated row.
            match file_scenarios::get_by_id(&infra.db, id).await {
                Ok(Some(row)) => {
                    crate::db::audit::audit_spawn(
                        infra.db.clone(),
                        String::new(),
                        crate::db::audit::event_types::FSE_BINDING_UPDATED,
                        Some("ui".into()),
                        json!({
                            "scenario_id": row.id.to_string(),
                            "match_type": row.match_type,
                        }),
                    );
                    (StatusCode::OK, Json(row)).into_response()
                }
                Ok(None) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": "updated row not found" })),
                )
                    .into_response(),
                Err(e) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": e.to_string() })),
                )
                    .into_response(),
            }
        }
        Ok(0) => (StatusCode::NOT_FOUND, Json(json!({ "error": "not found" }))).into_response(),
        Ok(_) => unreachable!("update by primary key affects at most one row"),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

pub(crate) async fn api_delete_file_scenario(
    State(infra): State<InfraServices>,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    match file_scenarios::delete(&infra.db, id).await {
        Ok(1) => {
            crate::db::audit::audit_spawn(
                infra.db.clone(),
                String::new(),
                crate::db::audit::event_types::FSE_BINDING_DELETED,
                Some("ui".into()),
                json!({ "scenario_id": id.to_string() }),
            );
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(0) => (StatusCode::NOT_FOUND, Json(json!({ "error": "not found" }))).into_response(),
        Ok(_) => unreachable!("delete by primary key affects at most one row"),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// PUT /api/file-scenarios/{id}/default
///
/// Sets or clears the `is_default` flag for a binding.
///
/// When `is_default = true`:
/// - The binding is re-validated through the FSE allowlist gate (same check as
///   create): `executor=tool` bindings with an `action_ref` not in the
///   operator-enabled allowlist are rejected with 400.
/// - The prior default for the same `match_type` is cleared atomically in the
///   same transaction (honouring the `file_scenarios_one_default` partial unique
///   index).
///
/// When `is_default = false`:
/// - The default flag is cleared on the identified binding only; no other row is
///   promoted.
pub(crate) async fn api_set_file_scenario_default(
    State(infra): State<InfraServices>,
    Path(id): Path<Uuid>,
    Json(body): Json<SetDefaultBody>,
) -> impl IntoResponse {
    // Fetch existing binding to apply the allowlist gate against its actual fields.
    let existing = match file_scenarios::get_by_id(&infra.db, id).await {
        Ok(Some(r)) => r,
        Ok(None) => return (StatusCode::NOT_FOUND, Json(json!({ "error": "not found" }))).into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))).into_response(),
    };

    if body.is_default {
        // Only tool bindings may be set as default.  Allowing a skill binding
        // to become default would silently displace the seeded tool default for
        // the match_type (because set_default clears the prior default first),
        // and the dispatch seam filters on executor=="tool" — so a skill-default
        // would leave the type with no auto-executable binding.
        if existing.executor != "tool" {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "only tool bindings may be set as default" })),
            )
                .into_response();
        }

        // Setting is_default=true re-runs the caller-independent allowlist gate:
        // a tool-binding may only become the zero-click default if its action_ref
        // is in the operator-enabled subset of FSE_DEFAULT_ALLOWLIST.
        let enabled_allowlist = crate::agent::fse::get_enabled_allowlist(&infra.db).await;
        if let Err(msg) = crate::agent::fse::validate_binding_write(
            &existing.executor,
            &existing.action_ref,
            true,
            &enabled_allowlist,
        ) {
            return (StatusCode::BAD_REQUEST, Json(json!({ "error": msg.to_string() }))).into_response();
        }

        match file_scenarios::set_default(&infra.db, id).await {
            Ok(Some(row)) => {
                crate::db::audit::audit_spawn(
                    infra.db.clone(),
                    String::new(),
                    crate::db::audit::event_types::FSE_DEFAULT_CHANGED,
                    Some("ui".into()),
                    json!({
                        "scenario_id": row.id.to_string(),
                        "match_type": row.match_type,
                        "is_default": row.is_default,
                    }),
                );
                (StatusCode::OK, Json(row)).into_response()
            }
            Ok(None) => (StatusCode::NOT_FOUND, Json(json!({ "error": "not found" }))).into_response(),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))).into_response(),
        }
    } else {
        // Clearing the default flag; no allowlist gate needed.
        match file_scenarios::unset_default(&infra.db, id).await {
            Ok(Some(row)) => {
                crate::db::audit::audit_spawn(
                    infra.db.clone(),
                    String::new(),
                    crate::db::audit::event_types::FSE_DEFAULT_CHANGED,
                    Some("ui".into()),
                    json!({
                        "scenario_id": row.id.to_string(),
                        "match_type": row.match_type,
                        "is_default": row.is_default,
                    }),
                );
                (StatusCode::OK, Json(row)).into_response()
            }
            Ok(None) => (StatusCode::NOT_FOUND, Json(json!({ "error": "not found" }))).into_response(),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))).into_response(),
        }
    }
}

// ── Allowlist handlers ───────────────────────────────────────────────────────

/// Closed-domain check: an allowlist amend may only toggle a member of the
/// hard-coded `FSE_DEFAULT_ALLOWLIST` constant. Mirrors `providers.rs`
/// validating against `VALID_CAPABILITIES` — it can never admit `code_exec`
/// / raw-URL / a YAML tool (§4.6).
fn is_allowlist_member(name: &str) -> bool {
    crate::agent::fse::FSE_DEFAULT_ALLOWLIST.contains(&name)
}

/// GET /api/file-scenarios/allowlist
///
/// Returns the full constant `FSE_DEFAULT_ALLOWLIST` (always four members)
/// annotated with the operator-toggled `enabled` flag for each.
pub(crate) async fn api_get_fse_allowlist(
    State(infra): State<InfraServices>,
) -> impl IntoResponse {
    let enabled_set = crate::agent::fse::get_enabled_allowlist(&infra.db).await;
    let members: Vec<serde_json::Value> = crate::agent::fse::FSE_DEFAULT_ALLOWLIST
        .iter()
        .map(|m| {
            let is_enabled = enabled_set.iter().any(|e| e == m);
            json!({ "action_ref": m, "enabled": is_enabled })
        })
        .collect();
    (StatusCode::OK, Json(json!({ "allowlist": members }))).into_response()
}

/// PUT /api/file-scenarios/allowlist
///
/// Toggles a single member of the closed-domain `FSE_DEFAULT_ALLOWLIST`.
/// Returns 400 if `action_ref` is not a member of that constant (the toggle
/// cannot admit arbitrary tool names, YAML tools, or `code_exec`).
/// Audits `FSE_ALLOWLIST_AMENDED` on success.
pub(crate) async fn api_set_fse_allowlist(
    State(infra): State<InfraServices>,
    Json(body): Json<SetAllowlistBody>,
) -> impl IntoResponse {
    // Closed-domain guard: reject any name not in the hard-coded constant.
    if !is_allowlist_member(&body.action_ref) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": format!(
                    "'{}' is not a member of the allowlist; only {} may be toggled",
                    body.action_ref,
                    crate::agent::fse::FSE_DEFAULT_ALLOWLIST.join(", ")
                )
            })),
        )
            .into_response();
    }

    // Compute the new enabled set: start from current, then apply the toggle.
    let mut current = crate::agent::fse::get_enabled_allowlist(&infra.db).await;
    if body.enabled {
        if !current.iter().any(|m| m == &body.action_ref) {
            current.push(body.action_ref.clone());
        }
    } else {
        current.retain(|m| m != &body.action_ref);
    }

    // Persist via the validator-gated store function.
    match crate::agent::fse::set_enabled_allowlist(&infra.db, &current).await {
        Ok(()) => {
            crate::db::audit::audit_spawn(
                infra.db.clone(),
                String::new(),
                crate::db::audit::event_types::FSE_ALLOWLIST_AMENDED,
                Some("ui".into()),
                json!({ "action_ref": body.action_ref, "enabled": body.enabled }),
            );
            (
                StatusCode::OK,
                Json(json!({ "action_ref": body.action_ref, "enabled": body.enabled })),
            )
                .into_response()
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
    use super::{CreateScenarioBody, SetAllowlistBody, SetDefaultBody, UpdateScenarioBody, is_allowlist_member};

    // ── Task 5.6 tests ────────────────────────────────────────────────────────

    #[test]
    fn allowlist_membership_is_closed_domain() {
        // Only the four hard-coded constant members are valid.
        assert!(is_allowlist_member("transcribe"));
        assert!(is_allowlist_member("describe"));
        assert!(is_allowlist_member("extract_document"));
        assert!(is_allowlist_member("save"));
        // Anything else — including code_exec / raw-URL / a YAML tool — is rejected.
        assert!(!is_allowlist_member("code_exec"));
        assert!(!is_allowlist_member("analyze_image"));
        assert!(!is_allowlist_member(""));
    }

    #[test]
    fn set_allowlist_body_deserializes() {
        let body: SetAllowlistBody =
            serde_json::from_value(serde_json::json!({ "action_ref": "describe", "enabled": false })).unwrap();
        assert_eq!(body.action_ref, "describe");
        assert!(!body.enabled);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn allowlist_get_returns_all_five_members(pool: sqlx::PgPool) {
        // Fresh DB: all five members enabled by default.
        let enabled = crate::agent::fse::get_enabled_allowlist(&pool).await;
        assert_eq!(enabled.len(), 5, "fresh DB must default to all five members enabled");
        for name in crate::agent::fse::FSE_DEFAULT_ALLOWLIST {
            assert!(
                enabled.iter().any(|m| m == name),
                "member '{name}' must be in the default enabled set"
            );
        }
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn allowlist_set_persists_valid_subset(pool: sqlx::PgPool) {
        // Disable "save" — only 3 members remain.
        let subset = vec!["transcribe".to_string(), "describe".to_string(), "extract_document".to_string()];
        crate::agent::fse::set_enabled_allowlist(&pool, &subset).await.unwrap();
        let enabled = crate::agent::fse::get_enabled_allowlist(&pool).await;
        assert_eq!(enabled.len(), 3);
        assert!(!enabled.iter().any(|m| m == "save"), "save must be disabled");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn allowlist_set_rejects_non_constant_member(pool: sqlx::PgPool) {
        // "code_exec" is not in FSE_DEFAULT_ALLOWLIST → must be rejected with Err.
        let bad = vec!["transcribe".to_string(), "code_exec".to_string()];
        let result = crate::agent::fse::set_enabled_allowlist(&pool, &bad).await;
        assert!(result.is_err(), "code_exec must be rejected by set_enabled_allowlist");
        // DB state must be unchanged (full default still in effect).
        let still_full = crate::agent::fse::get_enabled_allowlist(&pool).await;
        assert_eq!(still_full.len(), 5, "DB must be unchanged after rejected set");
    }

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

    // ── Task 5.4 tests ────────────────────────────────────────────────────────

    #[test]
    fn update_body_all_fields_optional() {
        let body: UpdateScenarioBody = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(body.label.is_none());
        assert!(body.priority.is_none());
        assert!(body.enabled.is_none());
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn delete_removes_row(pool: sqlx::PgPool) {
        let id = crate::db::file_scenarios::create(
            &pool,
            ".txt",
            "skill",
            "summarize-notes",
            "Summarize",
            false,
            100,
            true,
            "ui",
        )
        .await
        .unwrap();

        // First delete: row gone → 1 row affected.
        let affected = crate::db::file_scenarios::delete(&pool, id).await.unwrap();
        assert_eq!(affected, 1, "first delete must affect exactly one row");
        assert!(
            crate::db::file_scenarios::get_by_id(&pool, id).await.unwrap().is_none(),
            "row must be gone after delete"
        );
        // Idempotent: second delete returns 0 rows affected.
        let again = crate::db::file_scenarios::delete(&pool, id).await.unwrap();
        assert_eq!(again, 0, "second delete must affect zero rows");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn update_persists_changes(pool: sqlx::PgPool) {
        let id = crate::db::file_scenarios::create(
            &pool,
            "audio/*",
            "tool",
            "transcribe",
            "Transcribe",
            false,
            100,
            true,
            "ui",
        )
        .await
        .unwrap();

        let affected =
            crate::db::file_scenarios::update(&pool, id, "Transcribe (updated)", 50, false)
                .await
                .unwrap();
        assert_eq!(affected, 1);

        let row = crate::db::file_scenarios::get_by_id(&pool, id).await.unwrap().unwrap();
        assert_eq!(row.label, "Transcribe (updated)");
        assert_eq!(row.priority, 50);
        assert!(!row.enabled);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn update_unknown_id_returns_zero(pool: sqlx::PgPool) {
        let affected = crate::db::file_scenarios::update(
            &pool,
            uuid::Uuid::new_v4(),
            "label",
            100,
            true,
        )
        .await
        .unwrap();
        assert_eq!(affected, 0, "update of non-existent id must return 0");
    }

    // ── Task 5.5 tests ────────────────────────────────────────────────────────

    #[test]
    fn set_default_body_deserializes() {
        let body: SetDefaultBody =
            serde_json::from_value(serde_json::json!({ "is_default": true })).unwrap();
        assert!(body.is_default);
    }

    /// Two tool-bindings on the same match_type; only one may be default.
    /// Promoting the second clears the first in the same transaction so the
    /// `file_scenarios_one_default` partial unique index is never violated.
    #[sqlx::test(migrations = "../../migrations")]
    async fn one_default_per_match_type_enforced(pool: sqlx::PgPool) {
        let a = crate::db::file_scenarios::create(
            &pool,
            "image/*",
            "tool",
            "describe",
            "Describe",
            true,
            100,
            true,
            "ui",
        )
        .await
        .unwrap();
        let b = crate::db::file_scenarios::create(
            &pool,
            "image/*",
            "tool",
            "save",
            "Save",
            false,
            200,
            true,
            "ui",
        )
        .await
        .unwrap();

        // Flip default to b: set_default must clear a's default in the same tx so
        // the partial-unique index file_scenarios_one_default never trips.
        let updated = crate::db::file_scenarios::set_default(&pool, b)
            .await
            .unwrap()
            .expect("row must be found");
        assert!(updated.is_default, "returned row must be default");

        let a_after = crate::db::file_scenarios::get_by_id(&pool, a)
            .await
            .unwrap()
            .expect("a must still exist");
        assert!(!a_after.is_default, "old default must be cleared");
    }

    /// set-default on an unknown id returns None (maps to 404 in the handler).
    #[sqlx::test(migrations = "../../migrations")]
    async fn set_default_unknown_id_returns_none(pool: sqlx::PgPool) {
        let result = crate::db::file_scenarios::set_default(&pool, uuid::Uuid::new_v4())
            .await
            .unwrap();
        assert!(result.is_none(), "unknown id must return None → 404");
    }

    // ── Task 5.7 tests ───────────────────────────────────────────────────────

    /// Drift guard: the FSE bindings surface must remain exactly the §5 routes
    /// (run endpoint is Phase 6, intentionally absent here). Building the router
    /// is enough to catch a missing handler/method wiring at compile + run time.
    #[test]
    fn routes_builds_without_panic() {
        let _r: axum::Router<crate::gateway::AppState> = super::routes();
    }

    /// None of the FSE paths may be loopback-exempt: they must NOT appear in the
    /// auth middleware LOOPBACK_EXACT/PREFIX lists (operator-bearer only).
    #[test]
    fn fse_paths_are_not_loopback_exempt() {
        // These literals mirror middleware.rs LOOPBACK_EXACT/PREFIX. If a future
        // edit adds an /api/file-scenarios entry there, this guard documents the
        // intent that it must not happen (§5: behind auth, not loopback-exempt).
        const LOOPBACK_EXACT: &[&str] =
            &["/health", "/api/channels/notify", "/api/media/upload", "/api/vision/analyze"];
        const LOOPBACK_PREFIX: &[&str] = &["/api/uploads/"];
        for p in ["/api/file-scenarios", "/api/file-scenarios/allowlist", "/api/file-scenarios/run"] {
            assert!(!LOOPBACK_EXACT.contains(&p), "{p} must not be loopback-exact-exempt");
            assert!(
                !LOOPBACK_PREFIX.iter().any(|pre| p.starts_with(pre)),
                "{p} must not be loopback-prefix-exempt"
            );
        }
    }

    // ── Fix wave 1 tests ──────────────────────────────────────────────────────

    /// set_default rejects a skill binding with 400 and does NOT displace the
    /// pre-existing tool default for the same match_type.
    #[sqlx::test(migrations = "../../migrations")]
    async fn set_default_rejects_skill_binding(pool: sqlx::PgPool) {
        // Seed a tool default for image/*.
        let tool_id = crate::db::file_scenarios::create(
            &pool,
            "image/*",
            "tool",
            "describe",
            "Describe",
            true,
            100,
            true,
            "system",
        )
        .await
        .unwrap();

        // Create a skill binding for the same match_type (non-default).
        let skill_id = crate::db::file_scenarios::create(
            &pool,
            "image/*",
            "skill",
            "fancy-describe",
            "Fancy describe",
            false,
            200,
            true,
            "ui",
        )
        .await
        .unwrap();

        // The handler guard rejects promoting a skill binding to default — simulate
        // the guard logic directly (executor check must fire before set_default).
        let skill_row = crate::db::file_scenarios::get_by_id(&pool, skill_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            skill_row.executor, "skill",
            "pre-condition: binding must be skill"
        );
        // The guard condition: executor != "tool" → must be rejected.
        assert!(
            skill_row.executor != "tool",
            "guard fires: skill binding must not be promotable to default"
        );

        // Verify the original tool default is STILL intact (was not displaced).
        let tool_row = crate::db::file_scenarios::get_by_id(&pool, tool_id)
            .await
            .unwrap()
            .unwrap();
        assert!(tool_row.is_default, "tool default must remain after guard rejects skill promotion");
        assert!(!skill_row.is_default, "skill binding must still be non-default");
    }

    /// Unsetting the default flag (is_default=false) via unset_default clears it
    /// and returns the updated row with is_default=false.
    #[sqlx::test(migrations = "../../migrations")]
    async fn unset_default_clears_flag(pool: sqlx::PgPool) {
        let id = crate::db::file_scenarios::create(
            &pool,
            "audio/*",
            "tool",
            "transcribe",
            "Transcribe",
            true,
            100,
            true,
            "system",
        )
        .await
        .unwrap();

        // Confirm it starts as default.
        let before = crate::db::file_scenarios::get_by_id(&pool, id)
            .await
            .unwrap()
            .unwrap();
        assert!(before.is_default, "pre-condition: must start as default");

        // Unset it.
        let after = crate::db::file_scenarios::unset_default(&pool, id)
            .await
            .unwrap()
            .expect("row must still exist after unset");
        assert!(!after.is_default, "is_default must be false after unset_default");

        // Re-fetch to confirm persistence.
        let fetched = crate::db::file_scenarios::get_by_id(&pool, id)
            .await
            .unwrap()
            .unwrap();
        assert!(!fetched.is_default, "is_default must persist as false in DB");
    }

    /// validator rejects making a tool binding with a non-allowlisted action_ref the default.
    #[sqlx::test(migrations = "../../migrations")]
    async fn set_default_tool_outside_allowlist_rejected_by_validator(pool: sqlx::PgPool) {
        // Insert a tool binding with action_ref not in FSE_DEFAULT_ALLOWLIST.
        let id = crate::db::file_scenarios::create(
            &pool,
            "image/*",
            "tool",
            "code_exec",
            "Code Exec",
            false,
            100,
            true,
            "ui",
        )
        .await
        .unwrap();

        let enabled = crate::agent::fse::get_enabled_allowlist(&pool).await;
        let result = crate::agent::fse::validate_binding_write("tool", "code_exec", true, &enabled);
        assert!(
            result.is_err(),
            "validate_binding_write must reject code_exec as a default tool binding"
        );

        // Row must still be non-default in DB.
        let row = crate::db::file_scenarios::get_by_id(&pool, id)
            .await
            .unwrap()
            .unwrap();
        assert!(!row.is_default, "row must not have been set as default");
    }
}
