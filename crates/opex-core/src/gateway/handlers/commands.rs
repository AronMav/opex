//! `GET /api/commands` — exposes the in-core slash-command registry
//! (`crate::agent::commands::COMMAND_REGISTRY`) over HTTP so the UI and
//! channel adapters can render autocomplete / native command menus.
//!
//! Phase 1: no per-agent filtering (all builtin commands are
//! `Visibility::All`), no ETag-based versioning — see task-5-brief.md.

use axum::{Json, Router, extract::Query, response::IntoResponse, routing::get};
use serde::Deserialize;

use crate::agent::commands::spec::CommandScope;
use crate::gateway::state::AppState;

#[derive(Deserialize)]
struct CommandsQuery {
    // Accepted for UI/channel-adapter forward-compat; per-agent + language
    // filtering land in Phase 2 (all Phase-1 commands are `Visibility::All`).
    #[allow(dead_code)]
    agent: Option<String>,
    #[allow(dead_code)]
    lang: Option<String>,
    scope: Option<String>,
}

async fn list_commands(Query(q): Query<CommandsQuery>) -> impl IntoResponse {
    let reg = &*crate::agent::commands::COMMAND_REGISTRY;
    // Phase 1: every command is `Visibility::All` → `visible_for(false)` == all.
    let mut specs = reg.visible_for(false);
    if q.scope.as_deref() == Some("native") {
        specs.retain(|c| matches!(c.scope, CommandScope::Native | CommandScope::Both));
    }
    let version = specs.len().to_string(); // F8: simple version-tag; ETag versioning is Phase 2
    Json(serde_json::json!({ "commands": specs, "version": version }))
}

pub(crate) fn routes() -> Router<AppState> {
    Router::new().route("/api/commands", get(list_commands))
}

#[cfg(test)]
mod tests {
    #[test]
    fn commands_serialize_to_json_array() {
        let reg = &*crate::agent::commands::COMMAND_REGISTRY;
        let json = serde_json::to_value(reg.all()).unwrap();
        assert!(json.as_array().unwrap().len() >= 14);
        assert!(json[0].get("name").is_some());
    }
}
