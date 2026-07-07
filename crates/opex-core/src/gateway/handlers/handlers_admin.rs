//! Admin surface for the File Handler Hub — the "File Handlers" (Обработчики
//! файлов) tab under Tools. Read-only manifest listing + the builtin allowlist
//! toggle. The allowlist is the SAME single store the composer's
//! `/api/files/{id}/actions` reads (`system_flags['fse.allowlist.enabled']`
//! via `get_enabled_allowlist`), so toggling here changes which builtin
//! buttons appear per-file. Behind bearer auth (merged in `gateway/mod.rs`);
//! not loopback-exempt.

use std::path::{Path, PathBuf};

use axum::{
    Router,
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
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
use crate::gateway::clusters::{ConfigServices, InfraServices};

// ── Toolgate validation client ────────────────────────────────────────────────

const MAX_HANDLER_BYTES: usize = 256 * 1024;

static HANDLERS_HTTP: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
fn handlers_http() -> &'static reqwest::Client {
    HANDLERS_HTTP.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new())
    })
}

/// Ask toolgate to validate a handler source (exec-free). Returns `Ok(())` on
/// `ok: true`, `Err(errors_json)` on `ok: false`, `Err(...)` on transport
/// failure. Fail-closed: a validation we cannot run must NOT write the file.
async fn toolgate_validate(
    toolgate_url: &str,
    id: &str,
    source: &str,
) -> Result<(), serde_json::Value> {
    let url = format!("{}/handlers/validate", toolgate_url.trim_end_matches('/'));
    let resp = handlers_http()
        .post(&url)
        .json(&json!({ "source": source, "id": id }))
        .send()
        .await
        .map_err(|e| json!({ "errors": [{ "field": "toolgate", "message": e.to_string() }] }))?;
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| json!({ "errors": [{ "field": "toolgate", "message": e.to_string() }] }))?;
    if body.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
        Ok(())
    } else {
        Err(body)
    }
}

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
    pub source: String,
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
            source: m.source.clone(),
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

/// Body for `POST /api/handlers` — create a new workspace handler.
#[derive(Debug, Deserialize)]
pub(crate) struct CreateHandlerBody {
    pub id: String,
    pub source: String,
}

/// Body for `PUT /api/handlers/{id}` — edit/overwrite a handler.
#[derive(Debug, Deserialize)]
pub(crate) struct UpdateHandlerBody {
    pub source: String,
}

/// `id` must be `^[a-z0-9_-]+$` — no path separators (traversal-safe).
fn valid_handler_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

/// Workspace override/handler path: `workspace/file_handlers/{id}.py`.
fn workspace_handler_path(id: &str) -> PathBuf {
    Path::new(crate::config::WORKSPACE_DIR)
        .join("file_handlers")
        .join(format!("{id}.py"))
}

/// Pristine builtin source path (READ-ONLY): `toolgate/handlers/builtin/{id}.py`.
/// Relative like `WORKSPACE_DIR` — resolves against the core process CWD (the
/// deploy root `~/opex`, where `~/opex/toolgate/` lives). If CWD ever differs,
/// the builtin-source GET degrades to a graceful 404 (empty editor start) —
/// never a write path (builtin source is never written).
fn builtin_handler_path(id: &str) -> PathBuf {
    Path::new("toolgate")
        .join("handlers")
        .join("builtin")
        .join(format!("{id}.py"))
}

/// Returns true if `id` is one of the 5 hard-coded builtin handler ids.
fn is_builtin_id(id: &str) -> bool {
    FSE_DEFAULT_ALLOWLIST.contains(&id)
}

/// Closed-domain check: only a member of the hard-coded `FSE_DEFAULT_ALLOWLIST`
/// may be toggled (can never admit `code_exec` / a YAML tool). Mirrors the
/// legacy `file_scenarios::is_allowlist_member`.
fn is_allowlist_member(name: &str) -> bool {
    FSE_DEFAULT_ALLOWLIST.contains(&name)
}

fn too_big(src: &str) -> bool {
    src.len() > MAX_HANDLER_BYTES
}

/// Write `source` to `path` and trigger a best-effort registry refresh.
/// toolgate hot-reloads via watchfiles independently; the refresh keeps
/// the in-process cache warm for the immediate GET after create/edit.
async fn write_and_refresh(
    handlers: &HandlerRegistry,
    path: &std::path::Path,
    source: &str,
) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        tokio::fs::create_dir_all(dir).await?;
    }
    tokio::fs::write(path, source).await?;
    handlers.refresh().await;
    Ok(())
}

// ── Routes ───────────────────────────────────────────────────────────────────

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/handlers", get(api_list_handlers).post(api_create_handler))
        .route(
            "/api/handlers/allowlist",
            get(api_get_allowlist).put(api_set_allowlist),
        )
        // Literal "/validate" beats the "/{id}" capture in axum 0.8 — same
        // precedence rule as "/allowlist" above. No write: validate-only proxy.
        .route("/api/handlers/validate", post(api_validate_handler))
        .route("/api/handlers/{id}/source", get(api_get_handler_source))
        .route(
            "/api/handlers/{id}/config",
            get(api_get_handler_config).put(api_set_handler_config),
        )
        .route(
            "/api/handlers/{id}",
            axum::routing::put(api_update_handler).delete(api_delete_handler),
        )
}

#[derive(Deserialize)]
struct ConfigQuery {
    agent: Option<String>,
}

/// `GET /api/handlers/{id}/config?agent=NAME` → `{ fields, values }`.
/// `fields` are the handler's declared `<config>` descriptor fields; `values`
/// are the operator's saved settings for this (handler, agent) — `{}` if none.
async fn api_get_handler_config(
    State(infra): State<InfraServices>,
    State(handlers): State<HandlerRegistry>,
    axum::extract::Path(id): axum::extract::Path<String>,
    axum::extract::Query(q): axum::extract::Query<ConfigQuery>,
) -> impl IntoResponse {
    if !valid_handler_id(&id) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "invalid handler id" })),
        )
            .into_response();
    }
    handlers.refresh().await;
    let fields = handlers
        .manifests()
        .await
        .into_iter()
        .find(|m| m.id == id)
        .map(|m| m.config)
        .unwrap_or_else(|| json!([]));
    let agent = q.agent.unwrap_or_default();
    let values = if agent.is_empty() {
        json!({})
    } else {
        crate::db::handler_config::get_config(&infra.db, &id, &agent)
            .await
            .unwrap_or_else(|_| json!({}))
    };
    Json(json!({ "fields": fields, "values": values })).into_response()
}

/// `PUT /api/handlers/{id}/config?agent=NAME` body `{ "values": {..} }` — upsert
/// the operator's per-agent settings for this handler.
async fn api_set_handler_config(
    State(infra): State<InfraServices>,
    axum::extract::Path(id): axum::extract::Path<String>,
    axum::extract::Query(q): axum::extract::Query<ConfigQuery>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    if !valid_handler_id(&id) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "invalid handler id" })),
        )
            .into_response();
    }
    let agent = q.agent.unwrap_or_default();
    if agent.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "agent query parameter is required" })),
        )
            .into_response();
    }
    let values = body.get("values").cloned().unwrap_or_else(|| json!({}));
    if !values.is_object() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "'values' must be an object" })),
        )
            .into_response();
    }
    match crate::db::handler_config::set_config(&infra.db, &id, &agent, &values).await {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

// ── Handlers ─────────────────────────────────────────────────────────────────

/// `POST /api/handlers/validate` — proxy to toolgate's exec-free validation
/// endpoint. Returns the full verdict JSON (`{ok, descriptor, errors}`) so the
/// UI can read `descriptor` to populate the editor form on mount / "Sync from
/// code". Never writes anything. Fail-soft on transport error: returns
/// `{ok:false, descriptor:null, errors:[…]}` with 200 so the UI can render the
/// error inline instead of seeing a 404/500.
async fn api_validate_handler(
    State(config): State<ConfigServices>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let source = match body.get("source").and_then(|v| v.as_str()) {
        Some(s) => s.to_string(),
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "missing or non-string 'source' field" })),
            )
                .into_response();
        }
    };
    if source.len() > MAX_HANDLER_BYTES {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "source too large" })),
        )
            .into_response();
    }
    let toolgate_url = config
        .config
        .toolgate_url
        .clone()
        .unwrap_or_else(|| "http://localhost:9011".to_string());
    let url = format!(
        "{}/handlers/validate",
        toolgate_url.trim_end_matches('/')
    );
    // Forward the whole body (preserves optional `id` field if the caller sent it).
    match handlers_http().post(&url).json(&body).send().await {
        Ok(resp) => match resp.json::<serde_json::Value>().await {
            Ok(verdict) => Json(verdict).into_response(),
            Err(e) => Json(json!({
                "ok": false,
                "descriptor": null,
                "errors": [{ "field": "toolgate", "message": e.to_string() }]
            }))
            .into_response(),
        },
        Err(e) => Json(json!({
            "ok": false,
            "descriptor": null,
            "errors": [{ "field": "toolgate", "message": e.to_string() }]
        }))
        .into_response(),
    }
}

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

/// `GET /api/handlers/{id}/source` → raw `.py` for the editor. Precedence:
/// workspace override → pristine builtin (starting point for a new override) →
/// workspace-only handler. 404 if none exists and id is not a builtin.
async fn api_get_handler_source(
    axum::extract::Path(id): axum::extract::Path<String>,
) -> impl IntoResponse {
    if !valid_handler_id(&id) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "invalid handler id" })),
        )
            .into_response();
    }
    let ws = workspace_handler_path(&id);
    if let Ok(src) = tokio::fs::read_to_string(&ws).await {
        let kind = if is_builtin_id(&id) { "override" } else { "workspace" };
        return Json(json!({ "id": id, "source": src, "source_kind": kind })).into_response();
    }
    if is_builtin_id(&id)
        && let Ok(src) = tokio::fs::read_to_string(builtin_handler_path(&id)).await
    {
        return Json(json!({ "id": id, "source": src, "source_kind": "builtin" }))
            .into_response();
    }
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "error": "handler source not found" })),
    )
        .into_response()
}

/// `POST /api/handlers` — create a NEW workspace handler.
/// Rejects a builtin id (use PUT to create an override) or an existing
/// workspace file. Validates via toolgate before writing — fail-closed.
async fn api_create_handler(
    State(infra): State<InfraServices>,
    State(config): State<ConfigServices>,
    State(handlers): State<HandlerRegistry>,
    Json(body): Json<CreateHandlerBody>,
) -> impl IntoResponse {
    if !valid_handler_id(&body.id) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "invalid handler id" })),
        )
            .into_response();
    }
    if too_big(&body.source) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "source too large" })),
        )
            .into_response();
    }
    if is_builtin_id(&body.id) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "id is a builtin; edit it via PUT to create an override" })),
        )
            .into_response();
    }
    let path = workspace_handler_path(&body.id);
    if path.exists() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "handler already exists" })),
        )
            .into_response();
    }
    let toolgate_url = config
        .config
        .toolgate_url
        .clone()
        .unwrap_or_else(|| "http://localhost:9011".to_string());
    if let Err(errs) = toolgate_validate(&toolgate_url, &body.id, &body.source).await {
        return (StatusCode::BAD_REQUEST, Json(errs)).into_response();
    }
    if let Err(e) = write_and_refresh(&handlers, &path, &body.source).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response();
    }
    crate::db::audit::audit_spawn(
        infra.db.clone(),
        String::new(),
        crate::db::audit::event_types::HANDLER_CREATED,
        Some("ui".into()),
        json!({ "id": body.id }),
    );
    (StatusCode::CREATED, Json(json!({ "id": body.id }))).into_response()
}

/// `PUT /api/handlers/{id}` — edit a handler. Builtin id → writes/updates the
/// workspace override; workspace id → overwrites its file. Validates via
/// toolgate before writing — fail-closed.
async fn api_update_handler(
    State(infra): State<InfraServices>,
    State(config): State<ConfigServices>,
    State(handlers): State<HandlerRegistry>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(body): Json<UpdateHandlerBody>,
) -> impl IntoResponse {
    if !valid_handler_id(&id) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "invalid handler id" })),
        )
            .into_response();
    }
    if too_big(&body.source) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "source too large" })),
        )
            .into_response();
    }
    let toolgate_url = config
        .config
        .toolgate_url
        .clone()
        .unwrap_or_else(|| "http://localhost:9011".to_string());
    if let Err(errs) = toolgate_validate(&toolgate_url, &id, &body.source).await {
        return (StatusCode::BAD_REQUEST, Json(errs)).into_response();
    }
    let path = workspace_handler_path(&id);
    if let Err(e) = write_and_refresh(&handlers, &path, &body.source).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response();
    }
    crate::db::audit::audit_spawn(
        infra.db.clone(),
        String::new(),
        crate::db::audit::event_types::HANDLER_UPDATED,
        Some("ui".into()),
        json!({ "id": id }),
    );
    (StatusCode::OK, Json(json!({ "id": id }))).into_response()
}

/// `DELETE /api/handlers/{id}` — delete a workspace handler, or RESET a builtin
/// (delete its override → the pristine builtin resurfaces). Pristine builtin → 400.
async fn api_delete_handler(
    State(infra): State<InfraServices>,
    State(handlers): State<HandlerRegistry>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> impl IntoResponse {
    if !valid_handler_id(&id) {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "invalid handler id" }))).into_response();
    }
    let path = workspace_handler_path(&id);
    if !path.exists() {
        // No workspace file: a pristine builtin cannot be deleted; anything else is 404.
        let code = if is_builtin_id(&id) { StatusCode::BAD_REQUEST } else { StatusCode::NOT_FOUND };
        let msg = if is_builtin_id(&id) { "builtin handlers cannot be deleted (already at default)" } else { "handler not found" };
        return (code, Json(json!({ "error": msg }))).into_response();
    }
    if let Err(e) = tokio::fs::remove_file(&path).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))).into_response();
    }
    handlers.refresh().await;
    crate::db::audit::audit_spawn(infra.db.clone(), String::new(),
        crate::db::audit::event_types::HANDLER_DELETED, Some("ui".into()),
        json!({ "id": id, "reset": is_builtin_id(&id) }));
    let reset = is_builtin_id(&id);
    (StatusCode::OK, Json(json!({ "id": id, "reset": reset }))).into_response()
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
            config: serde_json::Value::Null,
            order: 0,
            tier: tier.to_string(),
            source: String::new(),
        }
    }

    #[test]
    fn handler_id_validation_blocks_traversal() {
        assert!(valid_handler_id("my_ocr"));
        assert!(valid_handler_id("summarize_video"));
        assert!(!valid_handler_id("../etc/passwd"));
        assert!(!valid_handler_id("a/b"));
        assert!(!valid_handler_id("Bad"));
        assert!(!valid_handler_id(""));
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

    #[test]
    fn create_guards() {
        assert!(is_builtin_id("transcribe"));
        assert!(!is_builtin_id("my_ocr"));
        assert!(too_big(&"x".repeat(MAX_HANDLER_BYTES + 1)));
        assert!(!too_big("small"));
        assert_eq!(
            workspace_handler_path("my_ocr"),
            std::path::Path::new("workspace/file_handlers/my_ocr.py")
        );
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
            config: serde_json::Value::Null,
            order: 0,
            tier: "builtin".to_string(),
            source: String::new(),
        };
        let row = HandlerAdminRow::from_manifest(&m, &get_enabled_allowlist(&pool).await);
        assert!(!row.enabled, "describe is not in the enabled set → disabled");
    }
}
