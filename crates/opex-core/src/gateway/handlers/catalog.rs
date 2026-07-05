//! Read-only API over the model catalog (models.dev/OpenRouter/…).
//!
//! `GET /api/catalog/providers` — provider presets for the "add provider" flow
//! (hundreds of providers with pre-filled base_url / env / model list). Powers
//! adding providers OPEX doesn't ship natively: most are OpenAI-compatible, so
//! the UI creates them as `openai_compat` with the catalog's `api` base_url.

use axum::{Router, response::Json, routing::get};

use crate::gateway::AppState;

pub(crate) fn routes() -> Router<AppState> {
    Router::new().route("/api/catalog/providers", get(list_providers))
}

async fn list_providers() -> Json<serde_json::Value> {
    let providers = opex_catalog::global_providers();
    Json(serde_json::json!({
        "count": providers.len(),
        "providers": providers,
    }))
}
