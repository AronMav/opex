# Этап C — Фаза 2A «Harden инициативы» — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Достроить gated-инициативу: Telegram approve/dismiss/cancel (owner-гейт), доставка предложений и результатов целей в канал владельца, durable re-drive инициативных целей, отмена одобренной цели, sqlx-проверка атомарности.

**Architecture:** Резолвер `owner_id→(channel,chat_id)` (из конфига агента). `initiative_tick` шлёт предложение в Telegram через `channel_router` (протянут в `InitiativeDeps`). TS-адаптер рендерит inline-кнопки. Callback owner-гейтится и зовёт ОБЩИЕ функции approve/dismiss/cancel (те же, что web); approve атомарен (одна tx). Re-drive расширяется на `origin='initiative'` с target из конфига.

**Tech Stack:** Rust 2024 (sqlx, tokio, axum 0.8; крейты `opex-core` + `opex-db`), TypeScript/Bun (`channels/`), Next.js (`ui/`).

## Global Constraints

- **rustls-tls only.**
- **H1:** `owner_id` для `resolve_owner_target`/`GoalTarget` ВСЕГДА из `engine.cfg().agent.access.owner_id` (сервер-сайд). НИКОГДА из callback_data/тела/`msg.user_id`.
- **`chat_id` в `ChannelAction.context`** (не `params`) — `telegram.ts` читает `context.chat_id` до switch.
- **Гейты (base-refuse) ВНУТРИ общих функций.**
- **approve атомарен:** флип статуса + create_session + upsert_goal в ОДНОЙ sqlx-транзакции (все три через `_tx`-варианты); spawn после commit.
- **Re-drive только `status='active'`**; `cancelled`/`done`/`paused` не воскрешаются; капы сохранены.
- **Fail-soft:** резолвер/доставка/callback не рушат инициативу.
- **Тесты:** opex-core в bin-таргете (Windows НЕ гоняет). Implementer верифицирует `cargo check --all-targets -p opex-core` + `cargo clippy -p opex-core --all-targets -- -D warnings`; sqlx/unit — на сервере. TS: `cd channels && bun test`; UI: `cd ui && npm run build`.
- Никаких Co-Authored-By; работа в master.

---

### Task 1: Резолвер `resolve_owner_target` (delivery.rs)

**Files:** Create `crates/opex-core/src/agent/initiative/delivery.rs`; Modify `crates/opex-core/src/agent/initiative/mod.rs` (`pub mod delivery;`)

**Interfaces — Produces:** `pub async fn resolve_owner_target(db: &PgPool, agent_name: &str, owner_id: Option<&str>) -> Option<(String,i64)>`; `pub(crate) fn parse_chat_id(owner_id: Option<&str>) -> Option<i64>`.

- [ ] **Step 1: Провальный тест**

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

- [ ] **Step 2: FAIL** — `cargo test --bin opex-core -- delivery::tests` → нет модуля.

- [ ] **Step 3: Реализовать**

```rust
//! Stage C phase 2A: resolve owner's channel + chat_id for delivering initiative
//! proposals and goal results. SECURITY (H1): the caller MUST pass owner_id
//! sourced from agent config (engine.cfg().agent.access.owner_id), never a request.
use sqlx::PgPool;

pub(crate) fn parse_chat_id(owner_id: Option<&str>) -> Option<i64> {
    owner_id?.trim().parse::<i64>().ok()
}

pub async fn resolve_owner_target(db: &PgPool, agent_name: &str, owner_id: Option<&str>) -> Option<(String, i64)> {
    let chat_id = parse_chat_id(owner_id)?;
    let ch: Option<String> = sqlx::query_scalar(
        "SELECT channel_type FROM agent_channels
         WHERE agent_name = $1 AND channel_type = 'telegram' AND status = 'running'
         ORDER BY created_at LIMIT 1",
    ).bind(agent_name).fetch_optional(db).await.ok().flatten();
    ch.map(|c| (c, chat_id))
}
```
Добавить `pub mod delivery;` в `agent/initiative/mod.rs`.

- [ ] **Step 4: Проверка** — `cargo test --bin opex-core -- delivery::tests && cargo check --all-targets -p opex-core` → PASS + 0 (dead_code на resolve_owner_target ок — свяжут T2/T3/T7).

- [ ] **Step 5: Commit** — `git add crates/opex-core/src/agent/initiative/{delivery.rs,mod.rs}; git commit -m "feat(initiative): resolve_owner_target (agent channel + owner chat_id)"`

---

### Task 2: channel_router в InitiativeDeps + доставка предложения (tick.rs)

**Files:** Modify `crates/opex-core/src/agent/initiative/tick.rs`, `crates/opex-core/src/agent/pipeline/finalize.rs`, `crates/opex-core/src/agent/initiative/delivery.rs`

**Interfaces:**
- Consumes: `resolve_owner_target` (T1).
- Produces: поле `pub channel_router: Option<ChannelActionRouter>` на `InitiativeDeps`; `pub async fn send_proposal_to_channel(router, channel, chat_id, proposal_id, text, rationale)`.

**Wire-контракт (для T9):** `ChannelAction{ name:"initiative_proposal", params:{proposal_id,text,rationale}, context:{chat_id}, target_channel:Some(channel) }`.

- [ ] **Step 1: Поле в InitiativeDeps**

В `tick.rs` (`#[derive(Clone)] pub struct InitiativeDeps`), после `ui_event_tx`:
```rust
    pub channel_router: Option<crate::agent::channel_actions::ChannelActionRouter>,
```
В `finalize.rs` — ЕДИНСТВЕННЫЙ литерал `InitiativeDeps {` (`finalize_context_from_engine`, ~733), рядом с `ui_event_tx: engine.state().ui_event_tx.clone()`, добавить:
```rust
                channel_router: engine.state().channel_router.clone(),
```
*(Тест-конструкторов `InitiativeDeps` нет — grep подтвердит; только прод-литерал.)*

- [ ] **Step 2: Отправитель в delivery.rs**

```rust
use crate::agent::channel_actions::{ChannelAction, ChannelActionRouter};

pub async fn send_proposal_to_channel(
    router: &ChannelActionRouter, channel: &str, chat_id: i64,
    proposal_id: uuid::Uuid, text: &str, rationale: &str,
) {
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    let action = ChannelAction {
        name: "initiative_proposal".to_string(),
        params: serde_json::json!({ "proposal_id": proposal_id.to_string(), "text": text, "rationale": rationale }),
        context: serde_json::json!({ "chat_id": chat_id }),
        reply: reply_tx,
        target_channel: Some(channel.to_string()),
    };
    if router.send(action).await.is_ok() {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), reply_rx).await;
    }
}
```
*(`ChannelAction` поля/`router.send(action)->Result<(),String>` — `channel_actions.rs:8,60`.)*

- [ ] **Step 3: Вызвать в initiative_tick_inner + hoist rationale**

В `tick.rs`, блок `if added { ... }`: сейчас `clean_rationale` вычисляется ВНУТРИ `if let Some(tx) = &deps.ui_event_tx {…}`. **Вынести** вычисление `clean_rationale` (sanitize) ВЫШЕ, чтобы канал-доставка его тоже использовала независимо от `ui_event_tx`. Затем после web-notify:
```rust
            if let (Some(router), Some((ch, chat_id))) = (
                deps.channel_router.as_ref(),
                crate::agent::initiative::delivery::resolve_owner_target(db, agent_name, deps.owner_id.as_deref()).await,
            ) {
                crate::agent::initiative::delivery::send_proposal_to_channel(
                    router, &ch, chat_id, proposal.id, clean_goal, &clean_rationale,
                ).await;
            }
```

- [ ] **Step 4: Проверка** — `cargo check --all-targets -p opex-core && cargo clippy -p opex-core --all-targets -- -D warnings` → 0/0.

- [ ] **Step 5: Commit** — `git add crates/opex-core/src/agent/initiative/{tick.rs,delivery.rs} crates/opex-core/src/agent/pipeline/finalize.rs; git commit -m "feat(initiative): thread channel_router + deliver proposal to owner telegram"`

---

### Task 3: tx-варианты + `approve_proposal` (атомарный)

**Files:** Modify `crates/opex-db/src/sessions.rs` (`create_new_session_tx`), `crates/opex-core/src/db/session_goals.rs` (`upsert_initiative_goal_tx`), `crates/opex-core/src/db/agent_plans.rs` (`try_set_proposal_status_tx`), `crates/opex-core/src/gateway/handlers/agents/initiative.rs` (`approve_proposal` + типы)

**Interfaces — Produces:**
- `pub(crate) enum ProposalError { BaseAgent, Db(String) }`
- `pub(crate) struct ApproveOutcome { pub spawned: bool, pub session_id: Option<Uuid> }`
- `pub(crate) async fn approve_proposal(db: &PgPool, engine: &Arc<AgentEngine>, proposal_id: Uuid) -> Result<ApproveOutcome, ProposalError>`
- tx-варианты: `create_new_session_tx(&mut Transaction, ...)`, `upsert_initiative_goal_tx(&mut Transaction, ...)`, `try_set_proposal_status_tx(&mut Transaction, ...) -> Option<Proposal>`.

- [ ] **Step 1: tx-варианты БД-функций**

Добавить рядом с существующими (тела копируют оригиналы, меняя executor на `&mut *tx`):
- `crates/opex-db/src/sessions.rs`: `pub async fn create_new_session_tx(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>, agent, user_id, channel) -> Result<Uuid>` — тело как `create_new_session` (`sessions.rs:295`), но `.fetch_one(&mut **tx)`.
- `crates/opex-core/src/db/session_goals.rs`: `pub async fn upsert_initiative_goal_tx(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>, session_id, goal_text, max_turns) -> Result<()>` — как `upsert_initiative_goal` (`:110`), `.execute(&mut **tx)`.
- `crates/opex-core/src/db/agent_plans.rs`: `pub async fn try_set_proposal_status_tx(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>, agent_id, id, new_status) -> Result<Option<Proposal>>` — как `try_set_proposal_status` (`:101`), `.fetch_optional(&mut **tx)`.

*(Свериться с типом executor в существующих `.bind(...).execute(db)` — заменить `db` на `&mut **tx`.)*

- [ ] **Step 2: M3-тест (провальный) на base-refuse**

Юнит (в `initiative.rs` тестах) — сложно без engine-мока; вместо этого простой тест на маппинг ошибки + компиляция. Реальную base-refuse проверяет E2E/интеграция. Минимум — тест что `ProposalError` имеет вариант `BaseAgent` и что web-хендлер маппит его в 403 (может быть покрыто в T4 вместе с рефактором web). Отметить: полный тест обоих путей — интеграционный на сервере.

- [ ] **Step 3: Реализовать `approve_proposal`**

```rust
pub(crate) enum ProposalError { BaseAgent, Db(String) }
pub(crate) struct ApproveOutcome { pub spawned: bool, pub session_id: Option<uuid::Uuid> }

pub(crate) async fn approve_proposal(
    db: &sqlx::PgPool, engine: &std::sync::Arc<crate::agent::engine::AgentEngine>, proposal_id: uuid::Uuid,
) -> Result<ApproveOutcome, ProposalError> {
    if engine.cfg().agent.base { return Err(ProposalError::BaseAgent); } // M3: gate inside
    let agent_name = engine.cfg().agent.name.clone();
    const INITIATIVE_GOAL_MAX_TURNS: i32 = 20;
    let channel = crate::agent::channel_kind::channel::CRON;
    // L1: flip + session + goal in ONE transaction. No "approved without goal".
    let mut tx = db.begin().await.map_err(|e| ProposalError::Db(e.to_string()))?;
    let flipped = crate::db::agent_plans::try_set_proposal_status_tx(&mut tx, &agent_name, proposal_id, "approved")
        .await.map_err(|e| ProposalError::Db(e.to_string()))?;
    let Some(proposal) = flipped else {
        tx.rollback().await.ok();
        return Ok(ApproveOutcome { spawned: false, session_id: None }); // not pending → idempotent
    };
    let session_id = crate::db::sessions::create_new_session_tx(&mut tx, &agent_name, "system", channel)
        .await.map_err(|e| ProposalError::Db(e.to_string()))?;
    crate::db::session_goals::upsert_initiative_goal_tx(&mut tx, session_id, &proposal.text, INITIATIVE_GOAL_MAX_TURNS)
        .await.map_err(|e| ProposalError::Db(e.to_string()))?;
    tx.commit().await.map_err(|e| ProposalError::Db(e.to_string()))?;
    // GoalTarget from CONFIG owner_id (H1), spawn after commit
    let owner = engine.cfg().agent.access.as_ref().and_then(|a| a.owner_id.clone());
    let target = crate::agent::initiative::delivery::resolve_owner_target(db, &agent_name, owner.as_deref()).await;
    if let Some(pool) = engine.cfg().goal_pool.clone() {
        let handle = crate::agent::goal::driver::spawn_goal_driver(engine.clone(), session_id, target);
        pool.insert(session_id, handle);
    }
    Ok(ApproveOutcome { spawned: true, session_id: Some(session_id) })
}
```

- [ ] **Step 4: Проверка** — `cargo check --all-targets -p opex-core && cargo clippy -p opex-core --all-targets -- -D warnings` → 0/0.

- [ ] **Step 5: Commit** — `git add crates/opex-db/src/sessions.rs crates/opex-core/src/db/{session_goals.rs,agent_plans.rs} crates/opex-core/src/gateway/handlers/agents/initiative.rs; git commit -m "feat(initiative): atomic approve_proposal (tx flip+session+goal, GoalTarget from config)"`

---

### Task 4: `dismiss_proposal` + `try_cancel_goal`/`cancel_goal` + рефактор web

**Files:** Modify `crates/opex-core/src/db/session_goals.rs` (`try_cancel_goal`), `crates/opex-core/src/gateway/handlers/agents/initiative.rs` (`dismiss_proposal`, `cancel_goal`, переписать web-хендлеры)

**Interfaces — Produces:**
- `pub async fn try_cancel_goal(db: &PgPool, session_id: Uuid) -> Result<bool>` — атомарно `UPDATE session_goals SET status='cancelled' WHERE session_id=$1 AND status='active' RETURNING session_id` → `bool` (была ли active).
- `pub(crate) async fn dismiss_proposal(db, engine, proposal_id) -> Result<bool, ProposalError>`
- `pub(crate) async fn cancel_goal(db, engine, session_id) -> Result<bool, ProposalError>`

- [ ] **Step 1: `try_cancel_goal` (атомарный conditional)**

В `session_goals.rs`:
```rust
pub async fn try_cancel_goal(db: &PgPool, session_id: Uuid) -> Result<bool> {
    let row: Option<(Uuid,)> = sqlx::query_as(
        "UPDATE session_goals SET status='cancelled', updated_at=now()
         WHERE session_id=$1 AND status='active' RETURNING session_id",
    ).bind(session_id).fetch_optional(db).await?;
    Ok(row.is_some())
}
```

- [ ] **Step 2: `dismiss_proposal` + `cancel_goal`**

В `initiative.rs`:
```rust
pub(crate) async fn dismiss_proposal(db: &sqlx::PgPool, engine: &Arc<AgentEngine>, proposal_id: uuid::Uuid) -> Result<bool, ProposalError> {
    if engine.cfg().agent.base { return Err(ProposalError::BaseAgent); }
    let agent_name = engine.cfg().agent.name.clone();
    let updated = crate::db::agent_plans::try_set_proposal_status(db, &agent_name, proposal_id, "dismissed")
        .await.map_err(|e| ProposalError::Db(e.to_string()))?;
    Ok(updated.is_some())
}

pub(crate) async fn cancel_goal(db: &sqlx::PgPool, engine: &Arc<AgentEngine>, session_id: uuid::Uuid) -> Result<bool, ProposalError> {
    if engine.cfg().agent.base { return Err(ProposalError::BaseAgent); }
    let cancelled = crate::db::session_goals::try_cancel_goal(db, session_id)
        .await.map_err(|e| ProposalError::Db(e.to_string()))?;
    if cancelled { if let Some(pool) = engine.cfg().goal_pool.clone() { crate::agent::goal::pool::stop(&pool, session_id); } }
    Ok(cancelled)
}
```

- [ ] **Step 3: Переписать web-хендлеры**

`api_approve_proposal`/`api_dismiss_proposal` (`initiative.rs`): `validate_agent_name` + `app.agents.get_engine(&name)` (404) → зовут `approve_proposal`/`dismiss_proposal(&app.infra.db, &engine, id)`; маппинг `Err(BaseAgent)→403`, `Err(Db)→500`, `Ok(ApproveOutcome)`/`Ok(bool)→json`.

- [ ] **Step 4: Проверка** — `cargo check --all-targets -p opex-core && cargo clippy -p opex-core --all-targets -- -D warnings` → 0/0.

- [ ] **Step 5: Commit** — `git commit -m "feat(initiative): dismiss + try_cancel_goal + web handlers via shared fns"`

---

### Task 5: Web cancel-роут + миграция status + UI-кнопка

**Files:** Create `migrations/078_initiative_status_and_origin.sql`; Modify `crates/opex-core/src/gateway/handlers/agents/initiative.rs`, `ui/src/app/(authenticated)/agents/plan/page.tsx`, `ui/src/lib/*` (API-клиент/queries), `ui/src/types/api.ts` (`AgentPlanActiveGoal.session_id`)

- [ ] **Step 1: Миграция 078 (status CHECK + origin comment)**

```sql
-- session_goals.status CHECK (m056) не пускал 'cancelled'; фаза 2A добавляет отмену.
ALTER TABLE session_goals DROP CONSTRAINT IF EXISTS session_goals_status_check;
ALTER TABLE session_goals ADD CONSTRAINT session_goals_status_check
    CHECK (status IN ('active','paused','done','cleared','cancelled'));
-- m077 говорил initiative NOT re-driven; фаза 2A добавляет durable re-drive.
COMMENT ON COLUMN session_goals.origin IS
  'goal = interactive /goal (never re-driven); cron = autonomous cron (crash re-driven); initiative = owner-approved self-initiated (crash re-driven since phase 2A).';
```
*(Автоимя inline-CHECK m056 — `session_goals_status_check`.)*

- [ ] **Step 2: Web cancel-роут**

В `routes()`: `.route("/api/agents/{name}/plan/goals/{session_id}/cancel", post(api_cancel_goal))`. Хендлер: `validate_agent_name` + `get_engine` (404) → `cancel_goal(&app.infra.db, &engine, session_id)` → `{ok, cancelled}`. Также в `api_get_plan` добавить `"session_id": g.session_id` в элементы `active_goals`.

- [ ] **Step 3: UI**

В `ui/src/types/api.ts` — `AgentPlanActiveGoal` += `session_id: string`. В `agents/plan/page.tsx` — кнопка «Отменить» у active-целей → `cancelGoal(name, sessionId)` (POST, API-клиент рядом с approve/dismiss) + инвалидация `qk.agentPlan`. Design-токены, shadcn Button.

- [ ] **Step 4: Проверка** — `cargo check --all-targets -p opex-core` + `cd ui && npm run build` → 0 / build ok.

- [ ] **Step 5: Commit** — `git commit -m "feat(initiative): cancel goal (m078 status CHECK + web route + UI button)"`

---

### Task 6: Telegram callback-intercept + cancel-кнопка после approve

**Files:** Modify `crates/opex-core/src/gateway/handlers/channel_ws/inline.rs`, `crates/opex-core/src/gateway/handlers/channel_ws/reader.rs`

- [ ] **Step 1: `handle_initiative_callback`**

Зеркало `handle_approval_callback` (`inline.rs:171`), параметры идентичны (`ctx: &CwsCtx, engine: &Arc<AgentEngine>, agent_name, request_id, msg, out_tx`). is_callback-guard; парс `iappr:{uuid}`/`idismiss:{uuid}` (мусор → `false`); owner-гейт `ctx.auth.access_guards.read().await.get(agent_name).cloned().is_some_and(|g| g.is_owner(&user_id))` (fail-closed → error-frame+`true`); `db = &ctx.infra.db`. **Пер-веточная обработка** (НЕ общий tuple — типы Ok разные):
```rust
    if let Some(id_str) = text.strip_prefix("iappr:") {
        let Ok(id) = id_str.parse::<uuid::Uuid>() else { return true };
        match crate::gateway::handlers::agents::initiative::approve_proposal(db, engine, id).await {
            Ok(out) => { /* Done «✅ Одобрено, цель запущена» + при out.session_id послать send_buttons [⏹ Отменить → icancel:{sid}] */ }
            Err(_) => { /* Error frame */ }
        }
        return true;
    }
    if let Some(id_str) = text.strip_prefix("idismiss:") {
        let Ok(id) = id_str.parse::<uuid::Uuid>() else { return true };
        let _ = crate::gateway::handlers::agents::initiative::dismiss_proposal(db, engine, id).await;
        /* Done «❌ Отклонено» */; return true;
    }
    if let Some(id_str) = text.strip_prefix("icancel:") {
        let Ok(sid) = id_str.parse::<uuid::Uuid>() else { return true };
        let _ = crate::gateway::handlers::agents::initiative::cancel_goal(db, engine, sid).await;
        /* Done «⏹ Отменено» */; return true;
    }
    false
```
**Cancel-кнопка (закрывает «icancel недоставляем»):** после успешного `approve_proposal` с `session_id`, отправить владельцу `ChannelAction{name:"send_buttons", params:{text:"Цель запущена", buttons:[{text:"⏹ Отменить", data:format!("icancel:{sid}")}]}, context:{chat_id}, target_channel}` через `ctx` channel_router (или out_tx-путь, свериться как approval шлёт ответ). Так `icancel:` становится достижимым из Telegram.

*(Свериться: как получить channel_router/послать send_buttons из callback-контекста; `Done`/`Error` frame — образец `handle_approval_callback` хвост `inline.rs:224-240`.)*

- [ ] **Step 2: Вплести в reader.rs** — рядом с `handle_approval_callback` (~115), после него; consumed → `continue`.

- [ ] **Step 3: Wire-guard тест** — по образцу `approval_wired_before_clarify` (`reader.rs:225-251`, `include_str!`): `handle_initiative_callback` вызван до `dispatch_message`.

- [ ] **Step 4: Проверка** — `cargo check --all-targets -p opex-core && cargo clippy -p opex-core --all-targets -- -D warnings` → 0/0.

- [ ] **Step 5: Commit** — `git commit -m "feat(initiative): owner-gated telegram approve/dismiss/cancel callback + cancel button"`

---

### Task 7: Durable re-drive инициативных целей

**Files:** Modify `crates/opex-core/src/db/session_goals.rs` (`RedrivableGoal.origin` + `list_redrivable` + тест), `crates/opex-core/src/main.rs` (`resume_autonomous_goals`)

- [ ] **Step 1: `RedrivableGoal.origin` + list_redrivable**

`RedrivableGoal` += `pub origin: String`. `list_redrivable` SELECT: `SELECT g.session_id, s.agent_id, g.origin ...`, фильтр `g.origin = 'cron'` → `g.origin IN ('cron','initiative')`, tuple `(Uuid,String,String)`, map с origin. Обновить doc-комментарий.

- [ ] **Step 2: resume GoalTarget**

В `main.rs` (~985), `spawn_goal_driver(engine.clone(), rg.session_id, None)` → :
```rust
                let target = if rg.origin == "initiative" {
                    let owner = engine.cfg().agent.access.as_ref().and_then(|a| a.owner_id.clone());
                    crate::agent::initiative::delivery::resolve_owner_target(&db, &rg.agent_id, owner.as_deref()).await
                } else { None };
                let handle = crate::agent::goal::driver::spawn_goal_driver(engine.clone(), rg.session_id, target);
```
Поправить stale-комментарий строки ~985.

- [ ] **Step 3: Расширить sqlx-тест**

`list_redrivable_selects_only_crashed_cron_goals` (`session_goals.rs:401`) → добавить active crashed initiative-строку (ожидается в результате) + проверка `origin` в результате; `origin='goal'` по-прежнему НЕ выбирается; `cancelled` initiative НЕ выбирается.

- [ ] **Step 4: Проверка** — `cargo check --all-targets -p opex-core && cargo clippy -p opex-core --all-targets -- -D warnings` → 0/0.

- [ ] **Step 5: Commit** — `git commit -m "feat(initiative): durable re-drive for initiative goals (RedrivableGoal.origin + config GoalTarget)"`

---

### Task 8: sqlx race + orchestration тесты

**Files:** Modify `crates/opex-core/src/db/agent_plans.rs` (race-тесты); `crates/opex-core/src/gateway/handlers/agents/initiative.rs` — если возможен sqlx-тест orchestration (иначе отметить E2E)

- [ ] **Step 1: sqlx race-тесты примитивов**

```rust
#[sqlx::test(migrations = "../../migrations")]
async fn concurrent_try_add_proposal_respects_cap(pool: sqlx::PgPool) -> sqlx::Result<()> {
    get_or_create(&pool, "raceA").await.unwrap();
    let today = chrono::Utc::now().date_naive();
    let mk = |t: &str| Proposal { id: uuid::Uuid::new_v4(), text: t.into(), status: "pending".into(), created_at: chrono::Utc::now(), acted_at: None };
    let (p1, p2) = (mk("g1"), mk("g2"));
    let (r1, r2) = tokio::join!(try_add_proposal(&pool,"raceA",today,1,&p1), try_add_proposal(&pool,"raceA",today,1,&p2));
    assert_eq!([r1.unwrap(), r2.unwrap()].iter().filter(|x| **x).count(), 1);
    let plan = get_or_create(&pool, "raceA").await.unwrap();
    assert_eq!(plan.proposals_today, 1);
    assert_eq!(plan.parsed_proposals().len(), 1);
    Ok(())
}

#[sqlx::test(migrations = "../../migrations")]
async fn concurrent_approve_flip_wins_once(pool: sqlx::PgPool) -> sqlx::Result<()> {
    get_or_create(&pool, "raceB").await.unwrap();
    let today = chrono::Utc::now().date_naive();
    let id = uuid::Uuid::new_v4();
    try_add_proposal(&pool, "raceB", today, 1, &Proposal { id, text:"g".into(), status:"pending".into(), created_at: chrono::Utc::now(), acted_at: None }).await.unwrap();
    let (a, b) = tokio::join!(try_set_proposal_status(&pool,"raceB",id,"approved"), try_set_proposal_status(&pool,"raceB",id,"approved"));
    assert_eq!([a.unwrap(), b.unwrap()].iter().filter(|x| x.is_some()).count(), 1);
    Ok(())
}
```
*(Путь `migrations` — как в существующих `#[sqlx::test]` `session_goals.rs`.)*

- [ ] **Step 2: L1 orchestration-тест (`try_cancel_goal` + approve idempotency)**

sqlx-тест `try_cancel_goal`: active→true+status cancelled; повторный→false; done-цель не трогается. (Полный `approve_proposal`-concurrency — интеграционный на сервере: двойной approve → один session_goals; отметить в §E2E.)

- [ ] **Step 3: Проверка** — `cargo check --all-targets -p opex-core` (sqlx на сервере).

- [ ] **Step 4: Commit** — `git commit -m "test(initiative): sqlx race + cancel atomicity tests"`

---

### Task 9: TS-адаптер (channels)

**Files:** Modify `channels/src/drivers/telegram.ts`, `channels/src/localization.ts`

- [ ] **Step 1: Локализация** — в `interface Strings` (`localization.ts:6`): `initiativeHeader: string; initiativeApprove: string; initiativeDismiss: string;`; RU: `"💡 Предложение цели"`/`"✅ Одобрить"`/`"❌ Отклонить"`; EN аналогично.

- [ ] **Step 2: `case "initiative_proposal"`** — в `telegram.ts::executeAction` рядом с `approval_request` (~1170):
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
      await bot.api.sendMessage(chatId, body, { reply_markup: keyboard, reply_parameters: safeReplyParams(messageId) });
      break;
    }
```
(`chatId` из `context.chat_id` в шапке — T2 кладёт туда. `send_buttons` для cancel-кнопки уже существует — T6 его переиспользует, TS-код готов.)

- [ ] **Step 3: Проверка** — `cd channels && bun test` (+ typecheck проекта) → тесты не сломаны.

- [ ] **Step 4: Commit** — `git commit -m "feat(channels): render initiative proposal with approve/dismiss inline buttons"`

---

## Замечания по исполнению

- **Порядок:** 1→2→3→4→5→6→7→8→9. Зависимости: 1→2,3,7; 3→4 (общие типы); 4→5,6; 2→9 (wire-контракт). 8 независим (после 3/4 для tx-функций).
- **Тесты Rust — на сервере** (bin + sqlx нужен live PG). Windows: `cargo check` + `clippy -D`. TS: `bun test`. UI: `npm run build`.
- **Не покрыто автотестами (интеграция/E2E на сервере):** base-refuse обоими путями (M3); полный `approve_proposal` двойной-approve → один goal (L1 orchestration); owner-гейт callback'а. Отметить в E2E-прогоне.
- **H1 инвариант** (owner из конфига) — зафиксирован в коде (`approve_proposal`/resume берут `engine.cfg().agent.access.owner_id`), доп. проверка на ревью.
- **E2E (сервер, manual):** агент с telegram+owner → предложение с кнопками → ✅ → goal стартует + приходит cancel-кнопка → результат в DM; cancel (web/TG) → цель стоп, не воскресает; краш посреди → рестарт → re-drive → результат владельцу; двойной approve web+TG → один goal.
- **Свериться:** executor-замена `db`→`&mut **tx` в tx-вариантах; как callback шлёт `send_buttons`/Done из `inline.rs`-контекста; путь `migrations` в `#[sqlx::test]`; блок `if added` в tick.rs (hoist `clean_rationale`).
