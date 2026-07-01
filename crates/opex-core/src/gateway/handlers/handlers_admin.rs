//! Admin surface for the File Handler Hub — the "File Handlers" (Обработчики
//! файлов) tab under Tools. Read-only manifest listing + the builtin allowlist
//! toggle. The allowlist is the SAME single store the composer's
//! `/api/files/{id}/actions` reads (`system_flags['fse.allowlist.enabled']`
//! via `get_enabled_allowlist`), so toggling here changes which builtin
//! buttons appear per-file. Behind bearer auth (merged in `gateway/mod.rs`);
//! not loopback-exempt.

use axum::{
    Router,
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::agent::fse::{
    get_enabled_allowlist, get_enabled_allowlist_strict, is_allowed_for_autorun,
    set_enabled_allowlist_checked, FSE_DEFAULT_ALLOWLIST,
};
use crate::agent::handler_registry::{HandlerManifest, HandlerRegistry};
use crate::gateway::AppState;
use crate::gateway::clusters::InfraServices;

// ── Response / request types ───────────────────────────────────────────────────

/// One handler row for the admin tab: the toolgate manifest plus the derived
/// `enabled` flag (builtin → allowlist-gated; workspace → always true).
/// `params` is intentionally omitted (the admin tab renders no param schema).
#[derive(Debug, Clone, Serialize)]
pub(crate) struct HandlerAdminRow {
    pub id: String,
    pub labels: std::collections::HashMap<String, String>,
    pub descriptions: std::collections::HashMap<String, String>,
    pub icon: String,
    #[serde(rename = "match")]
    pub match_: crate::agent::handler_registry::HandlerMatch,
    pub capability: Option<String>,
    pub provider: Option<String>,
    pub execution: String,
    pub output: String,
    pub order: i32,
    pub tier: String,
    pub enabled: bool,
}

impl HandlerAdminRow {
    fn from_manifest(m: &HandlerManifest, enabled_allowlist: &[String]) -> Self {
        let enabled = if m.tier == "builtin" {
            is_allowed_for_autorun(&m.id, enabled_allowlist)
        } else {
            true
        };
        Self {
            id: m.id.clone(),
            labels: m.labels.clone(),
            descriptions: m.descriptions.clone(),
            icon: m.icon.clone(),
            match_: m.match_.clone(),
            capability: m.capability.clone(),
            provider: m.provider.clone(),
            execution: m.execution.clone(),
            output: m.output.clone(),
            order: m.order,
            tier: m.tier.clone(),
            enabled,
        }
    }
}

/// Body for `PUT /api/handlers/allowlist` — toggles one builtin member.
#[derive(Debug, Deserialize)]
pub(crate) struct SetAllowlistBody {
    pub action_ref: String,
    pub enabled: bool,
}

/// Closed-domain check: only a member of the hard-coded `FSE_DEFAULT_ALLOWLIST`
/// may be toggled (can never admit `code_exec` / a YAML tool). Mirrors the
/// legacy `file_scenarios::is_allowlist_member`.
fn is_allowlist_member(name: &str) -> bool {
    FSE_DEFAULT_ALLOWLIST.contains(&name)
}

// ── Routes ───────────────────────────────────────────────────────────────────

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/handlers", get(api_list_handlers))
        .route(
            "/api/handlers/allowlist",
            get(api_get_allowlist).put(api_set_allowlist),
        )
}

// ── Handlers ─────────────────────────────────────────────────────────────────

/// `GET /api/handlers` → `{ handlers: [HandlerAdminRow...] }`. Lists ALL
/// registered manifests (no upload needed), each annotated with `enabled`.
/// Fail-soft: `refresh()` keeps stale/empty cache on toolgate error → an empty
/// list is returned (the tab shows an empty-state), never a 500.
async fn api_list_handlers(
    State(infra): State<InfraServices>,
    State(handlers): State<HandlerRegistry>,
) -> impl IntoResponse {
    handlers.refresh().await;
    let manifests = handlers.manifests().await;
    let enabled = get_enabled_allowlist(&infra.db).await;
    let mut rows: Vec<HandlerAdminRow> = manifests
        .iter()
        .map(|m| HandlerAdminRow::from_manifest(m, &enabled))
        .collect();
    rows.sort_by(|a, b| a.order.cmp(&b.order).then_with(|| a.id.cmp(&b.id)));
    Json(json!({ "handlers": rows })).into_response()
}

/// `GET /api/handlers/allowlist` → the 5 const members + enabled state.
/// Wrapper over `get_enabled_allowlist` — same store as the composer.
async fn api_get_allowlist(State(infra): State<InfraServices>) -> impl IntoResponse {
    let enabled_set = get_enabled_allowlist(&infra.db).await;
    let members: Vec<serde_json::Value> = FSE_DEFAULT_ALLOWLIST
        .iter()
        .map(|m| {
            let is_enabled = enabled_set.iter().any(|e| e == m);
            json!({ "action_ref": m, "enabled": is_enabled })
        })
        .collect();
    (StatusCode::OK, Json(json!({ "allowlist": members }))).into_response()
}

/// `PUT /api/handlers/allowlist` body `{action_ref, enabled}` → toggle one
/// builtin member. Non-member → 400. DB read error before mutation → 500
/// (fail-CLOSED: we never let a transient SELECT error silently re-enable all
/// builtins). Audit fires only inside the confirmed-write `Ok` arm.
async fn api_set_allowlist(
    State(infra): State<InfraServices>,
    Json(body): Json<SetAllowlistBody>,
) -> impl IntoResponse {
    if !is_allowlist_member(&body.action_ref) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": format!(
                    "'{}' is not a member of the allowlist; only {} may be toggled",
                    body.action_ref,
                    FSE_DEFAULT_ALLOWLIST.join(", ")
                )
            })),
        )
            .into_response();
    }

    // Strict read — propagate DB errors instead of silently defaulting to the
    // full constant (which would re-enable previously-disabled builtins).
    let mut current = match get_enabled_allowlist_strict(&infra.db).await {
        Ok(list) => list,
        Err(e) => {
            tracing::error!(error = %e, "api_set_allowlist: failed to read current allowlist");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "failed to read allowlist state; try again" })),
            )
                .into_response();
        }
    };

    if body.enabled {
        if !current.iter().any(|m| m == &body.action_ref) {
            current.push(body.action_ref.clone());
        }
    } else {
        current.retain(|m| m != &body.action_ref);
    }

    // Checked write — validation + upsert, both errors surfaced as 500.
    match set_enabled_allowlist_checked(&infra.db, current).await {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::handler_registry::{HandlerManifest, HandlerMatch};

    fn manifest(id: &str, tier: &str) -> HandlerManifest {
        HandlerManifest {
            id: id.to_string(),
            labels: Default::default(),
            descriptions: Default::default(),
            icon: String::new(),
            match_: HandlerMatch::default(),
            capability: None,
            provider: None,
            execution: "sync".to_string(),
            output: String::new(),
            params: serde_json::Value::Null,
            order: 0,
            tier: tier.to_string(),
        }
    }

    #[test]
    fn builtin_enabled_follows_allowlist() {
        let enabled = vec!["transcribe".to_string()];
        let on = HandlerAdminRow::from_manifest(&manifest("transcribe", "builtin"), &enabled);
        let off = HandlerAdminRow::from_manifest(&manifest("describe", "builtin"), &enabled);
        assert!(on.enabled, "allowlisted builtin must be enabled");
        assert!(!off.enabled, "non-allowlisted builtin must be disabled");
    }

    #[test]
    fn workspace_always_enabled() {
        let row = HandlerAdminRow::from_manifest(&manifest("my_handler", "workspace"), &[]);
        assert!(row.enabled, "workspace handlers are never allowlist-gated");
    }

    #[test]
    fn non_member_is_rejected_by_membership_guard() {
        assert!(!is_allowlist_member("code_exec"));
        assert!(is_allowlist_member("transcribe"));
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn allowlist_toggle_round_trips_through_shared_store(pool: sqlx::PgPool) {
        use crate::agent::fse::{get_enabled_allowlist, set_enabled_allowlist};
        // Start from a known state: only "transcribe" enabled.
        set_enabled_allowlist(&pool, &["transcribe".to_string()])
            .await
            .unwrap();
        let got = get_enabled_allowlist(&pool).await;
        assert_eq!(got, vec!["transcribe".to_string()]);
        // A HandlerAdminRow for a builtin reflects that store exactly.
        let m = HandlerManifest {
            id: "describe".to_string(),
            labels: Default::default(),
            descriptions: Default::default(),
            icon: String::new(),
            match_: crate::agent::handler_registry::HandlerMatch::default(),
            capability: None,
            provider: None,
            execution: "sync".to_string(),
            output: String::new(),
            params: serde_json::Value::Null,
            order: 0,
            tier: "builtin".to_string(),
        };
        let row = HandlerAdminRow::from_manifest(&m, &get_enabled_allowlist(&pool).await);
        assert!(!row.enabled, "describe is not in the enabled set → disabled");
    }
}
