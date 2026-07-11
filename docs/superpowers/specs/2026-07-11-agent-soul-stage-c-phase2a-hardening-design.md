# Этап C — Фаза 2A «Harden/завершение инициативы» — Design Spec

**Дата:** 2026-07-11
**Статус:** проектирование (ревизия 2 — после тройного ревью: сверка-с-кодом / безопасность / полнота)
**База:** задеплоенный gated-v1 (`docs/superpowers/specs/2026-07-11-agent-soul-stage-c-initiative-design.md`, §8); research находка 4.
**Область:** harden ЖИВОЙ инициативы. Иерархическая декомпозиция — ОТДЕЛЬНЫЙ цикл (Батч B), НЕ здесь.

---

## 1. Цель и не-цели

**Цель:** довести gated-инициативу до полноты — настоящий per-principal гейт через Telegram, доставка предложений/результатов в канал владельца, отмена одобренной цели, устойчивость к крашу, эмпирическая проверка атомарности.

**Не-цели (Батч B):** иерархическая декомпозиция, реактивное перепланирование, авто-approve, доп. источники целей.

**Инвариант:** без явного одобрения владельца автономный запуск невозможен; одобрение доступно из web И Telegram, оба owner-гейтятся; одобренную цель владелец может отменить.

---

## 2. Что переиспользуется (подтверждено ревью)

- `handle_approval_callback` (`channel_ws/inline.rs:171`) — Telegram callback с owner-гейтом `live_guard.is_owner(&user_id)` (`ctx.auth.access_guards`, fail-closed: нет guard → false). Точный образец.
- `reader.rs` intercept-цепочка (~114-132) перед `dispatcher::dispatch_message` (~134); есть wire-guard тест-паттерн (`reader.rs:225-251`, `include_str!` на порядок).
- `approval_manager.rs:154-190` — `ChannelAction{name:"approval_request", params, context, reply, target_channel}` через `channel_router`, reply-oneshot + 5s timeout + `.ok()`. `chat_id` едет в **`context`** (не params).
- `ChannelAction` (`agent/channel_actions.rs:8-20`, `reply` — НЕ Option, oneshot обязателен); `ChannelActionRouter` `#[derive(Clone)]` (`:31`); `AgentState.channel_router: Option<ChannelActionRouter>` (`agent_state.rs:61`).
- `channels/src/drivers/telegram.ts::executeAction` — `chatId` извлекается из `action.context?.chat_id` ДО switch (early-return при отсутствии); `send_buttons` (64-байт лимит callback_data); `channels/src/localization.ts` (`Strings`, RU/EN пары approvalHeader/Approve/Reject).
- `agent_channels(agent_name, channel_type, config, status)` (m001); `status='running'` — валидное значение (`channels.rs:277`).
- `session_goals`: `list_redrivable` (origin='cron' фильтр, `:204`), `RedrivableGoal{session_id, agent_id}` (`:177`, БЕЗ origin), `resume_autonomous_goals` (`main.rs:932`, hardcode `GoalTarget::None` `:985` со stale-комментом), `claim_redrive` (`sessions.rs:1106`, `retry_count<max_retries` атомарно), `next_redrive_at` backoff, `GoalTarget = Option<(String,i64)>` (`goal/pool.rs:11`).
- Web-хендлеры `api_approve_proposal`/`api_dismiss_proposal` (`gateway/handlers/agents/initiative.rs:42-98`) — тело выносится.
- `try_set_proposal_status`/`try_add_proposal` (`db/agent_plans.rs`) — атомарные (подтверждено race-safe).

---

## 3. Компоненты

### 3.0 Плюмбинг `channel_router` в `initiative_tick` (ревью-блокер)

`initiative_tick` зовётся из detached bg-таска (`knowledge_extractor.rs:181` ← `finalize.rs` `tracker.spawn`) и не имеет доступа к `AgentEngine`. Единственный путь — `InitiativeDeps`. Добавить поле:
```rust
pub struct InitiativeDeps { ... pub channel_router: Option<ChannelActionRouter> }
```
В `finalize_context_from_engine` (`finalize.rs`) — `channel_router: engine.state().channel_router.clone()` (рядом с `ui_event_tx`). Поправить тест-конструкторы `InitiativeDeps` (finalize.rs unit-тесты). §7 включает `finalize.rs`.

### 3.1 Резолвер `owner_id → (channel, chat_id)`

Новый `agent/initiative/delivery.rs`:
```
async fn resolve_owner_target(db, agent_name, owner_id: &str) -> Option<(String /*channel*/, i64 /*chat_id*/)>
```
- SQL: `SELECT channel_type FROM agent_channels WHERE agent_name=$1 AND channel_type='telegram' AND status='running' ORDER BY created_at LIMIT 1` (детерминизм; v1 — только telegram).
- `chat_id = owner_id.parse::<i64>().ok()?`.
- Fail-soft: нет канала / owner_id не число → `None` → доставка в канал пропускается (web остаётся).

**ИНВАРИАНТ БЕЗОПАСНОСТИ (H1):** `owner_id`, передаваемый в резолвер и в `GoalTarget`, ВСЕГДА берётся сервер-сайд из `agent.access.owner_id` / `AccessGuard.owner_id`. НИКОГДА из callback_data, тела запроса или `msg.user_id`. Юнит-тест фиксирует это.

### 3.2 Доставка предложения в Telegram + inline-кнопки

При успешном `try_add_proposal` (в `initiative_tick`, ПОСЛЕ web-`notify()`), если `resolve_owner_target(db, agent, deps.owner_id?)` = `Some((ch, chat_id))` И `deps.channel_router` = Some:
- построить `ChannelAction { name:"initiative_proposal", params:{proposal_id, text, rationale}, context:json!({"chat_id": chat_id}), reply:<throwaway oneshot>, target_channel:Some(ch) }`; `channel_router.send(action)`, затем `tokio::time::timeout(5s, reply_rx).await.ok()` (образец approval_manager). Fail-soft.
- **`chat_id` в `context`, НЕ params** (ревью-блокер: `telegram.ts` читает `context.chat_id` до switch).

**TS-адаптер** (`channels/src/drivers/telegram.ts`): новый `case "initiative_proposal"` в `executeAction` — `sendMessage(chatId, header+text+rationale)` + inline-клавиатура (`send_buttons`-паттерн) из `iappr:{proposal_id}` (✅) и `idismiss:{proposal_id}` (❌). Новые ключи локализации в `channels/src/localization.ts` (`initiativeHeader`, `initiativeApprove`, `initiativeDismiss`, RU/EN). callback_data `iappr:{uuid}` ≤43 байт < 64 — ок.

Web-bell `notify('initiative_proposal')` НЕ трогаем — оба канала.

### 3.3 Telegram callback-intercept инициативы

`handle_initiative_callback` в `channel_ws/inline.rs` (зеркало `handle_approval_callback`):
- при `is_callback`, парс `iappr:{uuid}` / `idismiss:{uuid}` / `icancel:{uuid}` (§3.6); мусор/не-UUID → consume без действия.
- **owner-гейт** `live_guard.is_owner(&user_id)` (fail-closed) — не-владелец → error-frame + consume.
- вызывает общую функцию (§3.5): approve/dismiss по proposal_id; cancel по session_id (§3.7-cancel).
- `agent_name` берётся из WS-СОЕДИНЕНИЯ (не из callback payload) → agent-scoping; `try_set_proposal_status(agent_id=<conn agent>, id)` не тронет чужое предложение.
- ответ `Done` «✅ Одобрено»/«❌ Отклонено»/«⏹ Отменено».
- вплетается в `reader.rs` рядом с `handle_approval_callback`; **wire-guard тест** (по образцу `approval_wired_before_clarify`) на порядок перед dispatcher.

### 3.4 Доставка результата цели в канал

`GoalTarget` строится через `resolve_owner_target` (owner_id из конфига агента, §3.1-H1) → `Some((ch, chat_id))` вместо `None`. Goal-driver доставляет финал в DM владельца. Fail-soft: резолвер `None` → `GoalTarget::None` (web).

### 3.5 Общая функция approve/dismiss (ревью M3)

Тело web-хендлеров выносится в `agent/initiative/` (или `initiative.rs`) функции с сигнатурой БЕЗ HTTP-типов (CwsCtx≠AppState):
```rust
async fn approve_proposal(db: &PgPool, engine: &Arc<AgentEngine>, proposal_id: Uuid)
    -> Result<ApproveOutcome, ProposalError>
async fn dismiss_proposal(db: &PgPool, engine: &Arc<AgentEngine>, proposal_id: Uuid)
    -> Result<bool, ProposalError>
```
- `engine` уже резолвнут в обоих местах (web: `app.agents.get_engine(&name)`; TG: `engine: &Arc<AgentEngine>` в inline.rs).
- **ВСЕ гейты — ВНУТРИ функции** (M3): base-refuse (`engine.cfg().agent.base` → `ProposalError::BaseAgent`); атомарный `try_set_proposal_status`; при `None` → `ApproveOutcome{spawned:false}` (идемпотентно). `agent_name = engine.cfg().agent.name` (не из payload).
- **approve в ОДНОЙ транзакции (L1):** флип статуса + `create_new_session` + `upsert_initiative_goal` атомарны; при краше посреди — либо всё, либо ничего (нет «approved без goal»). spawn goal-driver — после коммита. `GoalTarget` через резолвер (owner из конфига).
- `ProposalError` варианты: `BaseAgent` (403 web / «только non-base» TG), `Db(..)` (500 / ошибка), — `NotPending` НЕ ошибка (идемпотентный no-op, `spawned:false`). `ApproveOutcome{spawned:bool, session_id:Option<Uuid>}`. Web маппит в HTTP, TG — в Done/Error frame.
- Оба вызывающих сохраняют `validate_agent_name` (web) / owner-гейт (TG) ДО вызова.

### 3.6 Durable re-drive инициативных целей (ревью-уточнения)

- `list_redrivable` (`db/session_goals.rs:204`): фильтр `g.origin IN ('cron','initiative')`; **`RedrivableGoal` += поле `origin: String`** (добавить в SELECT + структуру — иначе resume не различит cron/initiative).
- `resume_autonomous_goals` (`main.rs:932`): для `origin='initiative'` строить `GoalTarget` через `resolve_owner_target(db, rg.agent_id, engine.cfg().agent.access.owner_id)` (owner из КОНФИГА, не из системной сессии где `user_id='system'`/`chat_id=NULL`, ревью M2); для `origin='cron'` — `GoalTarget::None` как сейчас. Поправить stale-комментарий (`main.rs:985`).
- Обновить sqlx-тест `list_redrivable_selects_only_crashed_cron_goals` → включает initiative-строку (и проверяет, что `origin` возвращается).
- Миграция 078: обновить `COMMENT ON COLUMN session_goals.origin` (в m077 сказано «initiative NOT re-driven in v1» — теперь re-driven).
- Безопасность (подтверждено CONFIRMED-SAFE): только `status='active'` воскрешается; `done`/`cleared`/`cancelled`/`paused` — нет; капы `max_retries=3`+backoff+`MAX_PER_BOOT=5`; deny-list/бюджет наследуются.

### 3.7 Отмена одобренной цели (ревью M1 — новый компонент)

Одобренную active-цель владелец сейчас отменить не может (dismiss работает только по `pending`-предложению; `/goal stop` требует system-session id, недоступный владельцу). Добавить:
- Общая `cancel_goal(db, engine, session_id) -> Result<bool, ProposalError>`: перевести `session_goals.status` `active→'cancelled'` (owner-gated вызывающими) + остановить драйвер (`goal_pool` cancel по session_id, образец `pool.rs:46`). `cancelled` НЕ воскрешается re-drive (§3.6 фильтр active).
- Web: `POST /api/agents/{name}/plan/goals/{session_id}/cancel` (в `initiative.rs`, `validate_agent_name`+base-refuse).
- Telegram: callback `icancel:{session_id}` (§3.3, owner-гейт).
- UI: кнопка «Отменить» у active initiative-целей на `/agents/plan` (расширение существующей панели; API-клиент `cancelGoal`).

### 3.8 sqlx race-тесты атомарности (ревью: новая инфра в agent_plans.rs)

`#[sqlx::test]` (live PG) в `db/agent_plans.rs` (сейчас там только чистый `#[test]` — добавляем sqlx-инфру):
- **cap-гонка:** `tokio::join!` двух `try_add_proposal(cap=1)` → ровно один `true`, `proposals_today=1`, одна строка в `proposals`.
- **approve-гонка:** `tokio::join!` двух `try_set_proposal_status(approve)` → ровно один `Some`.

---

## 4. Поток данных (E2E)

```
initiative_tick: try_add_proposal(ok) →
   notify(web bell) + [resolve_owner_target(owner из конфига) → ChannelAction 'initiative_proposal'
                       (chat_id в context) → TG DM + [✅ iappr][❌ idismiss]]
владелец ✅ в TG → callback iappr:{id} → owner-gate → approve_proposal(db, engine, id):
   [tx: try_set_proposal_status(pending→approved) → create_new_session → upsert_initiative_goal] →
   spawn_goal_driver(GoalTarget=resolve_owner_target(owner из конфига))
goal-driver: ходы → done → результат в DM владельца
владелец передумал → icancel/web cancel → session_goals active→cancelled → драйвер стоп
краш посреди → рестарт → list_redrivable(cron|initiative, origin в RedrivableGoal) →
   для initiative: GoalTarget из конфига agent.owner_id → respawn с backoff/cap → результат владельцу
```

---

## 5. Обработка ошибок

- Резолвер / доставка / TG-callback — fail-soft (провал не рушит инициативу/сессию; нет канала → web-only).
- Двойной approve (web+TG) → атомарный `try_set_proposal_status` → один spawn (подтверждено CONFIRMED-SAFE).
- Не-владелец в callback → error-frame + consume (fail-closed).
- approve-транзакция (L1): краш посреди → откат, нет «approved без goal».
- Re-drive: только `active`; исчерпание max_retries → paused; cancelled/done не воскрешаются.
- Open-mode агент без owner_id (L2): TG-approve недоступен (fail-closed, безопасно) — web-approve за bearer-токеном остаётся; требовать owner_id для инициативных агентов с telegram.

---

## 6. Тестирование

- **sqlx (live PG, сервер):** cap-гонка, approve-гонка (§3.8); расширенный `list_redrivable`-тест (initiative-строка + origin в результате).
- **Юнит:** резолвер (число/не-число owner_id, нет канала → None; **owner_id только из конфига — H1-тест**); парс callback `iappr:`/`idismiss:`/`icancel:` (валид/мусор); owner-гейт (не-владелец отклонён); base-refuse в `approve_proposal` по обоим путям (M3); **wire-guard тест** `handle_initiative_callback` вплетён до dispatcher (`reader.rs`, образец существующих).
- **E2E на сервере (manual, non-CI):** агент с telegram+owner_id → предложение в TG с кнопками → ✅ → goal-driver стартует, результат в DM; cancel → цель остановлена, не воскресает; краш core посреди → рестарт → re-drive → результат владельцу; двойной approve web+TG → один goal.

---

## 7. Файловая структура (для плана)

- `crates/opex-core/src/agent/initiative/delivery.rs` (новый) — `resolve_owner_target` + сборка `ChannelAction` предложения.
- `crates/opex-core/src/agent/initiative/tick.rs` — вызов доставки после `try_add_proposal`; использует `deps.channel_router`.
- `crates/opex-core/src/agent/pipeline/finalize.rs` — `channel_router` в `InitiativeDeps` + `finalize_context_from_engine` + тест-конструкторы.
- `crates/opex-core/src/agent/initiative/mod.rs` или `initiative.rs` — общие `approve_proposal`/`dismiss_proposal`/`cancel_goal` (§3.5/§3.7), `ProposalError`/`ApproveOutcome`.
- `crates/opex-core/src/gateway/handlers/agents/initiative.rs` — web-хендлеры зовут общие функции; новый cancel-роут.
- `crates/opex-core/src/gateway/handlers/channel_ws/inline.rs` — `handle_initiative_callback`.
- `crates/opex-core/src/gateway/handlers/channel_ws/reader.rs` — intercept + wire-guard тест.
- `crates/opex-core/src/db/session_goals.rs` — `list_redrivable` фильтр + `RedrivableGoal.origin` + тест.
- `crates/opex-core/src/main.rs` — `resume_autonomous_goals` GoalTarget для initiative + stale-коммент.
- `crates/opex-core/src/db/agent_plans.rs` — sqlx race-тесты.
- `migrations/078_*.sql` — обновить COMMENT origin.
- `channels/src/drivers/telegram.ts` + `channels/src/localization.ts` (TS) — `case "initiative_proposal"` → DM + кнопки `iappr:`/`idismiss:`; ключи локализации.
- `ui/src/app/(authenticated)/agents/plan/page.tsx` + API-клиент — кнопка «Отменить» для active-целей.

**Декомпозиция (~9 задач, порядок по зависимостям):** (1) резолвер+delivery.rs; (2) channel_router в InitiativeDeps+finalize; (3) доставка предложения tick.rs [зафиксировать wire-контракт chat_id-в-context ДО TS]; (4) общие approve/dismiss/cancel + ProposalError/tx; (5) web cancel-роут; (6) TG callback-intercept+wire-guard [зависит 4]; (7) durable re-drive + RedrivableGoal.origin + m078; (8) sqlx race-тесты [независим]; (9) TS-адаптер+localization [зависит контракт из 3]; (+UI cancel-кнопка). 1/8 параллелятся; 2→3→9, 4→5,6, 1→7.

---

## 8. Что дальше (Батч B, отдельный цикл)

Иерархическая декомпозиция: дневной план → 5-15-мин чанки → реактивное перепланирование. Отдельная спека.
