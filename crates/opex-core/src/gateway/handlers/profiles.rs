use axum::{
    Router,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::agent::handler_registry::HandlerRegistry;
use crate::db::profiles::{self, ProfileRow, Slots};
use crate::gateway::AppState;
use crate::gateway::clusters::{
    AgentCore, AuthServices, ChannelBus, ConfigServices, InfraServices, StatusMonitor,
};
use crate::gateway::handlers::agents::start_agent_from_config;

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/profiles", get(api_list_profiles).post(api_create_profile))
        .route(
            "/api/profiles/{id}",
            get(api_get_profile).put(api_update_profile).delete(api_delete_profile),
        )
        .route("/api/profiles/{id}/copy", post(api_copy_profile))
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// The "Default" profile is protected: it cannot be renamed or deleted.
fn is_protected_profile(name: &str) -> bool {
    name == crate::db::profiles::DEFAULT_PROFILE
}

/// Names of loaded agents whose config `profile` equals `profile_name`.
/// Reads the agent config directory on disk (same source `agents.rs` uses),
/// so the result reflects the current TOML files, not in-memory engines.
/// `Err` propagates a config-enumeration failure so destructive guards
/// (DELETE, rename) can fail closed instead of silently treating "cannot
/// verify" as "not in use".
fn agents_using_profile_checked(profile_name: &str) -> anyhow::Result<Vec<String>> {
    Ok(crate::config::load_agent_configs("config/agents")?
        .into_iter()
        .filter(|c| c.agent.profile == profile_name)
        .map(|c| c.agent.name)
        .collect())
}

/// Tolerant wrapper for display-only use (GET-list `agents` array): an
/// enumeration failure degrades to an empty list rather than blocking the
/// read. Never use this for a destructive guard — see
/// `agents_using_profile_checked`.
fn agents_using_profile(profile_name: &str) -> Vec<String> {
    agents_using_profile_checked(profile_name).unwrap_or_default()
}

/// Serialize a profile row and attach the `agents` array (names of agents
/// bound to this profile).
fn profile_json_with_agents(row: &ProfileRow) -> Value {
    let agents = agents_using_profile(&row.name);
    let mut v = serde_json::to_value(row).unwrap_or_default();
    if let Some(map) = v.as_object_mut() {
        map.insert("agents".into(), json!(agents));
    }
    v
}

/// Restart every live agent bound to `profile_name` so a profile edit takes
/// effect immediately. Mirrors the hot-restart sequence at the tail of
/// `api_update_agent` (crud.rs): remove the handle + access guard, shut the old
/// engine down, then `start_agent_from_config` and re-insert on success. A
/// single agent's restart failure is logged and skipped — it never aborts the
/// enclosing PUT.
///
/// Also called by `api_update_provider` (providers.rs) when a provider's
/// identity changes (base_url, model, key) — the agent must rebuild its
/// RoutingProvider to pick up the new settings without a core restart.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn hot_reload_agents_for_profile(
    profile_name: &str,
    agents: &AgentCore,
    infra: &InfraServices,
    auth: &AuthServices,
    bus: &ChannelBus,
    cfg_svc: &ConfigServices,
    status: &StatusMonitor,
    handlers: &HandlerRegistry,
) {
    let configs = match crate::config::load_agent_configs("config/agents") {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, profile = %profile_name,
                "profile hot-reload: failed to load agent configs");
            return;
        }
    };

    let matching: Vec<_> = configs.into_iter().filter(|c| c.agent.profile == profile_name).collect();
    if matching.is_empty() {
        tracing::warn!(profile = %profile_name,
            "profile hot-reload: no agents bound to this profile (check agent TOML `profile` field)");
    }

    for cfg in matching {
        let agent_name = cfg.agent.name.clone();

        // Stop the old engine (if running) and drop its access guard.
        let old_handle = agents.map.write().await.remove(&agent_name);
        auth.access_guards.write().await.remove(&agent_name);
        if let Some(handle) = &old_handle {
            // Cancel in-flight requests so active sessions reconnect against
            // the freshly-swapped engine (otherwise an active turn keeps the
            // old Arc<AgentEngine> with stale profile_slots until it finishes).
            handle.engine.state().cancel_all_requests();
            handle.engine.state().wait_drain(std::time::Duration::from_secs(5)).await;
        }
        if let Some(handle) = old_handle {
            handle.shutdown(&agents.scheduler).await;
        }

        match start_agent_from_config(
            &cfg, agents, infra, auth, bus, cfg_svc, status, handlers,
        )
        .await
        {
            Ok((handle, guard)) => {
                // Guard before handle — same ordering as api_update_agent.
                if let Some(guard) = guard {
                    auth.access_guards.write().await.insert(agent_name.clone(), guard);
                }
                agents.map.write().await.insert(agent_name.clone(), handle);
                tracing::info!(agent = %agent_name, profile = %profile_name,
                    "agent hot-reloaded after profile update");
            }
            Err(e) => {
                tracing::error!(agent = %agent_name, profile = %profile_name, error = %e,
                    "failed to restart agent after profile update; skipping");
            }
        }
    }
}

// ── CRUD handlers ───────────────────────────────────────────────────────────

pub(crate) async fn api_list_profiles(State(infra): State<InfraServices>) -> impl IntoResponse {
    match profiles::list_profiles(&infra.db).await {
        Ok(rows) => {
            let out: Vec<Value> = rows.iter().map(profile_json_with_agents).collect();
            (StatusCode::OK, Json(json!({ "profiles": out }))).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct CreateProfileBody {
    pub name: String,
    #[serde(default)]
    pub slots: Slots,
}

pub(crate) async fn api_create_profile(
    State(infra): State<InfraServices>,
    Json(body): Json<CreateProfileBody>,
) -> impl IntoResponse {
    if body.name.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "name is required" }))).into_response();
    }
    // Validate slots before writing.
    if let Err(msg) = profiles::validate_slots(&infra.db, &body.slots).await {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": msg }))).into_response();
    }
    match profiles::create_profile(&infra.db, &body.name, &body.slots).await {
        Ok(row) => (StatusCode::CREATED, Json(json!(row))).into_response(),
        Err(e) if e.to_string().contains("unique") || e.to_string().contains("duplicate") => (
            StatusCode::CONFLICT,
            Json(json!({ "error": "a profile with this name already exists" })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

pub(crate) async fn api_get_profile(
    State(infra): State<InfraServices>,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    match profiles::get_profile(&infra.db, id).await {
        Ok(Some(row)) => (StatusCode::OK, Json(json!(row))).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, Json(json!({ "error": "not found" }))).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct UpdateProfileBody {
    pub name: Option<String>,
    pub slots: Option<Slots>,
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn api_update_profile(
    State(agents): State<AgentCore>,
    State(infra): State<InfraServices>,
    State(auth): State<AuthServices>,
    State(bus): State<ChannelBus>,
    State(cfg_svc): State<ConfigServices>,
    State(status): State<StatusMonitor>,
    State(handlers): State<HandlerRegistry>,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateProfileBody>,
) -> impl IntoResponse {
    // Load the pre-update row: needed for Default-rename guard and to know which
    // profile name the affected agents reference (their config points at the
    // *current* name).
    let existing = match profiles::get_profile(&infra.db, id).await {
        Ok(Some(row)) => row,
        Ok(None) => return (StatusCode::NOT_FOUND, Json(json!({ "error": "not found" }))).into_response(),
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": e.to_string() })),
            )
                .into_response();
        }
    };

    // Validate slots (if provided) before writing.
    if let Some(ref slots) = body.slots
        && let Err(msg) = profiles::validate_slots(&infra.db, slots).await
    {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": msg }))).into_response();
    }

    // Reject a blank name outright — mirrors the POST guard. Without this,
    // `name: Some("")` flows through `COALESCE($2, name)` and blanks the row.
    if let Some(ref new_name) = body.name
        && new_name.trim().is_empty()
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "profile name cannot be empty" })),
        )
            .into_response();
    }

    // The Default profile cannot be renamed.
    if is_protected_profile(&existing.name)
        && let Some(ref new_name) = body.name
        && new_name != crate::db::profiles::DEFAULT_PROFILE
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "the Default profile cannot be renamed" })),
        )
            .into_response();
    }

    // Renaming a non-Default profile does NOT cascade to agent TOMLs — an
    // in-use profile being renamed would silently detach every agent whose
    // config still points at the old name (they'd fall back to Default at
    // next resolve). Same footgun the DELETE in-use guard protects against.
    // Only fires on an actual name CHANGE — a no-op self-rename is allowed.
    if let Some(ref new_name) = body.name
        && *new_name != existing.name
    {
        match agents_using_profile_checked(&existing.name) {
            Ok(names) if !names.is_empty() => {
                return (
                    StatusCode::CONFLICT,
                    Json(json!({
                        "error": "profile_in_use",
                        "agents": names,
                        "hint": "rename blocked: profile is assigned to agents; reassign them first",
                    })),
                )
                    .into_response();
            }
            Ok(_) => {}
            Err(_) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({
                        "error": "cannot verify profile usage",
                        "hint": "agent config enumeration failed",
                    })),
                )
                    .into_response();
            }
        }
    }

    let updated = match profiles::update_profile(
        &infra.db,
        id,
        body.name.as_deref(),
        body.slots.as_ref(),
    )
    .await
    {
        Ok(Some(row)) => row,
        Ok(None) => return (StatusCode::NOT_FOUND, Json(json!({ "error": "not found" }))).into_response(),
        Err(e) if e.to_string().contains("unique") || e.to_string().contains("duplicate") => {
            return (
                StatusCode::CONFLICT,
                Json(json!({ "error": "a profile with this name already exists" })),
            )
                .into_response();
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": e.to_string() })),
            )
                .into_response();
        }
    };

    // Hot-reload every agent bound to this profile. Agent configs reference the
    // profile by its pre-update name, so filter on `existing.name`.
    hot_reload_agents_for_profile(
        &existing.name, &agents, &infra, &auth, &bus, &cfg_svc, &status, &handlers,
    )
    .await;

    (StatusCode::OK, Json(json!(updated))).into_response()
}

pub(crate) async fn api_copy_profile(
    State(infra): State<InfraServices>,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    match profiles::copy_profile(&infra.db, id).await {
        Ok(Some(row)) => (StatusCode::CREATED, Json(json!(row))).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, Json(json!({ "error": "not found" }))).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

pub(crate) async fn api_delete_profile(
    State(infra): State<InfraServices>,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    let existing = match profiles::get_profile(&infra.db, id).await {
        Ok(Some(row)) => row,
        Ok(None) => return (StatusCode::NOT_FOUND, Json(json!({ "error": "not found" }))).into_response(),
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": e.to_string() })),
            )
                .into_response();
        }
    };

    // The Default profile cannot be deleted.
    if is_protected_profile(&existing.name) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "the Default profile cannot be deleted" })),
        )
            .into_response();
    }

    // In-use guard: refuse if any agent references this profile. Fail closed
    // on a config-enumeration error — an unreadable config dir must NOT be
    // treated as "no agents use this profile" (see agents_using_profile_checked).
    let in_use = match agents_using_profile_checked(&existing.name) {
        Ok(names) => names,
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({
                    "error": "cannot verify profile usage",
                    "hint": "agent config enumeration failed",
                })),
            )
                .into_response();
        }
    };
    if !in_use.is_empty() {
        return (
            StatusCode::CONFLICT,
            Json(json!({ "error": "profile_in_use", "agents": in_use })),
        )
            .into_response();
    }

    match profiles::delete_profile(&infra.db, id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => (StatusCode::NOT_FOUND, Json(json!({ "error": "not found" }))).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_profile_is_protected() {
        assert!(is_protected_profile("Default"));
        assert!(!is_protected_profile("Custom"));
    }
}
