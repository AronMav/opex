# Этап C — Фаза 2A «Harden/завершение инициативы» — Design Spec

**Дата:** 2026-07-11
**Статус:** проектирование (ревизия 1)
**База:** задеплоенный gated-v1 (`docs/superpowers/specs/2026-07-11-agent-soul-stage-c-initiative-design.md`, §8 «что дальше»); research находка 4 (plan-decompose-react).
**Область:** harden/достройка ЖИВОЙ инициативы. Иерархическая декомпозиция (дневной план→чанки→реплан) — ОТДЕЛЬНЫЙ цикл (Батч B), НЕ здесь.

---

## 1. Цель и не-цели

**Цель:** довести задеплоенную gated-инициативу до полноты — настоящий per-principal гейт через Telegram, доставка предложений и результатов в канал владельца, устойчивость к крашу, эмпирическая проверка атомарности.

**Не-цели (Батч B / позже):** иерархическая декомпозиция плана, реактивное перепланирование, авто-approve, дополнительные источники целей.

**Инвариант сохраняется:** без явного одобрения владельца автономный запуск невозможен (теперь одобрение доступно из web И Telegram, оба owner-гейтятся).

---

## 2. Что переиспользуется

- `handle_approval_callback` (`gateway/handlers/channel_ws/inline.rs:171`) — Telegram inline-callback с **owner-гейтом** `live_guard.is_owner(&user_id)` (`ctx.auth.access_guards`). Точный образец для инициативного callback.
- `reader.rs` (~114) — точка intercept callback'ов перед dispatcher.
- `approval_manager.rs:154-190` — отправка `ChannelAction{name:"approval_request", params, target_channel}` через `channel_router`; TS-адаптер рендерит inline-кнопки. Образец доставки предложения.
- `ChannelAction { name, params, context, reply, target_channel: Option<String> }` (`agent/channel_actions.rs:8`); `channel_router: Option<ChannelActionRouter>` (`agent_state.rs:61`).
- `agent_channels(agent_name, channel_type, config, status)` (m001) — источник канала для резолвера.
- `session_goals`: `list_redrivable` (origin='cron' hard-filter), `resume_autonomous_goals` (`main.rs:788`), `max_retries`/`next_redrive_at` backoff, `GoalTarget = Option<(String,i64)>`.
- Web approve/dismiss хендлеры (`gateway/handlers/agents/initiative.rs`) — тело выносится в общую функцию.

---

## 3. Компоненты

### 3.1 Резолвер `owner_id → (channel, chat_id)`

Новый хелпер (напр. `agent/initiative/delivery.rs` или в `db/agent_channels`): по имени агента находит его messaging-канал и chat_id владельца.

```
async fn resolve_owner_target(db, agent_name, owner_id: &str) -> Option<(String /*channel_type*/, i64 /*chat_id*/)>
```

- Запрос: `SELECT channel_type FROM agent_channels WHERE agent_name=$1 AND channel_type IN ('telegram') AND status='running' LIMIT 1` (v1 — только telegram; расширяемо).
- `chat_id = owner_id.parse::<i64>()` (для Telegram DM chat_id = user_id).
- Fail-soft: нет канала / owner_id не число → `None` → доставка в канал пропускается (web остаётся).

### 3.2 Доставка предложения в Telegram + inline-кнопки

При успешном `try_add_proposal` (в `initiative_tick`, ПОСЛЕ web-`notify()`), если `resolve_owner_target` дал `Some((ch, chat_id))`:
- отправить `ChannelAction{ name:"initiative_proposal", params:{proposal_id, text, rationale, chat_id}, target_channel:Some(ch) }` через `channel_router` (fail-soft: нет router → только web).
- **TS-адаптер** (`channels/src/`): обработать действие `initiative_proposal` → отправить владельцу DM с текстом+обоснованием и inline-клавиатурой из двух кнопок: `iappr:{proposal_id}` (✅) и `idismiss:{proposal_id}` (❌). Зеркало обработки `approval_request`.

Web-bell `notify('initiative_proposal')` НЕ трогаем — оба канала.

### 3.3 Telegram callback-intercept инициативы

Новый `handle_initiative_callback` в `channel_ws/inline.rs` (зеркало `handle_approval_callback`):
- срабатывает при `is_callback`, парсит `iappr:{uuid}` / `idismiss:{uuid}` (иначе `false` — пропуск дальше).
- **owner-гейт:** `live_guard.is_owner(&user_id)` — не-владелец → error-frame + consume (как approval).
- вызывает ОБЩУЮ серверную логику (§3.5): approve → spawn goal-driver (с GoalTarget владельца, §3.4); dismiss → статус.
- отвечает `Done` с «✅ Одобрено»/«❌ Отклонено».
- вплетается в `reader.rs` intercept-цепочку РЯДОМ с `handle_approval_callback` (перед dispatcher). Порядок: сначала approval, потом initiative (разные префиксы — коллизии нет).

### 3.4 Доставка результата цели в канал

При approve (из web ИЛИ Telegram) `GoalTarget` строится через `resolve_owner_target(...)` → `Some((ch, chat_id))` вместо `None`. Существующий goal-driver доставляет финальный ответ цели в DM владельца. Fail-soft: резолвер `None` → `GoalTarget::None` (результат в web, как v1).

### 3.5 Общий рефактор approve/dismiss

Тело web-хендлеров `api_approve_proposal`/`api_dismiss_proposal` выносится в чистые от HTTP функции (напр. в `gateway/handlers/agents/initiative.rs` или `agent/initiative/`):
```
async fn approve_proposal(state, agent_name, proposal_id) -> Result<ApproveOutcome, ProposalError>
async fn dismiss_proposal(state, agent_name, proposal_id) -> Result<bool, ProposalError>
```
Зовут ОБА: web-хендлер (маппит в HTTP-ответ) И Telegram-callback (маппит в Done/Error frame). Атомарный `try_set_proposal_status` внутри — двойной approve (web+Telegram гонка) даёт один spawn. `approve_proposal` строит `GoalTarget` через резолвер.

### 3.6 Durable re-drive инициативных целей

- `list_redrivable` (`db/session_goals.rs:194`): фильтр `g.origin = 'cron'` → `g.origin IN ('cron','initiative')`. Те же max_retries + `next_redrive_at` backoff + 4ч-окно.
- `resume_autonomous_goals` (`main.rs`): при respawn инициативной цели восстановить `GoalTarget` через `resolve_owner_target` (результат по-прежнему уйдёт владельцу). Обновить doc-комменты (scope больше не «только cron»).
- Обновить существующий sqlx-тест `list_redrivable_selects_only_crashed_cron_goals` → включает initiative-строку.
- Безопасность: владелец уже одобрил цель; капы (max_retries) + backoff защищают от бесконечного цикла; завершённые (`done`/`cleared`/`dismissed`) НЕ воскрешаются (только `active`).

### 3.7 sqlx race-тесты атомарности

`#[sqlx::test]` (live PG) в `db/agent_plans.rs`:
- **cap-гонка:** засеять plan-строку; `tokio::join!` двух `try_add_proposal(cap=1)` → ровно один вернул `true`, в `proposals` одна строка, `proposals_today=1`.
- **approve-гонка:** засеять pending-proposal; `tokio::join!` двух `try_set_proposal_status(approve)` → ровно один `Some`, второй `None`.

---

## 4. Поток данных (E2E)

```
initiative_tick: try_add_proposal(ok) →
   notify(web bell) + [resolve_owner_target → ChannelAction 'initiative_proposal' → TG DM + [✅][❌]]
владелец жмёт ✅ в Telegram → callback iappr:{id} → owner-gate → approve_proposal():
   try_set_proposal_status(pending→approved) → create_new_session →
   session_goals(origin='initiative') → spawn_goal_driver(GoalTarget=resolve_owner_target)
goal-driver: автономные ходы → done → результат в DM владельца
краш посреди → рестарт → list_redrivable(cron|initiative) → respawn с backoff/cap → результат владельцу
```

---

## 5. Обработка ошибок

- Резолвер, доставка в канал, TG-callback — все fail-soft: провал не рушит инициативу/сессию; при отсутствии канала — web-only.
- Двойной approve (web+Telegram одновременно) → атомарный `try_set_proposal_status` → один spawn, второй путь получает «уже одобрено».
- Не-владелец в callback → error-frame, consume, без действия.
- Re-drive: только `active`-строки; исчерпание max_retries → paused (как cron).

---

## 6. Тестирование

- **sqlx (live PG):** cap-гонка, approve-гонка (§3.7); расширенный `list_redrivable`-тест с initiative-строкой.
- **Юнит:** резолвер (число/не-число owner_id, нет канала → None), парс callback `iappr:`/`idismiss:` (валид/мусор), owner-гейт (не-владелец отклонён).
- **E2E на сервере:** включённый агент с telegram-каналом + owner_id → предложение приходит в TG с кнопками → ✅ → goal-driver стартует, результат в DM; краш core посреди цели → рестарт → re-drive → результат владельцу; двойной approve web+TG → один goal.

---

## 7. Файловая структура (для плана)

- `crates/opex-core/src/agent/initiative/delivery.rs` (новый) — `resolve_owner_target` + сборка `ChannelAction` предложения.
- `crates/opex-core/src/agent/initiative/tick.rs` — вызов доставки после `try_add_proposal`.
- `crates/opex-core/src/gateway/handlers/agents/initiative.rs` — вынести `approve_proposal`/`dismiss_proposal` (общие), web-хендлеры зовут их; GoalTarget через резолвер.
- `crates/opex-core/src/gateway/handlers/channel_ws/inline.rs` — `handle_initiative_callback`.
- `crates/opex-core/src/gateway/handlers/channel_ws/reader.rs` — вплести intercept.
- `crates/opex-core/src/db/session_goals.rs` — `list_redrivable` фильтр + тест.
- `crates/opex-core/src/main.rs` — `resume_autonomous_goals` восстановление GoalTarget + doc.
- `crates/opex-core/src/db/agent_plans.rs` — sqlx race-тесты.
- `channels/src/` (TS) — обработка действия `initiative_proposal` → DM + inline-кнопки `iappr:`/`idismiss:`.

---

## 8. Что дальше (Батч B, отдельный цикл)

Иерархическая декомпозиция: персистентный дневной план → декомпозиция в 5-15-мин чанки → реактивное перепланирование остатка. Отдельная спека.
