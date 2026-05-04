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
use sqlx::PgPool;
use uuid::Uuid;

use std::sync::LazyLock;
use regex::Regex;

use crate::agent::providers::PROVIDER_CREDENTIALS;
use crate::db::providers::{self, CreateProvider, UpdateProvider, ProviderRow};
use crate::gateway::AppState;
use crate::gateway::clusters::{AuthServices, ConfigServices, InfraServices};
use crate::secrets::SecretsManager;
use super::secrets::mask_secret_value;

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/provider-types", get(api_list_provider_types))
        .route("/api/media-drivers", get(api_list_media_drivers))
        .route("/api/media-config", get(api_media_config_export))
        .route("/api/providers", get(api_list_providers).post(api_create_provider))
        .route("/api/providers/{id}", get(api_get_provider).put(api_update_provider).delete(api_delete_provider).patch(api_patch_cli_options))
        .route("/api/providers/{id}/models", get(api_unified_provider_models))
        .route("/api/providers/{id}/resolve", get(api_provider_resolve))
        .route("/api/providers/{id}/test-cli", post(api_test_cli))
        .route("/api/provider-active", get(api_list_provider_active).put(api_set_provider_active))
}

// ── Constants ───────────────────────────────────────────────────────────────
const VALID_TYPES: &[&str] = &["text", "stt", "tts", "vision", "imagegen", "embedding"];
const VALID_CAPABILITIES: &[&str] = &["stt", "tts", "vision", "imagegen", "embedding"];

static NAME_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[a-zA-Z0-9_-]+$").expect("valid regex pattern")
});

/// Media capabilities that should trigger toolgate reload when changed.
const MEDIA_CAPABILITIES: &[&str] = &["stt", "tts", "vision", "imagegen", "embedding"];

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Notify toolgate to reload config and invalidate caches.
/// Retries up to 3 times with a 1-second delay between attempts.
pub(crate) fn notify_toolgate_reload(toolgate_url: Option<String>) {
    let url = toolgate_url.unwrap_or_else(|| "http://localhost:9011".to_string());
    tokio::spawn(async move {
        let client = reqwest::Client::new();
        const MAX_ATTEMPTS: u32 = 3;
        for attempt in 1..=MAX_ATTEMPTS {
            match client.post(format!("{url}/reload")).send().await {
                Ok(_) => {
                    tracing::debug!("toolgate config reloaded successfully");
                    return;
                }
                Err(e) if attempt < MAX_ATTEMPTS => {
                    tracing::debug!(attempt, error = %e, "toolgate reload failed, retrying in 1s");
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "failed to reload toolgate config after {MAX_ATTEMPTS} attempts");
                }
            }
        }
    });
}

/// Resolve API key for a provider from vault (scoped by UUID).
async fn resolve_key(secrets: &SecretsManager, provider: &ProviderRow) -> Option<String> {
    secrets.get_scoped(PROVIDER_CREDENTIALS, &provider.id.to_string()).await
}

/// Build the public JSON representation of a provider (masked `api_key`).
async fn provider_json(secrets: &SecretsManager, p: &ProviderRow) -> Value {
    let key = resolve_key(secrets, p).await;
    let mut obj = serde_json::to_value(p).unwrap_or_default();
    if let Some(map) = obj.as_object_mut() {
        map.insert("api_key".into(), json!(key.as_deref().map(mask_secret_value)));
        map.insert("has_api_key".into(), json!(key.is_some()));
    }
    obj
}

// ── CRUD handlers ───────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub(crate) struct ListProvidersQuery {
    #[serde(rename = "type")]
    pub category: Option<String>,
}

pub(crate) async fn api_list_providers(
    State(infra): State<InfraServices>,
    State(auth): State<AuthServices>,
    axum::extract::Query(params): axum::extract::Query<ListProvidersQuery>,
) -> impl IntoResponse {
    let result = if let Some(ref cat) = params.category {
        providers::list_providers_by_type(&infra.db, cat).await
    } else {
        providers::list_providers(&infra.db).await
    };
    match result {
        Ok(providers) => {
            let mut out = Vec::with_capacity(providers.len());
            for p in &providers {
                out.push(provider_json(&auth.secrets, p).await);
            }
            (StatusCode::OK, Json(json!({ "providers": out }))).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))).into_response(),
    }
}

/// Inline body that extends `CreateProvider` with an optional `api_key`.
#[derive(Debug, Deserialize)]
pub(crate) struct CreateProviderBody {
    pub name: String,
    #[serde(rename = "type")]
    pub category: String,
    pub provider_type: String,
    pub base_url: Option<String>,
    pub default_model: Option<String>,
    pub enabled: Option<bool>,
    pub options: Option<Value>,
    pub notes: Option<String>,
    pub api_key: Option<String>,
}

/// Validate a persisted `options` blob as `ProviderOptions` and enforce
/// `timeouts` ranges. Returns a 400-ready error string on failure.
/// Spec §4.3: "validate runs on every load and on every PUT /api/providers write."
fn validate_provider_options(options: Option<&Value>) -> Result<(), String> {
    let Some(raw) = options else { return Ok(()) };
    // Missing `options` or `null` is valid (defaults apply).
    if raw.is_null() {
        return Ok(());
    }
    let opts: crate::agent::providers::timeouts::ProviderOptions =
        serde_json::from_value(raw.clone())
            .map_err(|e| format!("invalid options JSON: {e}"))?;
    opts.validate()
}

/// Pure-options validation for TTS `normalize_provider_id` field:
/// checks the field is a well-formed UUID if present. DB-backed existence
/// + type check happens in `validate_tts_options_db`.
fn validate_tts_options_opts_only(options: &Value) -> Result<(), String> {
    let id = match options.get("normalize_provider_id") {
        Some(v) if !v.is_null() => v,
        _ => return Ok(()),  // missing is fine
    };
    let s = id.as_str()
        .ok_or_else(|| "normalize_provider_id must be a string (uuid)".to_string())?;
    Uuid::parse_str(s)
        .map_err(|e| format!("normalize_provider_id is not a valid uuid: {e}"))?;
    Ok(())
}

/// Full DB-backed validation: UUID shape + provider exists + category=text.
/// Called from api_create_provider / api_update_provider when category == "tts".
async fn validate_tts_options_db(db: &PgPool, options: Option<&Value>) -> Result<(), String> {
    let Some(opts) = options else { return Ok(()) };
    validate_tts_options_opts_only(opts)?;
    let id_str = match opts.get("normalize_provider_id") {
        Some(v) if !v.is_null() => v.as_str().unwrap_or(""),
        _ => return Ok(()),
    };
    let id = Uuid::parse_str(id_str).expect("already validated above");

    // NB: DB column is `type` (Postgres reserved word handled by sqlx). The Rust
    // struct ProviderRow renames it to `category`, but ad-hoc queries must use
    // the actual column name. We compare the string to "text" (the value).
    let row: Option<(String,)> = sqlx::query_as("SELECT type FROM providers WHERE id = $1")
        .bind(id)
        .fetch_optional(db)
        .await
        .map_err(|e| format!("db error checking normalize_provider_id: {e}"))?;
    match row {
        None => Err(format!(
            "normalize_provider_id {id_str} does not reference an existing provider"
        )),
        Some((cat,)) if cat != "text" => Err(format!(
            "normalize_provider_id {id_str} references a '{cat}' provider, expected 'text'"
        )),
        Some(_) => Ok(()),
    }
}

pub(crate) async fn api_create_provider(
    State(infra): State<InfraServices>,
    State(auth): State<AuthServices>,
    State(cfg): State<ConfigServices>,
    Json(body): Json<CreateProviderBody>,
) -> impl IntoResponse {
    // Validate type
    if !VALID_TYPES.contains(&body.category.as_str()) {
        return (StatusCode::BAD_REQUEST, Json(json!({
            "error": format!("invalid type '{}', must be one of: {}", body.category, VALID_TYPES.join(", "))
        }))).into_response();
    }
    // Validate name
    if !NAME_RE.is_match(&body.name) {
        return (StatusCode::BAD_REQUEST, Json(json!({
            "error": "name must match [a-zA-Z0-9_-]+"
        }))).into_response();
    }
    // For type=text, require default_model
    if body.category == "text" && body.default_model.as_ref().is_none_or(std::string::String::is_empty) {
        return (StatusCode::BAD_REQUEST, Json(json!({
            "error": "default_model is required for type=text"
        }))).into_response();
    }
    // Validate ProviderOptions if supplied (timeouts ranges etc.)
    if let Err(msg) = validate_provider_options(body.options.as_ref()) {
        return (StatusCode::BAD_REQUEST, Json(json!({
            "error": format!("invalid options: {msg}")
        }))).into_response();
    }
    // Phase: toolgate-config-sot — validate cross-reference to text provider
    // when creating a TTS provider with normalize_provider_id.
    if body.category == "tts"
        && let Some(opts) = body.options.as_ref()
        && let Err(msg) = validate_tts_options_db(&infra.db, Some(opts)).await
    {
        return (StatusCode::BAD_REQUEST, Json(json!({
            "error": format!("invalid TTS options: {msg}")
        }))).into_response();
    }

    let api_key = body.api_key.clone().filter(|k| !k.is_empty());
    let input = CreateProvider {
        name: body.name,
        category: body.category,
        provider_type: body.provider_type,
        base_url: body.base_url,
        default_model: body.default_model,
        enabled: body.enabled,
        options: body.options,
        notes: body.notes,
    };

    match providers::create_provider(&infra.db, input).await {
        Ok(p) => {
            if let Some(key) = api_key {
                let desc = format!("Credentials for provider '{}'", p.name);
                if let Err(e) = auth.secrets.set_scoped(PROVIDER_CREDENTIALS, &p.id.to_string(), &key, Some(&desc)).await {
                    tracing::warn!(provider = %p.name, error = %e, "failed to store provider key in vault");
                }
            }
            if p.category != "text" {
                notify_toolgate_reload(cfg.config.toolgate_url.clone());
            }
            let json = provider_json(&auth.secrets, &p).await;
            (StatusCode::CREATED, Json(json)).into_response()
        }
        Err(e) if e.to_string().contains("unique") || e.to_string().contains("duplicate") => {
            (StatusCode::CONFLICT, Json(json!({"error": "a provider with this name already exists"}))).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
    }
}

pub(crate) async fn api_get_provider(
    State(infra): State<InfraServices>,
    State(auth): State<AuthServices>,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    match providers::get_provider(&infra.db, id).await {
        Ok(Some(p)) => (StatusCode::OK, Json(provider_json(&auth.secrets, &p).await)).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, Json(json!({"error": "not found"}))).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
    }
}

/// Inline body that extends `UpdateProvider` with an optional `api_key`.
#[derive(Debug, Deserialize)]
pub(crate) struct UpdateProviderBody {
    pub name: Option<String>,
    #[serde(rename = "type")]
    pub category: Option<String>,
    pub provider_type: Option<String>,
    pub base_url: Option<String>,
    pub default_model: Option<String>,
    pub enabled: Option<bool>,
    pub options: Option<Value>,
    pub notes: Option<String>,
    pub api_key: Option<String>,
}

pub(crate) async fn api_update_provider(
    State(infra): State<InfraServices>,
    State(auth): State<AuthServices>,
    State(cfg): State<ConfigServices>,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateProviderBody>,
) -> impl IntoResponse {
    // Validate type if changing
    if let Some(ref cat) = body.category
        && !VALID_TYPES.contains(&cat.as_str())
    {
        return (StatusCode::BAD_REQUEST, Json(json!({
            "error": format!("invalid type '{}', must be one of: {}", cat, VALID_TYPES.join(", "))
        }))).into_response();
    }
    // Validate ProviderOptions if supplied (timeouts ranges etc.)
    if let Err(msg) = validate_provider_options(body.options.as_ref()) {
        return (StatusCode::BAD_REQUEST, Json(json!({
            "error": format!("invalid options: {msg}")
        }))).into_response();
    }
    // Compute the EFFECTIVE category after this update would apply.
    // If body.category is supplied, that wins. Otherwise look up the
    // current row. We only validate TTS options when the row would be
    // (or remain) a TTS provider.
    let needs_tts_check = match body.category.as_deref() {
        Some(cat) => cat == "tts",
        None => {
            let current = sqlx::query_as::<_, (String,)>(
                "SELECT type FROM providers WHERE id = $1"
            ).bind(id).fetch_optional(&infra.db).await
                .inspect_err(|e| tracing::warn!(error = %e,
                    "pre-check SELECT type failed; skipping TTS validation"))
                .ok().flatten();
            matches!(current, Some((ref c,)) if c == "tts")
        }
    };
    if needs_tts_check
        && let Some(opts) = body.options.as_ref()
        && let Err(msg) = validate_tts_options_db(&infra.db, Some(opts)).await
    {
        return (StatusCode::BAD_REQUEST, Json(json!({
            "error": format!("invalid TTS options: {msg}")
        }))).into_response();
    }

    // Check if type is changing — need to clear provider_active references
    let old_provider = if body.category.is_some() {
        providers::get_provider(&infra.db, id).await.ok().flatten()
    } else {
        None
    };

    let api_key = body.api_key.clone().filter(|k| !k.is_empty());
    let input = UpdateProvider {
        name: body.name,
        category: body.category,
        provider_type: body.provider_type,
        base_url: body.base_url,
        default_model: body.default_model,
        enabled: body.enabled,
        options: body.options,
        notes: body.notes,
    };

    match providers::update_provider(&infra.db, id, input).await {
        Ok(Some(p)) => {
            if let Some(key) = api_key {
                let desc = format!("Credentials for provider '{}'", p.name);
                if let Err(e) = auth.secrets.set_scoped(PROVIDER_CREDENTIALS, &p.id.to_string(), &key, Some(&desc)).await {
                    tracing::warn!(provider = %p.name, error = %e, "failed to update provider key in vault");
                }
            }

            // If type changed, clear provider_active entries that referenced this provider by name
            if let Some(old) = old_provider
                && old.category != p.category
            {
                // Clear active binding for old capabilities that referenced this provider
                let active = providers::list_provider_active(&infra.db).await.unwrap_or_default();
                for a in active {
                    if a.provider_name.as_deref() == Some(&p.name) {
                        let _ = providers::set_provider_active(&infra.db, &a.capability, None).await;
                    }
                }
            }

            if p.category != "text" {
                notify_toolgate_reload(cfg.config.toolgate_url.clone());
            }
            let json = provider_json(&auth.secrets, &p).await;
            (StatusCode::OK, Json(json)).into_response()
        }
        Ok(None) => (StatusCode::NOT_FOUND, Json(json!({"error": "not found"}))).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
    }
}

pub(crate) async fn api_delete_provider(
    State(infra): State<InfraServices>,
    State(auth): State<AuthServices>,
    State(cfg): State<ConfigServices>,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    // Check type before deleting to decide about toolgate reload
    let provider = providers::get_provider(&infra.db, id).await.ok().flatten();
    match providers::delete_provider(&infra.db, id).await {
        Ok(true) => {
            if let Err(e) = auth.secrets.delete_scoped(PROVIDER_CREDENTIALS, &id.to_string()).await {
                tracing::debug!(provider = %id, error = %e, "no vault key to delete for provider");
            }
            if provider.is_some_and(|p| p.category != "text") {
                notify_toolgate_reload(cfg.config.toolgate_url.clone());
            }
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => (StatusCode::NOT_FOUND, Json(json!({"error": "not found"}))).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
    }
}

// ── Model discovery ─────────────────────────────────────────────────────────

pub(crate) async fn api_unified_provider_models(
    State(infra): State<InfraServices>,
    State(auth): State<AuthServices>,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    let provider = match providers::get_provider(&infra.db, id).await {
        Ok(Some(p)) => p,
        Ok(None) => return (StatusCode::NOT_FOUND, Json(json!({"error": "provider not found"}))).into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
    };

    let api_key = resolve_key(&auth.secrets, &provider).await;

    let models = crate::agent::model_discovery::discover_models_with_key(
        &provider.provider_type,
        &auth.secrets,
        provider.base_url.as_deref(),
        api_key.as_deref(),
    )
    .await;

    match &models {
        Ok(m) if !m.is_empty() => {
            (StatusCode::OK, Json(json!({ "models": m }))).into_response()
        }
        _ => {
            // For CLI providers: return hardcoded fallback models from preset
            if let Some(preset) = crate::agent::cli_backend::find_preset(&provider.provider_type) {
                let fallback: Vec<crate::agent::model_discovery::ModelInfo> = preset.default_models
                    .iter()
                    .map(|id| crate::agent::model_discovery::ModelInfo {
                        id: (*id).to_string(),
                        owned_by: Some(preset.models_provider.to_string()),
                    })
                    .collect();
                (StatusCode::OK, Json(json!({ "models": fallback, "fallback": true }))).into_response()
            } else {
                // Non-CLI providers: return whatever we got (empty list or error-default)
                let m = models.unwrap_or_default();
                (StatusCode::OK, Json(json!({ "models": m }))).into_response()
            }
        }
    }
}

// ── Resolve (unmasked credentials for internal use) ─────────────────────────

pub(crate) async fn api_provider_resolve(
    State(infra): State<InfraServices>,
    State(auth): State<AuthServices>,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    let provider = match providers::get_provider(&infra.db, id).await {
        Ok(Some(p)) => p,
        Ok(None) => return (StatusCode::NOT_FOUND, Json(json!({"error": "provider not found"}))).into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
    };

    let api_key = resolve_key(&auth.secrets, &provider).await.unwrap_or_default();

    Json(json!({
        "base_url": provider.base_url,
        "provider_type": provider.provider_type,
        "default_model": provider.default_model,
        "api_key": api_key,
    })).into_response()
}

// ── Active handlers ─────────────────────────────────────────────────────────

pub(crate) async fn api_list_provider_active(State(infra): State<InfraServices>) -> impl IntoResponse {
    match providers::list_provider_active(&infra.db).await {
        Ok(active) => (StatusCode::OK, Json(json!({ "active": active }))).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))).into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct SetProviderActiveInput {
    pub capability: String,
    pub provider_name: Option<String>,
}

pub(crate) async fn api_set_provider_active(
    State(infra): State<InfraServices>,
    State(cfg): State<ConfigServices>,
    Json(input): Json<SetProviderActiveInput>,
) -> impl IntoResponse {
    if !VALID_CAPABILITIES.contains(&input.capability.as_str()) {
        return (StatusCode::BAD_REQUEST, Json(json!({
            "error": format!("invalid capability '{}', must be one of: {}", input.capability, VALID_CAPABILITIES.join(", "))
        }))).into_response();
    }
    match providers::set_provider_active(
        &infra.db,
        &input.capability,
        input.provider_name.as_deref(),
    )
    .await
    {
        Ok(row) => {
            if MEDIA_CAPABILITIES.contains(&input.capability.as_str()) {
                notify_toolgate_reload(cfg.config.toolgate_url.clone());
            }
            (StatusCode::OK, Json(json!(row))).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
    }
}

// ── Toolgate config export (internal, unmasked keys) ────────────────────────

/// Internal endpoint for toolgate — returns full config with real `api_keys`.
/// Emits `"driver"` field (mapped from `provider_type`) which toolgate matches on.
/// Build media config JSON — used by API handler and main.rs toolgate export.
pub(crate) async fn build_media_config(infra: &InfraServices, auth: &AuthServices) -> Value {
    // Collect all media-type providers
    let mut all_providers = Vec::new();
    for media_type in &["stt", "tts", "vision", "imagegen", "embedding"] {
        if let Ok(rows) = providers::list_providers_by_type(&infra.db, media_type).await {
            all_providers.extend(rows);
        }
    }

    let active_rows = providers::list_provider_active(&infra.db).await.unwrap_or_default();

    let mut provider_map = serde_json::Map::new();
    for p in &all_providers {
        if !p.enabled {
            continue;
        }
        let api_key = resolve_key(&auth.secrets, p).await;
        provider_map.insert(
            p.name.clone(),
            json!({
                "type":     p.category,
                "driver":   p.provider_type,
                "base_url": p.base_url,
                "model":    p.default_model,
                "api_key":  api_key,
                "enabled":  p.enabled,
                "options":  p.options,
            }),
        );
    }

    let mut active_map = serde_json::Map::new();
    for a in active_rows {
        // Only include media capabilities
        if MEDIA_CAPABILITIES.contains(&a.capability.as_str()) {
            active_map.insert(a.capability, json!(a.provider_name));
        }
    }

    json!({
        "version": 1,
        "active": active_map,
        "providers": provider_map,
    })
}

pub(crate) async fn api_media_config_export(
    State(infra): State<InfraServices>,
    State(auth): State<AuthServices>,
) -> Json<Value> {
    Json(build_media_config(&infra, &auth).await)
}

// ── Static metadata ─────────────────────────────────────────────────────────

/// Single source of truth for media driver metadata.
/// Embedded at compile time from config/media-drivers.yaml so the binary
/// stays self-contained while the YAML remains human-editable in the repo.
/// Python toolgate reads the same file (or a derived constant) — see registry.py.
const MEDIA_DRIVERS_YAML: &str = include_str!("../../../../../config/media-drivers.yaml");

static MEDIA_DRIVERS_JSON: LazyLock<Value> = LazyLock::new(|| {
    let parsed: serde_yaml::Value = serde_yaml::from_str(MEDIA_DRIVERS_YAML)
        .expect("config/media-drivers.yaml: invalid YAML (compile-time embedded)");
    serde_json::to_value(&parsed)
        .expect("config/media-drivers.yaml: cannot convert to JSON")
});

pub(crate) async fn api_list_media_drivers() -> Json<Value> {
    Json(MEDIA_DRIVERS_JSON.clone())
}

pub(crate) async fn api_list_provider_types() -> Json<Value> {
    let types: Vec<Value> = crate::agent::providers::PROVIDER_TYPES
        .iter()
        .map(|pt| {
            json!({
                "id": pt.id,
                "name": pt.name,
                "default_base_url": pt.default_base_url,
                "chat_path": pt.chat_path,
                "default_secret_name": pt.default_secret_name,
                "requires_api_key": pt.requires_api_key,
                "supports_model_listing": pt.supports_model_listing,
            })
        })
        .collect();
    Json(json!({ "provider_types": types }))
}

// ── Vault migration ─────────────────────────────────────────────────────────

/// One-time startup migration: copy provider API keys from legacy vault patterns
/// (`LLM_CREDENTIALS::{uuid`} and `MEDIA_CREDENTIALS::{name`}) into the new
/// `PROVIDER_CREDENTIALS::{uuid`} pattern.
/// Idempotent — providers already migrated are skipped.
pub async fn migrate_provider_keys_to_vault(db: &PgPool, secrets: &SecretsManager) {
    let all_providers = match providers::list_providers(db).await {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(error = %e, "migrate_provider_keys: failed to list providers");
            return;
        }
    };

    let mut migrated = 0u32;
    for p in all_providers {
        let scope = p.id.to_string();

        // Already migrated?
        if secrets.get_scoped(PROVIDER_CREDENTIALS, &scope).await.is_some() {
            continue;
        }

        // Try legacy LLM vault key: LLM_CREDENTIALS scoped by UUID
        if let Some(key) = secrets.get_scoped(crate::agent::providers::LLM_CREDENTIALS, &scope).await {
            let desc = format!("Credentials for provider '{}' (migrated from LLM_CREDENTIALS)", p.name);
            if let Err(e) = secrets.set_scoped(PROVIDER_CREDENTIALS, &scope, &key, Some(&desc)).await {
                tracing::error!(provider = %p.name, error = %e, "migrate_provider_keys: vault write failed");
            } else {
                migrated += 1;
                tracing::info!(provider = %p.name, "migrate_provider_keys: migrated from LLM_CREDENTIALS");
            }
            continue;
        }

        // Try legacy media vault key: MEDIA_CREDENTIALS scoped by name
        const LEGACY_MEDIA_CREDENTIALS: &str = "MEDIA_CREDENTIALS";
        if let Some(key) = secrets.get_scoped(LEGACY_MEDIA_CREDENTIALS, &p.name).await {
            let desc = format!("Credentials for provider '{}' (migrated from MEDIA_CREDENTIALS)", p.name);
            if let Err(e) = secrets.set_scoped(PROVIDER_CREDENTIALS, &scope, &key, Some(&desc)).await {
                tracing::error!(provider = %p.name, error = %e, "migrate_provider_keys: vault write failed");
            } else {
                migrated += 1;
                tracing::info!(provider = %p.name, "migrate_provider_keys: migrated from MEDIA_CREDENTIALS");
            }
            continue;
        }
    }

    if migrated > 0 {
        tracing::info!(count = migrated, "migrate_provider_keys: complete");
    }
}

// ── CLI health-check ───────────────────────────────────────────────────────

/// Response from the CLI provider health-check endpoint.
#[derive(serde::Serialize, Clone)]
struct CliTestResult {
    cli_found: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    cli_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cli_version: Option<String>,
    auth_ok: bool,
    response_ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_time_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl CliTestResult {
    fn not_found(error: String) -> Self {
        Self { cli_found: false, cli_path: None, cli_version: None, auth_ok: false, response_ok: false, response_time_ms: None, error: Some(error) }
    }

    fn no_key(cli_path: String, cli_version: Option<String>) -> Self {
        Self { cli_found: true, cli_path: Some(cli_path), cli_version, auth_ok: false, response_ok: false, response_time_ms: None, error: Some("No API key configured. Add key in Provider settings.".into()) }
    }
}

/// Install hints for CLI presets.
fn install_hint(preset_id: &str) -> &'static str {
    match preset_id {
        "gemini-cli" => "npm install -g @google/gemini-cli",
        "claude-cli" => "npm install -g @anthropic-ai/claude-code",
        "codex-cli" => "npm install -g @openai/codex",
        _ => "see provider documentation",
    }
}

/// Allowed option keys for PATCH CLI options endpoint.
const ALLOWED_CLI_OPTION_KEYS: &[&str] = &["command", "args", "prompt_arg", "model_arg", "env_key"];

/// Validate that only allowed keys are present in the CLI options object.
fn validate_cli_option_keys(options: &Value) -> Result<(), String> {
    if let Some(obj) = options.as_object() {
        let unknown: Vec<&String> = obj.keys()
            .filter(|k| !ALLOWED_CLI_OPTION_KEYS.contains(&k.as_str()))
            .collect();
        if !unknown.is_empty() {
            return Err(format!(
                "unknown option keys: {}. Allowed: {}",
                unknown.iter().map(|k| k.as_str()).collect::<Vec<_>>().join(", "),
                ALLOWED_CLI_OPTION_KEYS.join(", ")
            ));
        }
        Ok(())
    } else {
        Err("options must be a JSON object".into())
    }
}

/// Reusable CLI health-check logic — validates CLI installation, API key, and runs a test prompt.
/// Used by both `api_test_cli` and `api_patch_cli_options`.
async fn run_cli_health_check(
    provider: &ProviderRow,
    secrets: &SecretsManager,
) -> CliTestResult {
    use std::process::Stdio;
    use std::time::Instant;

    // Validate CLI type
    let preset = match crate::agent::cli_backend::find_preset(&provider.provider_type) {
        Some(p) => p,
        None => return CliTestResult::not_found("Not a CLI provider".into()),
    };

    // Resolve config with DB overrides
    let config = match crate::agent::cli_backend::resolve_cli_config(&provider.provider_type, &provider.options) {
        Some(c) => c,
        None => return CliTestResult::not_found("Failed to resolve CLI config".into()),
    };

    // Step 1: which/where — check if CLI is installed
    #[cfg(target_os = "windows")]
    let which_cmd = "where.exe";
    #[cfg(not(target_os = "windows"))]
    let which_cmd = "which";

    let which_result = match tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tokio::process::Command::new(which_cmd)
            .arg(&config.command)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output(),
    ).await {
        Ok(Ok(output)) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        }
        _ => {
            return CliTestResult::not_found(format!("CLI not installed. Install: {}", install_hint(preset.id)));
        }
    };

    let cli_path = which_result;

    // Step 2: version
    let cli_version = match tokio::time::timeout(
        std::time::Duration::from_secs(10),
        tokio::process::Command::new(&config.command)
            .arg("--version")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output(),
    ).await {
        Ok(Ok(output)) if output.status.success() => {
            let raw = String::from_utf8_lossy(&output.stdout);
            raw.lines().next().map(|l| l.trim().to_string())
        }
        _ => None,
    };

    // Step 3: resolve API key
    let api_key = match resolve_key(secrets, provider).await {
        Some(k) => k,
        None => {
            // Fallback: check global secret under preset env_key
            match secrets.get_scoped(preset.env_key, "").await {
                Some(k) => k,
                None => {
                    return CliTestResult::no_key(cli_path, cli_version);
                }
            }
        }
    };

    // Step 4: test run
    let mut cmd = tokio::process::Command::new(&config.command);

    // Base args
    for arg in &config.args {
        cmd.arg(arg);
    }

    // Model arg
    if let Some(ref model_arg) = config.model_arg {
        let model = provider.default_model.as_deref()
            .or_else(|| preset.default_models.first().copied())
            .unwrap_or("default");
        cmd.arg(model_arg);
        cmd.arg(model);
    }

    // Prompt arg
    if let Some(ref prompt_arg) = config.prompt_arg {
        cmd.arg(prompt_arg);
        cmd.arg("say hi");
    } else {
        cmd.arg("say hi");
    }

    // Environment: inject API key
    cmd.env(preset.env_key, &api_key);

    // Clear env vars (security)
    for key in &config.clear_env {
        cmd.env_remove(key);
    }

    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let start = Instant::now();

    let output = match tokio::time::timeout(
        std::time::Duration::from_secs(30),
        cmd.output(),
    ).await {
        Ok(Ok(output)) => output,
        Ok(Err(e)) => {
            let elapsed = start.elapsed().as_millis() as u64;
            return CliTestResult {
                cli_found: true,
                cli_path: Some(cli_path),
                cli_version,
                auth_ok: true,
                response_ok: false,
                response_time_ms: Some(elapsed),
                error: Some(format!("CLI failed to start: {e}")),
            };
        }
        Err(_) => {
            let elapsed = start.elapsed().as_millis() as u64;
            return CliTestResult {
                cli_found: true,
                cli_path: Some(cli_path),
                cli_version,
                auth_ok: true,
                response_ok: false,
                response_time_ms: Some(elapsed),
                error: Some("CLI timed out after 30s".into()),
            };
        }
    };

    let elapsed = start.elapsed().as_millis() as u64;

    // Step 5: parse result
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_lowercase();
        let auth_keywords = ["401", "403", "unauthorized", "invalid key", "authentication", "invalid api key", "api key"];
        let is_auth_error = auth_keywords.iter().any(|kw| stderr.contains(kw));

        if is_auth_error {
            return CliTestResult {
                cli_found: true,
                cli_path: Some(cli_path),
                cli_version,
                auth_ok: false,
                response_ok: false,
                response_time_ms: Some(elapsed),
                error: Some("API key rejected".into()),
            };
        }

        let code = output.status.code().map_or("unknown".to_string(), |c| c.to_string());
        return CliTestResult {
            cli_found: true,
            cli_path: Some(cli_path),
            cli_version,
            auth_ok: true,
            response_ok: false,
            response_time_ms: Some(elapsed),
            error: Some(format!("CLI exited with code {code}")),
        };
    }

    // Exit code 0 — try to parse JSON
    let stdout = String::from_utf8_lossy(&output.stdout);
    match serde_json::from_str::<Value>(&stdout) {
        Ok(_) => CliTestResult {
            cli_found: true,
            cli_path: Some(cli_path),
            cli_version,
            auth_ok: true,
            response_ok: true,
            response_time_ms: Some(elapsed),
            error: None,
        },
        Err(_) => CliTestResult {
            cli_found: true,
            cli_path: Some(cli_path),
            cli_version,
            auth_ok: true,
            response_ok: false,
            response_time_ms: Some(elapsed),
            error: Some("CLI output is not valid JSON".into()),
        },
    }
}

/// `POST /api/providers/{id}/test-cli`
///
/// Validates CLI installation, API key, and runs a test prompt.
pub(crate) async fn api_test_cli(
    State(infra): State<InfraServices>,
    State(auth): State<AuthServices>,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    // Load provider
    let provider = match providers::get_provider(&infra.db, id).await {
        Ok(Some(p)) => p,
        Ok(None) => return (StatusCode::NOT_FOUND, Json(json!({"error": "provider not found"}))).into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
    };

    // Validate CLI type
    if crate::agent::cli_backend::find_preset(&provider.provider_type).is_none() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "Not a CLI provider"}))).into_response();
    }

    let result = run_cli_health_check(&provider, &auth.secrets).await;
    (StatusCode::OK, Json(serde_json::to_value(result).unwrap_or_default())).into_response()
}

// ── PATCH CLI options ──────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub(crate) struct PatchCliOptionsBody {
    pub options: Value,
}

/// `PATCH /api/providers/{id}`
///
/// Updates CLI-specific options (command, args, `prompt_arg`, `model_arg`, `env_key`)
/// with validation: command override is checked via which/where.exe.
/// After successful update, runs a health-check and returns the result.
pub(crate) async fn api_patch_cli_options(
    State(infra): State<InfraServices>,
    State(auth): State<AuthServices>,
    Path(id): Path<Uuid>,
    Json(body): Json<PatchCliOptionsBody>,
) -> impl IntoResponse {
    use std::process::Stdio;

    // Load provider
    let provider = match providers::get_provider(&infra.db, id).await {
        Ok(Some(p)) => p,
        Ok(None) => return (StatusCode::NOT_FOUND, Json(json!({"error": "not found"}))).into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
    };

    // Validate it's a CLI provider
    if crate::agent::cli_backend::find_preset(&provider.provider_type).is_none() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "Not a CLI provider"}))).into_response();
    }

    // Validate only allowed keys
    if let Err(msg) = validate_cli_option_keys(&body.options) {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": msg}))).into_response();
    }

    // If command override is present, validate it exists on system
    if let Some(cmd_val) = body.options.get("command").and_then(|v| v.as_str()) {
        #[cfg(target_os = "windows")]
        let which_cmd = "where.exe";
        #[cfg(not(target_os = "windows"))]
        let which_cmd = "which";

        let found = match tokio::time::timeout(
            std::time::Duration::from_secs(5),
            tokio::process::Command::new(which_cmd)
                .arg(cmd_val)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output(),
        ).await {
            Ok(Ok(output)) => output.status.success(),
            _ => false,
        };

        if !found {
            return (StatusCode::BAD_REQUEST, Json(json!({
                "error": format!("command '{}' not found on system", cmd_val)
            }))).into_response();
        }
    }

    // Merge new options into existing provider.options (shallow merge)
    let merged_options = {
        let mut existing = provider.options.as_object().cloned().unwrap_or_default();
        if let Some(new_obj) = body.options.as_object() {
            for (k, v) in new_obj {
                existing.insert(k.clone(), v.clone());
            }
        }
        Value::Object(existing)
    };

    // Update DB with merged options
    let input = UpdateProvider {
        name: None,
        category: None,
        provider_type: None,
        base_url: None,
        default_model: None,
        enabled: None,
        options: Some(merged_options),
        notes: None,
    };

    match providers::update_provider(&infra.db, id, input).await {
        Ok(Some(updated)) => {
            // Run health-check on the updated provider
            let health_check = run_cli_health_check(&updated, &auth.secrets).await;
            let provider_json = provider_json(&auth.secrets, &updated).await;
            (StatusCode::OK, Json(json!({
                "provider": provider_json,
                "health_check": health_check,
            }))).into_response()
        }
        Ok(None) => (StatusCode::NOT_FOUND, Json(json!({"error": "not found"}))).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_types_complete() {
        assert!(VALID_TYPES.contains(&"text"));
        assert!(VALID_TYPES.contains(&"stt"));
        assert!(VALID_TYPES.contains(&"embedding"));
        assert!(!VALID_TYPES.contains(&"audio"));
    }

    #[test]
    fn valid_capabilities_complete() {
        assert!(VALID_CAPABILITIES.contains(&"stt"));
        assert!(VALID_CAPABILITIES.contains(&"embedding"));
        assert!(!VALID_CAPABILITIES.contains(&"graph_extraction"));
        assert!(!VALID_CAPABILITIES.contains(&"compaction"));
        assert!(!VALID_CAPABILITIES.contains(&"text"));
    }

    #[test]
    fn media_drivers_yaml_parses_with_expected_capabilities() {
        // Forces LazyLock initialization — would panic on bad YAML.
        let drivers = MEDIA_DRIVERS_JSON.get("drivers").expect("drivers root key");
        for cap in ["stt", "vision", "tts", "imagegen", "embedding"] {
            let arr = drivers.get(cap).unwrap_or_else(|| panic!("missing capability {cap}"));
            let list = arr.as_array().expect("capability must be array");
            assert!(!list.is_empty(), "capability {cap} has no drivers");
            for entry in list {
                assert!(entry.get("driver").and_then(|v| v.as_str()).is_some(),
                    "missing 'driver' string in {cap} entry: {entry}");
                assert!(entry.get("label").and_then(|v| v.as_str()).is_some(),
                    "missing 'label' string in {cap} entry: {entry}");
                assert!(entry.get("requires_key").and_then(|v| v.as_bool()).is_some(),
                    "missing 'requires_key' bool in {cap} entry: {entry}");
            }
        }
    }

    #[test]
    fn validate_tts_options_accepts_missing_field() {
        // options without normalize_provider_id is fine
        let opts = serde_json::json!({"voice": "nova"});
        let res = validate_tts_options_opts_only(&opts);
        assert!(res.is_ok(), "missing field should be ok, got {:?}", res);
    }

    #[test]
    fn validate_tts_options_rejects_invalid_uuid() {
        let opts = serde_json::json!({"normalize_provider_id": "not-a-uuid"});
        let res = validate_tts_options_opts_only(&opts);
        assert!(res.is_err());
        assert!(res.unwrap_err().contains("uuid"), "expected uuid error");
    }

    #[test]
    fn validate_tts_options_accepts_valid_uuid() {
        let opts = serde_json::json!({"normalize_provider_id": "00000000-0000-0000-0000-000000000001"});
        let res = validate_tts_options_opts_only(&opts);
        assert!(res.is_ok(), "valid uuid should parse ok, got {:?}", res);
    }

    #[test]
    fn provider_active_row_serializes() {
        let row = crate::db::providers::ProviderActiveRow {
            capability: "stt".into(),
            provider_name: Some("whisper-local".into()),
        };
        let json = serde_json::to_value(&row).unwrap();
        assert_eq!(json["capability"], "stt");
        assert_eq!(json["provider_name"], "whisper-local");
    }

    #[test]
    fn create_provider_deserializes() {
        let json = serde_json::json!({
            "name": "my-provider",
            "type": "text",
            "provider_type": "openai",
            "default_model": "gpt-4o"
        });
        let input: crate::db::providers::CreateProvider = serde_json::from_value(json).unwrap();
        assert_eq!(input.category, "text");
        assert_eq!(input.provider_type, "openai");
    }

    // ── CLI option key validation ─────────────────────────────────────────

    #[test]
    fn validate_cli_options_valid_keys() {
        let opts = serde_json::json!({
            "args": ["--output-format", "json"],
            "prompt_arg": "-p"
        });
        assert!(validate_cli_option_keys(&opts).is_ok());
    }

    #[test]
    fn validate_cli_options_all_allowed_keys() {
        let opts = serde_json::json!({
            "command": "/usr/bin/gemini",
            "args": ["--json"],
            "prompt_arg": "-p",
            "model_arg": "--model",
            "env_key": "GEMINI_API_KEY"
        });
        assert!(validate_cli_option_keys(&opts).is_ok());
    }

    #[test]
    fn validate_cli_options_unknown_key() {
        let opts = serde_json::json!({
            "args": ["--json"],
            "sneaky_field": "bad"
        });
        let err = validate_cli_option_keys(&opts).unwrap_err();
        assert!(err.contains("sneaky_field"), "error should mention the unknown key: {}", err);
    }

    #[test]
    fn validate_cli_options_not_object() {
        let opts = serde_json::json!("not an object");
        let err = validate_cli_option_keys(&opts).unwrap_err();
        assert!(err.contains("must be a JSON object"));
    }

    #[test]
    fn validate_cli_options_empty_object() {
        let opts = serde_json::json!({});
        assert!(validate_cli_option_keys(&opts).is_ok());
    }

    #[test]
    fn patch_cli_options_body_deserializes() {
        let json = serde_json::json!({
            "options": {
                "args": ["--output-format", "json"],
                "prompt_arg": "-p"
            }
        });
        let body: PatchCliOptionsBody = serde_json::from_value(json).unwrap();
        assert!(body.options.is_object());
        assert!(body.options.get("args").is_some());
    }

    fn is_valid_type(t: &str) -> bool { VALID_TYPES.contains(&t) }
    fn is_valid_capability(c: &str) -> bool { VALID_CAPABILITIES.contains(&c) }

    #[test]
    fn type_validation() {
        assert!(is_valid_type("text"));
        assert!(is_valid_type("embedding"));
        assert!(!is_valid_type(""));
        assert!(!is_valid_type("TEXT"));
    }

    #[test]
    fn capability_validation() {
        assert!(is_valid_capability("stt"));
        assert!(is_valid_capability("embedding"));
        assert!(!is_valid_capability("graph_extraction"));
        assert!(!is_valid_capability("compaction"));
        assert!(!is_valid_capability("text"));
        assert!(!is_valid_capability(""));
    }
}
