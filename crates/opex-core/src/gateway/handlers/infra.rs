use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, State},
    response::IntoResponse,
    routing::post,
};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use crate::agent::channel_actions::ChannelAction;
use crate::agent::engine::AgentEngine;
use crate::agent::initiative::delivery::resolve_owner_target;
use crate::db::infra_decisions::InfraDecision;
use crate::gateway::clusters::{AgentCore, ChannelBus, InfraServices};
use crate::gateway::state::AppState;

const INFRA_COOLDOWN_HOURS: i64 = 24;
const INFRA_TTL_DAYS: i64 = 7;

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/internal/infra-event", post(api_infra_event))
        .route(
            "/api/infra/decisions",
            post(api_create_decision).get(api_list_decisions),
        )
        .route("/api/infra/decisions/{id}/resolve", post(api_resolve_decision))
        .route("/api/infra/decisions/{id}", axum::routing::patch(api_patch_decision))
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

// ── Decisions API (Task 5) ──────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct CreateDecisionBody {
    container: String,
    diagnosis: String,
    #[serde(default)]
    proposed_action: String,
    #[serde(default)]
    proposed_commands: serde_json::Value,
    /// pending | done | dismissed — итог диагностики Opex.
    status: String,
}

/// POST /api/infra/decisions — Opex создаёт решение (вопрос владельцу либо
/// молчаливый итог done/dismissed). Для `pending` — UI-уведомление +
/// Telegram inline-кнопки владельцу.
async fn api_create_decision(
    State(infra): State<InfraServices>,
    State(bus): State<ChannelBus>,
    State(agents): State<AgentCore>,
    Json(body): Json<CreateDecisionBody>,
) -> impl IntoResponse {
    let cmds = if body.proposed_commands.is_null() {
        json!([])
    } else {
        body.proposed_commands.clone()
    };
    let id = match crate::db::infra_decisions::create(
        &infra.db,
        &body.container,
        &body.diagnosis,
        &body.proposed_action,
        &cmds,
        &body.status,
        INFRA_TTL_DAYS,
    )
    .await
    {
        Ok(id) => id,
        Err(e) => {
            // UNIQUE-нарушение (уже есть pending) трактуем как «принято» — не ошибка сервера.
            tracing::warn!(error = %e, "create infra decision failed (возможно уже есть pending)");
            return (
                axum::http::StatusCode::CONFLICT,
                Json(json!({"ok": false, "error": e.to_string()})),
            )
                .into_response();
        }
    };

    // Уведомляем владельца ТОЛЬКО для pending (вопрос). done/dismissed — молча.
    if body.status == "pending" {
        // (а) UI-notification (колокольчик, chat_id-независимо).
        crate::gateway::handlers::notifications::notify(
            &infra.db,
            &bus.ui_event_tx,
            "infra_decision",
            "Требуется решение по инфраструктуре",
            &format!("Контейнер {}: {}", body.container, body.proposed_action),
            json!({
                "decision_id": id.to_string(),
                "container": body.container,
                "proposed_action": body.proposed_action,
            }),
        )
        .await
        .ok();

        // (б) Telegram inline-кнопки владельцу — переиспускаем паттерн initiative.
        if let Some(engine) = agents.base_engine().await {
            deliver_infra_buttons(&infra.db, &engine, id, &body.container, &body.proposed_action)
                .await;
        }
    }
    (axum::http::StatusCode::OK, Json(json!({"ok": true, "id": id.to_string()}))).into_response()
}

/// Отправляет владельцу inline-кнопки «Выполнить/Отклонить» в его DM-канал.
/// `channel_router` живёт per-agent (`AgentEngine::state().channel_router`,
/// НЕ в `AppState`/кластерах) — тот же паттерн, что и весь остальной
/// код доставки (см. `channel_ws/handshake.rs`, `initiative/tick.rs`).
/// `owner_id` берётся ТОЛЬКО из конфига base-агента (SECURITY, как в initiative H1).
async fn deliver_infra_buttons(
    db: &sqlx::PgPool,
    engine: &AgentEngine,
    decision_id: Uuid,
    container: &str,
    proposed_action: &str,
) {
    let Some(router) = engine.channel_router_ref() else { return };
    let agent_name = engine.cfg().agent.name.clone();
    let owner_id = engine.cfg().agent.access.as_ref().and_then(|a| a.owner_id.clone());
    let Some((channel, chat_id)) = resolve_owner_target(db, &agent_name, owner_id.as_deref()).await
    else {
        return;
    };

    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    let action = ChannelAction {
        name: "infra_decision".to_string(),
        params: json!({
            "decision_id": decision_id.to_string(),
            "container": container,
            "proposed_action": proposed_action,
        }),
        context: json!({ "chat_id": chat_id }),
        reply: reply_tx,
        target_channel: Some(channel),
    };
    if router.send(action).await.is_ok() {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), reply_rx).await;
    }
}

/// GET /api/infra/decisions — список последних решений (для UI-страницы).
async fn api_list_decisions(State(infra): State<InfraServices>) -> impl IntoResponse {
    match crate::db::infra_decisions::list(&infra.db, 100).await {
        Ok(rows) => Json(json!({"decisions": rows})).into_response(),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

#[derive(Debug, Deserialize)]
struct PatchBody {
    status: String,
}

/// PATCH /api/infra/decisions/{id} — Opex отмечает исполнение одобренного
/// действия (done | failed). Не транзакционно (`mark_status`), вызывается
/// самим Opex по итогу выполнения.
async fn api_patch_decision(
    State(infra): State<InfraServices>,
    Path(id): Path<Uuid>,
    Json(body): Json<PatchBody>,
) -> impl IntoResponse {
    // Только терминальные статусы исполнения.
    if !matches!(body.status.as_str(), "done" | "failed") {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(json!({"error": "status must be done|failed"})),
        )
            .into_response();
    }
    match crate::db::infra_decisions::mark_status(&infra.db, id, &body.status).await {
        Ok(()) => Json(json!({"ok": true})).into_response(),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

/// Собирает затравку для изолированной сессии Opex, выполняющей одобренное решение.
fn build_execute_seed(d: &InfraDecision) -> String {
    format!(
        "[Infra] Владелец одобрил решение {id}: {action}. Выполни зафиксированные шаги: \
{cmds}. По завершении вызови PATCH /api/infra/decisions/{id} со статусом done или \
failed и кратко сообщи итог.",
        id = d.id,
        action = d.proposed_action,
        cmds = d.proposed_commands,
    )
}

/// Единая точка подтверждения решения (UI-`/resolve` и Telegram-callback
/// (Task 6) сводятся сюда). Идемпотентна через `resolve_strict` — повторный
/// вызов на уже обработанном решении возвращает `AlreadyResolved`, а не
/// молча перезапускает Opex второй раз.
pub(crate) async fn resolve_infra_decision(
    infra: &InfraServices,
    agents: &AgentCore,
    id: Uuid,
    approved: bool,
    resolved_by: &str,
) -> Result<(), crate::db::infra_decisions::InfraError> {
    let status = if approved { "approved" } else { "rejected" };
    let decision = crate::db::infra_decisions::resolve_strict(&infra.db, id, status, resolved_by).await?;
    if approved
        && let Some(engine) = agents.base_engine().await
    {
        let agent_name = engine.cfg().agent.name.clone();
        let seed = build_execute_seed(&decision);
        spawn_infra_session(engine, agent_name, seed);
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct ResolveBody {
    approved: bool,
}

/// POST /api/infra/decisions/{id}/resolve — владелец подтверждает/отклоняет.
/// Owner-guard: этот роут не входит в `PUBLIC_*`/`LOOPBACK_*` (см.
/// `middleware.rs`), поэтому auth-middleware уже требует Bearer-токен
/// владельца — единственный токен в системе. Более строгая привязка к
/// конкретному owner_id не нужна в v1; `AuthServices` подключён для
/// будущей многопользовательности.
async fn api_resolve_decision(
    State(infra): State<InfraServices>,
    State(agents): State<AgentCore>,
    State(_auth): State<crate::gateway::clusters::AuthServices>,
    Path(id): Path<Uuid>,
    Json(body): Json<ResolveBody>,
) -> impl IntoResponse {
    match resolve_infra_decision(&infra, &agents, id, body.approved, "owner").await {
        Ok(()) => Json(json!({"ok": true})).into_response(),
        Err(crate::db::infra_decisions::InfraError::AlreadyResolved { status, .. }) => (
            axum::http::StatusCode::CONFLICT,
            Json(json!({"ok": false, "error": format!("уже обработано: {status}")})),
        )
            .into_response(),
        Err(crate::db::infra_decisions::InfraError::NotFound { .. }) => (
            axum::http::StatusCode::NOT_FOUND,
            Json(json!({"ok": false, "error": "not found"})),
        )
            .into_response(),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "error": e.to_string()})),
        )
            .into_response(),
    }
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

    fn sample_decision() -> InfraDecision {
        InfraDecision {
            id: uuid::Uuid::new_v4(),
            container: "docker-tts-silero-1".into(),
            diagnosis: "orphan".into(),
            proposed_action: "remove + edit compose".into(),
            proposed_commands: serde_json::json!(["docker rm docker-tts-silero-1"]),
            status: "approved".into(),
            created_at: chrono::Utc::now(),
            resolved_at: None,
            resolved_by: Some("owner".into()),
            expires_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn execute_seed_carries_commands_and_id() {
        let d = sample_decision();
        let s = build_execute_seed(&d);
        assert!(s.contains(&d.id.to_string()));
        assert!(s.contains("docker rm"));
        assert!(s.contains("PATCH"));
    }
}
