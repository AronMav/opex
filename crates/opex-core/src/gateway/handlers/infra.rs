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
        .route("/api/infra/decisions/{id}/resolve", post(api_resolve_decision))
        .route("/api/infra/decisions/{id}", axum::routing::patch(api_patch_decision))
}

#[derive(Debug, Deserialize)]
struct InfraEventBody {
    docker_name: String,
    status: String,
}

/// Собирает диагноз-затравку для изолированной сессии Opex. Решение `decision_id`
/// уже создано (pending) и владелец уже уведомлён кнопками — задача Opex ДОПОЛНИТЬ
/// его диагнозом/командами через PATCH либо самому резолвить (safe→done, штатно→
/// dismissed). Владелец получит осмысленный вопрос независимо от того, отработает
/// ли LLM — базовый pending уже виден.
fn build_diagnostic_seed(docker_name: &str, status: &str, decision_id: Uuid) -> String {
    format!(
        "[Infra] Watchdog обнаружил проблемный контейнер `{docker_name}` в состоянии \
`{status}` (держится ≥2 циклов). По нему УЖЕ создано pending-решение `{decision_id}`, \
владелец уведомлён кнопками. Используй скилл infra-triage: продиагностируй \
(docker inspect, сверка с compose/провайдерами/портом) и обнови ЭТО решение через \
`PATCH /api/infra/decisions/{decision_id}`: safe (нужный упавший сервис) → сам \
`docker restart` + status `done`; штатно/не требуется → status `dismissed`; нужно \
удаление/правка compose → передай уточнённый diagnosis + proposed_commands, оставь \
pending (владелец одобрит кнопкой). НЕ создавай новое решение и НЕ пиши владельцу \
текстом — работай через PATCH этого id."
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
    State(bus): State<ChannelBus>,
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

    // Создаём pending-решение СРАЗУ (базовое содержимое от watchdog) + уведомляем
    // владельца кнопками. Так владелец получает осмысленный вопрос независимо от
    // того, отработает ли LLM-сессия Opex — не полагаемся на adherence модели.
    // Этот pending также служит anchor'ом анти-петли (has_recent покрывает pending).
    let diagnosis = format!("Watchdog: контейнер в состоянии `{}`", body.status);
    let proposed_action =
        "Проверить и починить или удалить (Opex уточнит; можно решить сразу)".to_string();
    let decision_id = match crate::db::infra_decisions::create(
        &infra.db,
        &body.docker_name,
        &diagnosis,
        &proposed_action,
        &json!([]),
        "pending",
        INFRA_TTL_DAYS,
    )
    .await
    {
        Ok(id) => id,
        Err(e) => {
            // UNIQUE-нарушение = уже есть pending по контейнеру (гонка с debounce) →
            // не плодим и не спавним второй раз.
            tracing::warn!(error = %e, "infra-event: pending уже существует, skip");
            return Json(json!({ "skipped": true, "reason": "pending exists" }));
        }
    };
    notify_owner_of_pending(&infra, &bus, &engine, decision_id, &body.docker_name, &proposed_action)
        .await;

    let agent_name = engine.cfg().agent.name.clone();
    let seed = build_diagnostic_seed(&body.docker_name, &body.status, decision_id);
    spawn_infra_session(engine, agent_name, seed);
    Json(json!({ "spawned": true, "decision_id": decision_id.to_string() }))
}

/// UI-notification (колокольчик) + Telegram inline-кнопки владельцу для pending-решения.
/// Общий путь для `api_infra_event` (авто-создание) и `api_create_decision` (Opex).
async fn notify_owner_of_pending(
    infra: &InfraServices,
    bus: &ChannelBus,
    engine: &AgentEngine,
    decision_id: Uuid,
    container: &str,
    proposed_action: &str,
) {
    crate::gateway::handlers::notifications::notify(
        &infra.db,
        &bus.ui_event_tx,
        "infra_decision",
        "Требуется решение по инфраструктуре",
        &format!("Контейнер {container}: {proposed_action}"),
        json!({
            "decision_id": decision_id.to_string(),
            "container": container,
            "proposed_action": proposed_action,
        }),
    )
    .await
    .ok();
    deliver_infra_buttons(&infra.db, engine, decision_id, container, proposed_action).await;
}

// ── Decisions API (Task 5) ──────────────────────────────────────────────────

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
#[derive(Debug, Deserialize)]
struct PatchBody {
    /// done | failed | dismissed. Опционально — можно обновить только содержимое.
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    diagnosis: Option<String>,
    #[serde(default)]
    proposed_action: Option<String>,
    #[serde(default)]
    proposed_commands: Option<serde_json::Value>,
}

/// PATCH /api/infra/decisions/{id} — Opex дополняет авто-созданный pending
/// (diagnosis/proposed_action/proposed_commands, статус остаётся pending) ЛИБО
/// резолвит его сам (`done` после restart, `dismissed` если действий не нужно,
/// `failed` при сбое исполнения). Owner-resolve (approve/reject) идёт отдельным
/// путём через `/resolve`.
async fn api_patch_decision(
    State(infra): State<InfraServices>,
    Path(id): Path<Uuid>,
    Json(body): Json<PatchBody>,
) -> impl IntoResponse {
    // 1. Обновить содержимое, если переданы поля (COALESCE — только не-None).
    let has_content =
        body.diagnosis.is_some() || body.proposed_action.is_some() || body.proposed_commands.is_some();
    if has_content
        && let Err(e) = crate::db::infra_decisions::update_content(
            &infra.db,
            id,
            body.diagnosis.as_deref(),
            body.proposed_action.as_deref(),
            body.proposed_commands.as_ref(),
        )
        .await
    {
        return (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response();
    }

    // 2. Перевести статус, если передан (терминальные для Opex).
    if let Some(status) = body.status.as_deref() {
        if !matches!(status, "done" | "failed" | "dismissed") {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                Json(json!({"error": "status must be done|failed|dismissed"})),
            )
                .into_response();
        }
        if let Err(e) = crate::db::infra_decisions::mark_status(&infra.db, id, status).await {
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": e.to_string()})),
            )
                .into_response();
        }
    }

    Json(json!({"ok": true})).into_response()
}

/// Собирает затравку для изолированной сессии Opex, выполняющей одобренное решение.
fn build_execute_seed(d: &InfraDecision) -> String {
    let cmds_empty = d
        .proposed_commands
        .as_array()
        .is_none_or(|a| a.is_empty());
    let action_line = if cmds_empty {
        "Конкретные команды не зафиксированы — продиагностируй контейнер `{container}` \
и выполни необходимое по своему суждению (restart нужного сервиса, либо `docker rm` \
осиротевшего + правка compose, если требуется)."
            .replace("{container}", &d.container)
    } else {
        format!("Выполни зафиксированные шаги: {}.", d.proposed_commands)
    };
    format!(
        "[Infra] Владелец одобрил решение {id} по контейнеру `{container}`: {action}. \
{action_line} Если правишь серверный docker-compose.yml — предупреди владельца, что \
git-версию надо синхронизировать. По завершении вызови \
PATCH /api/infra/decisions/{id} со статусом done или failed и кратко сообщи итог.",
        id = d.id,
        container = d.container,
        action = d.proposed_action,
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
    fn seed_mentions_container_skill_and_decision() {
        let did = uuid::Uuid::new_v4();
        let s = build_diagnostic_seed("docker-tts-silero-1", "Created", did);
        assert!(s.contains("docker-tts-silero-1"));
        assert!(s.contains("Created"));
        assert!(s.contains("infra-triage"));
        assert!(s.contains(&did.to_string()), "seed должен нести decision_id для PATCH");
        assert!(s.contains("PATCH"));
    }

    #[test]
    fn execute_seed_empty_commands_asks_diagnose() {
        let mut d = sample_decision();
        d.proposed_commands = serde_json::json!([]);
        let s = build_execute_seed(&d);
        assert!(s.contains(&d.id.to_string()));
        assert!(s.contains(&d.container));
        assert!(s.contains("продиагностируй"), "при пустых командах Opex должен разобраться сам");
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
