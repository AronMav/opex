//! `GET /api/commands` — exposes the merged slash-command registry
//! (builtin plus live toolgate-handler commands, see
//! `crate::agent::commands::merge`) over HTTP so the UI and channel
//! adapters can render autocomplete / native command menus.
//!
//! Phase 2a: per-lang handler descriptions + `fse.allowlist` gating for
//! builtin-tier handlers; `version` is the `HandlerRegistry` ETag (falls
//! back to a manifest-count tag when toolgate never sent one) — see
//! task-3-brief.md.

use axum::{Json, Router, extract::{Query, State}, response::IntoResponse, routing::get};
use serde::Deserialize;

use crate::agent::commands::spec::CommandScope;
use crate::gateway::state::AppState;

#[derive(Deserialize)]
struct CommandsQuery {
    // Accepted for UI/channel-adapter forward-compat; per-agent visibility
    // filtering (base vs. non-base) is not yet wired to a caller identity here.
    #[allow(dead_code)]
    agent: Option<String>,
    lang: Option<String>,
    scope: Option<String>,
}

async fn list_commands(State(state): State<AppState>, Query(q): Query<CommandsQuery>) -> impl IntoResponse {
    let lang = q.lang.as_deref().unwrap_or("en");
    let db = &state.infra.db;
    state.handlers.refresh().await;
    let manifests = state.handlers.manifests().await;
    let enabled = crate::agent::fse::get_enabled_allowlist(db).await;
    let registry = crate::agent::commands::merge::build_registry(&manifests, &enabled, lang);
    let mut specs = registry.visible_for(false);
    if q.scope.as_deref() == Some("native") {
        specs.retain(|c| matches!(c.scope, CommandScope::Native | CommandScope::Both));
    }
    let version = state
        .handlers
        .etag()
        .await
        .unwrap_or_else(|| specs.len().to_string()); // F8: ETag when available, else manifest-count fallback
    Json(serde_json::json!({ "commands": specs, "version": version }))
}

pub(crate) fn routes() -> Router<AppState> {
    Router::new().route("/api/commands", get(list_commands))
}

#[cfg(test)]
mod tests {
    use crate::agent::commands::merge::build_registry;
    use crate::agent::handler_registry::HandlerManifest;
    use serde_json::json;

    #[test]
    fn merged_registry_serializes_builtin_plus_handler() {
        let m: HandlerManifest = serde_json::from_value(json!({
            "id":"summarize_video","execution":"async","tier":"workspace",
            "descriptions":{"en":"Summarize a video"},"config":[]}))
        .unwrap();
        let reg = build_registry(&[m], &[], "en");
        let json = serde_json::to_value(reg.all()).unwrap();
        let names: Vec<&str> = json
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"status")); // builtin
        assert!(names.contains(&"summarize_video")); // handler
    }
}
