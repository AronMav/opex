# Этап C — Фаза 2A «Harden инициативы» — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Достроить задеплоенную gated-инициативу: Telegram approve/dismiss/cancel-кнопки (owner-гейт), доставка предложений и результатов целей в канал владельца, durable re-drive инициативных целей, отмена одобренной цели, sqlx-проверка атомарности.

**Architecture:** Резолвер `owner_id→(channel,chat_id)` (из конфига агента, не из запроса). `initiative_tick` шлёт предложение в Telegram через `channel_router` (протянут в `InitiativeDeps`). TS-адаптер рендерит inline-кнопки. Callback owner-гейтится и зовёт ОБЩИЕ функции approve/dismiss/cancel (те же, что web). Re-drive расширяется на `origin='initiative'` с резолвом target из конфига.

**Tech Stack:** Rust 2024 (sqlx, tokio, axum 0.8), TypeScript/Bun (grammy — `channels/`), Next.js (ui/). Переиспользуются `handle_approval_callback`, `ChannelAction`/`channel_router`, `list_redrivable`/`resume_autonomous_goals`, goal-driver.

## Global Constraints

- **rustls-tls only, никакого OpenSSL.**
- **H1 (безопасность):** `owner_id` для `resolve_owner_target` и `GoalTarget` ВСЕГДА из `engine.cfg().agent.access.owner_id` / `AccessGuard.owner_id` (сервер-сайд). НИКОГДА из callback_data / тела запроса / `msg.user_id`.
- **`chat_id` едет в `ChannelAction.context` (не `params`)** — `telegram.ts::executeAction` читает `context.chat_id` до switch, иначе early-return.
- **Гейты (base-refuse) — ВНУТРИ общих функций** `approve_proposal`/`dismiss_proposal`/`cancel_goal`, не в HTTP-обёртке.
- **approve атомарен** (флип статуса + create_session + upsert_goal в одной транзакции); spawn после коммита.
- **Re-drive только `status='active'`**; `cancelled`/`done`/`paused` не воскрешаются; капы `max_retries`+backoff сохранены.
- **Fail-soft:** резолвер/доставка/callback — провал не рушит инициативу/сессию.
- **Тесты opex-core в bin-таргете** (`cargo test --bin opex-core`); Windows их не гоняет — implementer верифицирует `cargo check --all-targets -p opex-core` + `cargo clippy -p opex-core --all-targets -- -D warnings`; sqlx/unit гоняются на сервере. TS: `cd channels && bun test`; UI: `cd ui && npm run build`.
- Никаких Co-Authored-By; работа в master.

---

### Task 1: Резолвер `resolve_owner_target` (delivery.rs)

**Files:**
- Create: `crates/opex-core/src/agent/initiative/delivery.rs`
- Modify: `crates/opex-core/src/agent/initiative/mod.rs` (`pub mod delivery;`)

**Interfaces:**
- Produces: `pub async fn resolve_owner_target(db: &sqlx::PgPool, agent_name: &str, owner_id: Option<&str>) -> Option<(String, i64)>` — `(channel_type, chat_id)`; `None` если нет owner_id / канала / owner_id не число.

- [ ] **Step 1: Написать провальный тест (парс owner_id)**

Чистая часть — парс chat_id (DB-часть тестируется sqlx на сервере). В `delivery.rs`:

```rust
#[cfg(test)]
mod tests {
    #[test]
    fn chat_id_parses_only_numeric_owner() {
        assert_eq!(super::parse_chat_id(Some("388443751")), Some(388443751));
        assert_eq!(super::parse_chat_id(Some("not-a-number")), None);
        assert_eq!(super::parse_chat_id(None), None);
        assert_eq!(super::parse_chat_id(Some("")), None);
    }
}
```

- [ ] **Step 2: Запустить — FAIL**

Run: `cargo test --bin opex-core -- delivery::tests`
Expected: FAIL (нет модуля/функции).

- [ ] **Step 3: Реализовать**

```rust
//! Stage C phase 2A: resolve the owner's messaging channel + chat_id for
//! delivering initiative proposals and goal results.
//! SECURITY (H1): owner_id is passed in by the caller, who MUST source it from
//! agent config (engine.cfg().agent.access.owner_id), never from a request.
use sqlx::PgPool;

pub(crate) fn parse_chat_id(owner_id: Option<&str>) -> Option<i64> {
    owner_id?.trim().parse::<i64>().ok()
}

/// Resolve `(channel_type, chat_id)` for the agent's owner. `None` (→ web-only
/// delivery) when owner_id is absent/non-numeric or the agent has no running
/// telegram channel.
pub async fn resolve_owner_target(
    db: &PgPool,
    agent_name: &str,
    owner_id: Option<&str>,
) -> Option<(String, i64)> {
    let chat_id = parse_chat_id(owner_id)?;
    let channel_type: Option<String> = sqlx::query_scalar(
        "SELECT channel_type FROM agent_channels
         WHERE agent_name = $1 AND channel_type = 'telegram' AND status = 'running'
         ORDER BY created_at LIMIT 1",
    )
    .bind(agent_name)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();
    channel_type.map(|ch| (ch, chat_id))
}
```

Добавить `pub mod delivery;` в `crates/opex-core/src/agent/initiative/mod.rs`.

- [ ] **Step 4: Проверка**

Run: `cargo test --bin opex-core -- delivery::tests && cargo check --all-targets -p opex-core`
Expected: PASS + 0 ошибок (возможны dead_code на `resolve_owner_target` — свяжут Task 3/4/7).

- [ ] **Step 5: Commit**

```bash
git add crates/opex-core/src/agent/initiative/delivery.rs crates/opex-core/src/agent/initiative/mod.rs
git commit -m "feat(initiative): resolve_owner_target (agent channel + owner chat_id)"
```

---

### Task 2: `channel_router` в `InitiativeDeps` + finalize

**Files:**
- Modify: `crates/opex-core/src/agent/initiative/tick.rs` (`InitiativeDeps` struct ~26-34)
- Modify: `crates/opex-core/src/agent/pipeline/finalize.rs` (`finalize_context_from_engine` InitiativeDeps-литерал + тест-конструкторы)

**Interfaces:**
- Produces: поле `pub channel_router: Option<crate::agent::channel_actions::ChannelActionRouter>` на `InitiativeDeps`.

- [ ] **Step 1: Добавить поле в `InitiativeDeps`**

В `crates/opex-core/src/agent/initiative/tick.rs` (`#[derive(Clone)] pub struct InitiativeDeps`), после `ui_event_tx`:

```rust
    pub channel_router: Option<crate::agent::channel_actions::ChannelActionRouter>,
```

- [ ] **Step 2: Заполнить в finalize_context_from_engine**

В `crates/opex-core/src/agent/pipeline/finalize.rs`, в литерале `InitiativeDeps { ... }` внутри `finalize_context_from_engine` (рядом с `ui_event_tx: engine.state().ui_event_tx.clone()`), добавить:

```rust
                channel_router: engine.state().channel_router.clone(),
```
(`ChannelActionRouter` — `#[derive(Clone)]`, `channel_actions.rs:31`; `AgentState.channel_router` — `agent_state.rs:61`.)

- [ ] **Step 3: Починить тест-конструкторы**

Найти в `finalize.rs` (и где-либо ещё) все литералы `InitiativeDeps { ... }` в тестах (`grep -rn "InitiativeDeps {" crates/opex-core/src`); в каждый добавить `channel_router: None,`.

- [ ] **Step 4: Проверка**

Run: `cargo check --all-targets -p opex-core`
Expected: 0 ошибок.

- [ ] **Step 5: Commit**

```bash
git add crates/opex-core/src/agent/initiative/tick.rs crates/opex-core/src/agent/pipeline/finalize.rs
git commit -m "feat(initiative): thread channel_router into InitiativeDeps"
```

---

### Task 3: Доставка предложения в Telegram (tick.rs)

**Files:**
- Modify: `crates/opex-core/src/agent/initiative/tick.rs` (после `try_add_proposal` в `initiative_tick_inner`)
- Modify: `crates/opex-core/src/agent/initiative/delivery.rs` (хелпер сборки ChannelAction)

**Interfaces:**
- Consumes: `resolve_owner_target` (Task 1), `deps.channel_router`/`deps.owner_id` (Task 2), `Proposal` (`db::agent_plans`).
- Produces: `pub async fn send_proposal_to_channel(router, channel, chat_id, proposal_id, text, rationale)` в delivery.rs.

**ВАЖНО (wire-контракт для Task 9):** `ChannelAction{ name:"initiative_proposal", params:{proposal_id, text, rationale}, context:{chat_id}, target_channel:Some(channel) }`. `chat_id` — В `context`.

- [ ] **Step 1: Реализовать отправитель в delivery.rs**

```rust
use crate::agent::channel_actions::{ChannelAction, ChannelActionRouter};

/// Fire-and-forget: send an initiative proposal to the owner's channel with
/// inline approve/dismiss buttons. Mirrors approval_manager's send pattern
/// (throwaway reply oneshot + 5s timeout + ignore). Fail-soft.
pub async fn send_proposal_to_channel(
    router: &ChannelActionRouter,
    channel: &str,
    chat_id: i64,
    proposal_id: uuid::Uuid,
    text: &str,
    rationale: &str,
) {
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    let action = ChannelAction {
        name: "initiative_proposal".to_string(),
        params: serde_json::json!({
            "proposal_id": proposal_id.to_string(),
            "text": text,
            "rationale": rationale,
        }),
        context: serde_json::json!({ "chat_id": chat_id }),
        reply: reply_tx,
        target_channel: Some(channel.to_string()),
    };
    if router.send(action).await.is_ok() {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), reply_rx).await;
    }
}
```

*(Свериться с точной сигнатурой `ChannelActionRouter::send` и полями `ChannelAction` в `agent/channel_actions.rs:8` — `reply` обязателен, non-Option.)*

- [ ] **Step 2: Вызвать в initiative_tick_inner**

В `tick.rs`, в блоке `if added { ... }` (после web-`notify(...)`), добавить доставку в канал:

```rust
        if added {
            // web bell (existing notify) ... then channel delivery (fail-soft):
            if let (Some(router), Some((ch, chat_id))) = (
                deps.channel_router.as_ref(),
                crate::agent::initiative::delivery::resolve_owner_target(
                    db, agent_name, deps.owner_id.as_deref(),
                ).await,
            ) {
                crate::agent::initiative::delivery::send_proposal_to_channel(
                    router, &ch, chat_id, proposal.id, clean_goal,
                    &proposal_gen.rationale, // sanitize applied below or reuse clean_rationale
                ).await;
            }
        }
```
*(Разместить рядом с существующим `notify`-блоком; переиспользовать уже вычисленный `clean_rationale` (санитизированный) вместо сырого — свериться с текущим кодом блока `if added`.)*

- [ ] **Step 3: Проверка**

Run: `cargo check --all-targets -p opex-core && cargo clippy -p opex-core --all-targets -- -D warnings`
Expected: 0 ошибок / 0 warnings.

- [ ] **Step 4: Commit**

```bash
git add crates/opex-core/src/agent/initiative/delivery.rs crates/opex-core/src/agent/initiative/tick.rs
git commit -m "feat(initiative): deliver proposal to owner telegram channel (chat_id in context)"
```

---

### Task 4: Общие функции approve/dismiss/cancel

**Files:**
- Modify: `crates/opex-core/src/gateway/handlers/agents/initiative.rs` (вынести общие функции; web-хендлеры зовут их)

**Interfaces:**
- Produces:
  - `pub(crate) enum ProposalError { BaseAgent, Db(String) }`
  - `pub(crate) struct ApproveOutcome { pub spawned: bool, pub session_id: Option<uuid::Uuid> }`
  - `pub(crate) async fn approve_proposal(db: &PgPool, engine: &Arc<AgentEngine>, proposal_id: Uuid) -> Result<ApproveOutcome, ProposalError>`
  - `pub(crate) async fn dismiss_proposal(db: &PgPool, engine: &Arc<AgentEngine>, proposal_id: Uuid) -> Result<bool, ProposalError>`
  - `pub(crate) async fn cancel_goal(db: &PgPool, engine: &Arc<AgentEngine>, session_id: Uuid) -> Result<bool, ProposalError>`

- [ ] **Step 1: Реализовать общие функции**

Вынести тело текущего `api_approve_proposal` в `approve_proposal`. Гейты ВНУТРИ. `agent_name = engine.cfg().agent.name.clone()`. Атомарная транзакция:

```rust
pub(crate) enum ProposalError { BaseAgent, Db(String) }
pub(crate) struct ApproveOutcome { pub spawned: bool, pub session_id: Option<uuid::Uuid> }

pub(crate) async fn approve_proposal(
    db: &PgPool, engine: &Arc<AgentEngine>, proposal_id: Uuid,
) -> Result<ApproveOutcome, ProposalError> {
    if engine.cfg().agent.base { return Err(ProposalError::BaseAgent); }
    let agent_name = engine.cfg().agent.name.clone();
    // atomic flip pending→approved
    let proposal = crate::db::agent_plans::try_set_proposal_status(db, &agent_name, proposal_id, "approved")
        .await.map_err(|e| ProposalError::Db(e.to_string()))?;
    let Some(proposal) = proposal else {
        return Ok(ApproveOutcome { spawned: false, session_id: None }); // not pending → idempotent
    };
    // transaction: create session + upsert initiative goal (no "approved without goal")
    let mut tx = db.begin().await.map_err(|e| ProposalError::Db(e.to_string()))?;
    const INITIATIVE_GOAL_MAX_TURNS: i32 = 20;
    let channel = crate::agent::channel_kind::channel::CRON;
    let session_id = crate::db::sessions::create_new_session_tx(&mut tx, &agent_name, "system", channel)
        .await.map_err(|e| ProposalError::Db(e.to_string()))?;
    crate::db::session_goals::upsert_initiative_goal_tx(&mut tx, session_id, &proposal.text, INITIATIVE_GOAL_MAX_TURNS)
        .await.map_err(|e| ProposalError::Db(e.to_string()))?;
    tx.commit().await.map_err(|e| ProposalError::Db(e.to_string()))?;
    // GoalTarget from agent config owner_id (H1), never request
    let owner = engine.cfg().agent.access.as_ref().and_then(|a| a.owner_id.clone());
    let target = crate::agent::initiative::delivery::resolve_owner_target(db, &agent_name, owner.as_deref()).await;
    if let Some(pool) = engine.cfg().goal_pool.clone() {
        let handle = crate::agent::goal::driver::spawn_goal_driver(engine.clone(), session_id, target);
        pool.insert(session_id, handle);
    }
    Ok(ApproveOutcome { spawned: true, session_id: Some(session_id) })
}
```

`dismiss_proposal` — вынести из `api_dismiss_proposal` (гейты внутри; `try_set_proposal_status(...,"dismissed")`). `cancel_goal` — `session_goals::set_status(session_id,"cancelled")` + `goal::pool::stop(&pool, session_id)`.

*(Если `create_new_session`/`upsert_initiative_goal` не имеют `_tx`-вариантов, принимающих `&mut Transaction`, добавить их по образцу существующих (или обернуть через `Executor`-дженерик). Свериться с сигнатурами в `db/sessions.rs`/`db/session_goals.rs`.)*

- [ ] **Step 2: Web-хендлеры зовут общие функции**

Переписать `api_approve_proposal`/`api_dismiss_proposal` (`initiative.rs`) — резолв `engine` через `app.agents.get_engine(&name)` (404 если нет) + `validate_agent_name`, затем `approve_proposal(&app.infra.db, &engine, id)`, маппинг `ProposalError::BaseAgent→403`, `Db→500`, `Ok→json`.

- [ ] **Step 3: Проверка**

Run: `cargo check --all-targets -p opex-core && cargo clippy -p opex-core --all-targets -- -D warnings`
Expected: 0/0.

- [ ] **Step 4: Commit**

```bash
git add crates/opex-core/src/gateway/handlers/agents/initiative.rs crates/opex-core/src/db/
git commit -m "feat(initiative): shared approve/dismiss/cancel fns (gates inside, atomic tx, GoalTarget from config)"
```

---

### Task 5: Web cancel-роут + UI-кнопка

**Files:**
- Modify: `crates/opex-core/src/gateway/handlers/agents/initiative.rs` (роут + хендлер)
- Modify: `ui/src/app/(authenticated)/agents/plan/page.tsx` + API-клиент (`ui/src/lib/*`)

**Interfaces:**
- Consumes: `cancel_goal` (Task 4).

- [ ] **Step 1: Web cancel-роут**

В `initiative.rs` `routes()` добавить `.route("/api/agents/{name}/plan/goals/{session_id}/cancel", post(api_cancel_goal))`; хендлер: `validate_agent_name` + `get_engine` (404) → `cancel_goal(&app.infra.db, &engine, session_id)` → `{ok, cancelled}`.

- [ ] **Step 2: UI-кнопка**

В `agents/plan/page.tsx` у каждой active initiative-цели (`active_goals`) добавить кнопку «Отменить» (shadcn Button, design-токены, без raw values) → API-клиент `cancelGoal(name, sessionId)` (POST) + инвалидация `qk.agentPlan`. Требует `session_id` в ответе `/plan` `active_goals` — добавить его в GET plan (`api_get_plan`: `"session_id": g.session_id`).

- [ ] **Step 3: Проверка**

Run: `cargo check --all-targets -p opex-core` + `cd ui && npm run build`
Expected: 0 ошибок; build ok.

- [ ] **Step 4: Commit**

```bash
git add crates/opex-core/src/gateway/handlers/agents/initiative.rs ui/src/
git commit -m "feat(initiative): cancel active initiative goal (web route + UI button)"
```

---

### Task 6: Telegram callback-intercept

**Files:**
- Modify: `crates/opex-core/src/gateway/handlers/channel_ws/inline.rs` (`handle_initiative_callback`)
- Modify: `crates/opex-core/src/gateway/handlers/channel_ws/reader.rs` (intercept + wire-guard тест)

**Interfaces:**
- Consumes: `approve_proposal`/`dismiss_proposal`/`cancel_goal` (Task 4).

- [ ] **Step 1: `handle_initiative_callback`**

Зеркало `handle_approval_callback` (`inline.rs:171`): парс `iappr:{uuid}`/`idismiss:{uuid}`/`icancel:{uuid}`; owner-гейт `live_guard.is_owner(&user_id)` (fail-closed); `agent_name` из соединения (параметр), НЕ из payload. Зовёт общую функцию (Task 4) через `engine` (уже в параметрах, как у approval). Ответ `Done` «✅ Одобрено»/«❌ Отклонено»/«⏹ Отменено». Возврат `true` (consumed).

```rust
pub(super) async fn handle_initiative_callback(
    ctx: &CwsCtx, engine: &Arc<AgentEngine>, agent_name: &str,
    request_id: &str, msg: &IncomingMessageDto, out_tx: &mpsc::Sender<OutboundMsg>,
) -> bool {
    // is_callback guard (as handle_approval_callback) ...
    let text = msg.text.as_deref().unwrap_or("");
    let (kind, id_str) = if let Some(s) = text.strip_prefix("iappr:") { ("approve", s) }
        else if let Some(s) = text.strip_prefix("idismiss:") { ("dismiss", s) }
        else if let Some(s) = text.strip_prefix("icancel:") { ("cancel", s) }
        else { return false };
    let user_id = msg.user_id.clone();
    let live_guard = ctx.auth.access_guards.read().await.get(agent_name).cloned();
    if !live_guard.as_ref().is_some_and(|g| g.is_owner(&user_id)) { /* error-frame + return true */ }
    let Ok(id) = id_str.parse::<uuid::Uuid>() else { return true };
    let db = &ctx.infra.db;
    let (ok_text, ..) = match kind {
        "approve" => (crate::gateway::handlers::agents::initiative::approve_proposal(db, engine, id).await, "✅ Одобрено"),
        "dismiss" => (crate::gateway::handlers::agents::initiative::dismiss_proposal(db, engine, id).await.map(|_| Default::default()), "❌ Отклонено"),
        _ => (crate::gateway::handlers::agents::initiative::cancel_goal(db, engine, id).await.map(|_| Default::default()), "⏹ Отменено"),
    };
    // send Done frame with ok_text (or Error on Err) ...
    true
}
```
*(Точный тип `CwsCtx`/`IncomingMessageDto`/`OutboundMsg`/`db`-доступ — свериться с `handle_approval_callback` сигнатурой; `ctx.infra.db` vs иное. Общие функции — `pub(crate)`.)*

- [ ] **Step 2: Вплести в reader.rs**

В `reader.rs` intercept-цепочке (рядом с `handle_approval_callback`, ~115) добавить вызов `handle_initiative_callback` (после approval — разные префиксы). Consumed → `continue`.

- [ ] **Step 3: Wire-guard тест**

По образцу `approval_wired_before_clarify` (`reader.rs:225-251`, `include_str!` assert): тест что `handle_initiative_callback` вызывается в reader до `dispatch_message`.

- [ ] **Step 4: Проверка**

Run: `cargo check --all-targets -p opex-core && cargo clippy -p opex-core --all-targets -- -D warnings`
Expected: 0/0.

- [ ] **Step 5: Commit**

```bash
git add crates/opex-core/src/gateway/handlers/channel_ws/inline.rs crates/opex-core/src/gateway/handlers/channel_ws/reader.rs
git commit -m "feat(initiative): owner-gated telegram approve/dismiss/cancel callback"
```

---

### Task 7: Durable re-drive инициативных целей

**Files:**
- Modify: `crates/opex-core/src/db/session_goals.rs` (`RedrivableGoal.origin` + `list_redrivable` + тест)
- Modify: `crates/opex-core/src/main.rs` (`resume_autonomous_goals` GoalTarget + коммент)
- Create: `migrations/078_initiative_redrive_comment.sql`

**Interfaces:**
- Produces: `RedrivableGoal { session_id, agent_id, origin: String }`.

- [ ] **Step 1: `RedrivableGoal.origin` + list_redrivable**

В `db/session_goals.rs`: добавить `pub origin: String` в `RedrivableGoal`; в `list_redrivable` SELECT добавить `g.origin`, фильтр `g.origin = 'cron'` → `g.origin IN ('cron','initiative')`, tuple `(Uuid,String,String)`, маппинг с origin. Обновить doc-комментарий (scope больше не только cron).

- [ ] **Step 2: resume_autonomous_goals GoalTarget**

В `main.rs` (~985), заменить `let handle = spawn_goal_driver(engine.clone(), rg.session_id, None);` на резолв target для initiative:

```rust
                let target = if rg.origin == "initiative" {
                    let owner = engine.cfg().agent.access.as_ref().and_then(|a| a.owner_id.clone());
                    crate::agent::initiative::delivery::resolve_owner_target(&db, &rg.agent_id, owner.as_deref()).await
                } else {
                    None // cron: ui_event delivery as before
                };
                let handle = crate::agent::goal::driver::spawn_goal_driver(engine.clone(), rg.session_id, target);
```
Поправить stale-комментарий на строке ~985.

- [ ] **Step 3: Расширить sqlx-тест**

`list_redrivable_selects_only_crashed_cron_goals` (`session_goals.rs:401`) → добавить initiative-строку (active, crashed) и проверить, что она ТОЖЕ возвращается + `origin` в результате корректен; `origin='goal'` по-прежнему НЕ выбирается.

- [ ] **Step 4: Миграция 078**

`migrations/078_initiative_redrive_comment.sql`:
```sql
-- m077 said initiative is NOT re-driven; phase 2A adds durable re-drive.
COMMENT ON COLUMN session_goals.origin IS
  'goal = interactive /goal (never auto-re-driven); cron = autonomous cron run (crash re-driven); initiative = owner-approved self-initiated goal (crash re-driven since phase 2A).';
```

- [ ] **Step 5: Проверка**

Run: `cargo check --all-targets -p opex-core && cargo clippy -p opex-core --all-targets -- -D warnings`
Expected: 0/0.

- [ ] **Step 6: Commit**

```bash
git add crates/opex-core/src/db/session_goals.rs crates/opex-core/src/main.rs migrations/078_initiative_redrive_comment.sql
git commit -m "feat(initiative): durable re-drive for initiative goals (RedrivableGoal.origin + config GoalTarget)"
```

---

### Task 8: sqlx race-тесты атомарности

**Files:**
- Modify: `crates/opex-core/src/db/agent_plans.rs` (добавить `#[sqlx::test]` инфру)

- [ ] **Step 1: Написать sqlx-тесты**

```rust
#[sqlx::test(migrations = "../../migrations")]
async fn concurrent_try_add_proposal_respects_cap(pool: sqlx::PgPool) -> sqlx::Result<()> {
    get_or_create(&pool, "raceA").await.unwrap();
    let today = chrono::Utc::now().date_naive();
    let p1 = Proposal { id: uuid::Uuid::new_v4(), text: "g1".into(), status: "pending".into(), created_at: chrono::Utc::now(), acted_at: None };
    let p2 = Proposal { id: uuid::Uuid::new_v4(), text: "g2".into(), status: "pending".into(), created_at: chrono::Utc::now(), acted_at: None };
    let (r1, r2) = tokio::join!(
        try_add_proposal(&pool, "raceA", today, 1, &p1),
        try_add_proposal(&pool, "raceA", today, 1, &p2),
    );
    assert_eq!([r1.unwrap(), r2.unwrap()].iter().filter(|x| **x).count(), 1, "exactly one add under cap=1");
    let plan = get_or_create(&pool, "raceA").await.unwrap();
    assert_eq!(plan.proposals_today, 1);
    assert_eq!(plan.parsed_proposals().len(), 1);
    Ok(())
}

#[sqlx::test(migrations = "../../migrations")]
async fn concurrent_approve_spawns_once(pool: sqlx::PgPool) -> sqlx::Result<()> {
    get_or_create(&pool, "raceB").await.unwrap();
    let today = chrono::Utc::now().date_naive();
    let id = uuid::Uuid::new_v4();
    let p = Proposal { id, text: "g".into(), status: "pending".into(), created_at: chrono::Utc::now(), acted_at: None };
    try_add_proposal(&pool, "raceB", today, 1, &p).await.unwrap();
    let (a, b) = tokio::join!(
        try_set_proposal_status(&pool, "raceB", id, "approved"),
        try_set_proposal_status(&pool, "raceB", id, "approved"),
    );
    assert_eq!([a.unwrap(), b.unwrap()].iter().filter(|x| x.is_some()).count(), 1, "exactly one wins the flip");
    Ok(())
}
```
*(Свериться с точным путём `migrations` относительно crate (образец существующих `#[sqlx::test]` в session_goals.rs — там уже задан рабочий путь; повторить его).)*

- [ ] **Step 2: Проверка**

Run: `cargo check --all-targets -p opex-core` (тесты гоняются на сервере с live PG).
Expected: 0 ошибок компиляции.

- [ ] **Step 3: Commit**

```bash
git add crates/opex-core/src/db/agent_plans.rs
git commit -m "test(initiative): sqlx race tests for atomic cap + approve"
```

---

### Task 9: TS-адаптер (channels) — рендер кнопок

**Files:**
- Modify: `channels/src/drivers/telegram.ts` (`case "initiative_proposal"` в `executeAction`)
- Modify: `channels/src/localization.ts` (ключи `initiativeHeader`/`initiativeApprove`/`initiativeDismiss`, RU+EN)

**Interfaces:**
- Consumes: wire-контракт из Task 3 (`params:{proposal_id,text,rationale}`, `context:{chat_id}`).

- [ ] **Step 1: Локализация**

В `channels/src/localization.ts` в `interface Strings` добавить `initiativeHeader: string; initiativeApprove: string; initiativeDismiss: string;`; в RU-реализацию: `initiativeHeader: "💡 Предложение цели", initiativeApprove: "✅ Одобрить", initiativeDismiss: "❌ Отклонить"`; в EN аналогично.

- [ ] **Step 2: `case "initiative_proposal"`**

В `telegram.ts::executeAction`, рядом с `case "approval_request"` (~1170):

```ts
    case "initiative_proposal": {
      const proposalId = action.params.proposal_id as string;
      const text = action.params.text as string;
      const rationale = (action.params.rationale as string) ?? "";
      if (!strings) { console.error("[tg] initiative_proposal requires strings"); break; }
      const s = strings;
      const body = `${s.initiativeHeader}\n${text}${rationale ? "\n\n" + rationale : ""}`;
      const keyboard = new InlineKeyboard()
        .text(s.initiativeApprove, `iappr:${proposalId}`).row()
        .text(s.initiativeDismiss, `idismiss:${proposalId}`);
      await bot.api.sendMessage(chatId, body, {
        reply_markup: keyboard,
        reply_parameters: safeReplyParams(messageId),
      });
      break;
    }
```
(`chatId` уже извлечён из `context.chat_id` в шапке — Task 3 кладёт его туда. callback_data `iappr:{uuid}` ≤43 байт < 64.)

- [ ] **Step 3: Проверка**

Run: `cd channels && bun test && bunx tsc --noEmit` (или существующий lint/typecheck проекта)
Expected: тесты не сломаны, типы ок.

- [ ] **Step 4: Commit**

```bash
git add channels/src/drivers/telegram.ts channels/src/localization.ts
git commit -m "feat(channels): render initiative proposal with approve/dismiss inline buttons"
```

---

## Замечания по исполнению

- **Порядок/зависимости:** 1 (резолвер) и 8 (sqlx) независимы. 2→3→9 (channel_router→доставка→TS, wire-контракт фиксируется в 3 до 9). 4→5,6 (общие функции→cancel-роут/UI, TG-callback). 1→7 (резолвер→re-drive). Реком. последовательность: 1,2,3,4,5,6,7,8,9.
- **Тесты Rust — на сервере** (bin-таргет + sqlx нужен live PG). Implementer на Windows верифицирует `cargo check --all-targets -p opex-core` + `cargo clippy -p opex-core --all-targets -- -D warnings`. TS: `bun test`. UI: `npm run build`.
- **E2E (после всех, на сервере):** агент с telegram+owner_id → предложение приходит в TG с кнопками → ✅ → goal-driver стартует, результат в DM; cancel (web/TG) → цель остановлена; краш core посреди цели → рестарт → re-drive → результат владельцу; двойной approve web+TG → один goal.
- **Свериться при реализации** (помечено в задачах): `ChannelActionRouter::send` сигнатура; `create_new_session`/`upsert_initiative_goal` `_tx`-варианты (или Executor-дженерик); `CwsCtx`/`IncomingMessageDto`/`db`-доступ в inline.rs; путь `migrations` в `#[sqlx::test]`; точный блок `if added` в tick.rs (переиспользовать `clean_rationale`).
