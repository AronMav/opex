use std::sync::Arc;

use axum::{Json, Router, extract::State, response::IntoResponse, routing::post};
use serde::Deserialize;
use serde_json::json;

use crate::agent::engine::AgentEngine;
use crate::gateway::clusters::{AgentCore, InfraServices};
use crate::gateway::state::AppState;

const INFRA_COOLDOWN_HOURS: i64 = 24;

pub(crate) fn routes() -> Router<AppState> {
    Router::new().route("/api/internal/infra-event", post(api_infra_event))
}

#[derive(Debug, Deserialize)]
struct InfraEventBody {
    docker_name: String,
    status: String,
}

/// Собирает диагноз-затравку для изолированной сессии Opex.
fn build_infra_seed(docker_name: &str, status: &str) -> String {
    format!(
        "[Infra] Watchdog обнаружил проблемный контейнер `{docker_name}` в состоянии \
`{status}` (держится ≥2 циклов). Используй скилл infra-triage: продиагностируй и, \
если безопасно — почини сам; иначе создай infra-решение с вопросом владельцу. \
По итогу ОБЯЗАТЕЛЬНО оставь ровно одну запись в infra_decisions (pending | done | \
dismissed) — молчаливого завершения быть не должно."
    )
}

/// Fire-and-forget запуск изолированной сессии base-агента.
fn spawn_infra_session(engine: Arc<AgentEngine>, agent_name: String, seed: String) {
    tokio::spawn(async move {
        let msg = opex_types::IncomingMessage {
            user_id: "system".to_string(),
            text: Some(seed),
            attachments: vec![],
            agent_id: agent_name,
            channel: crate::agent::channel_kind::channel::SYSTEM.to_string(),
            context: serde_json::json!({}),
            timestamp: chrono::Utc::now(),
            formatting_prompt: None,
            tool_policy_override: None,
            leaf_message_id: None,
            user_message_id: None,
        };
        if let Err(e) = engine.handle_isolated_via_pipeline(&msg).await {
            tracing::warn!(error = %e, "infra self-heal session failed");
        }
    });
}

/// POST /api/internal/infra-event — watchdog reports a persistently unhealthy
/// docker container. Debounced via `infra_decisions::has_recent`; spawns an
/// isolated base-agent session (fire-and-forget) to triage/fix/escalate.
/// Loopback-only (see `LOOPBACK_EXACT` in `middleware.rs`) — watchdog calls
/// without an owner bearer token.
async fn api_infra_event(
    State(infra): State<InfraServices>,
    State(agents): State<AgentCore>,
    Json(body): Json<InfraEventBody>,
) -> impl IntoResponse {
    // Ленивый TTL-expiry.
    let _ = crate::db::infra_decisions::expire_stale(&infra.db).await;

    // Дебаунс: недавняя запись по контейнеру → skip.
    match crate::db::infra_decisions::has_recent(&infra.db, &body.docker_name, INFRA_COOLDOWN_HOURS).await {
        Ok(true) => return Json(json!({ "skipped": true, "reason": "recent decision" })),
        Ok(false) => {}
        Err(e) => {
            tracing::warn!(error = %e, "infra debounce query failed");
            return Json(json!({ "skipped": true, "reason": "db error" }));
        }
    }

    let Some(engine) = agents.base_engine().await else {
        tracing::warn!("infra-event: no base agent to respond");
        return Json(json!({ "skipped": true, "reason": "no base agent" }));
    };
    let agent_name = engine.cfg().agent.name.clone();
    let seed = build_infra_seed(&body.docker_name, &body.status);
    spawn_infra_session(engine, agent_name, seed);
    Json(json!({ "spawned": true }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_mentions_container_and_skill() {
        let s = build_infra_seed("docker-tts-silero-1", "Created");
        assert!(s.contains("docker-tts-silero-1"));
        assert!(s.contains("Created"));
        assert!(s.contains("infra-triage"));
    }
}
