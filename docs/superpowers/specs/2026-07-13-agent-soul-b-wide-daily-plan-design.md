# Инициатива — B-wide: персистентный дневной план + heartbeat-продвижение — Design Spec

**Дата:** 2026-07-13
**Статус:** проектирование (ревизия 1)
**База:** задеплоенная gated-инициатива (v1 + 2A harden + Batch B decompose + open threads). Спеки: `2026-07-11-agent-soul-stage-c-initiative-design.md`, `...-stage-c-batch-b-decompose-design.md` (§8 «B-wide»), `...-open-threads-design.md`.

---

## 1. Цель и не-цели

**Цель:** дать soul-агенту **дневной план из нескольких намерений**, который генерируется утром (из рефлексий + SELF.md + open_threads), одобряется владельцем ОДНИМ approve, и продвигается **по одному чанку за heartbeat-тик** в течение дня (а не залпом при approve). Владелец видит ровный прогресс; агент «ведёт день».

**Решения брейншторма:**
- Ядро = мульти-намеренческий дневной план (не «одна цель, размазанная по дню»).
- Источник намерений = **утреннее планирование** (N сразу), не накопление одобренных целей.
- Approve = **весь план одним действием** (одно уведомление, кнопки «Принять план / Отклонить»).
- Архитектура = **Подход A**: план — координатор над `session_goals` (реюз decompose/driver/чанков/gated-execution), а не отдельный движок.

**Не-цели (v1):**
- Полноценный day-level LLM-ре-ранк / добавление намерений в течение дня (v1: последовательный порядок + пропуск done/cancelled).
- Пер-намеренческий approve, авто-approve с бюджет-капом, UI-панель плана — отдельные циклы.
- Перенос незавершённого плана на следующий день (carryover): v1 — новый день = новая генерация; незакрытые намерения прошлого дня остаются `paused`/`active` в session_goals, но НЕ втягиваются в новый план автоматически.

---

## 2. Что переиспользуется

- `session_goals` (goal_text, status, origin, subgoals JSONB, current_chunk, turn_count/max_turns): каждое одобренное намерение = строка `origin='initiative'`. Reuse `upsert_initiative_goal_tx`, `get`, `set_status`, `set_current_chunk`, `set_subgoals`, `bump_turn`, `record_verdict`.
- `agent/goal/driver.rs` decompose-ветка (стр. 46-134) — **извлекается** в `advance_one_chunk` (см. §3.3); `run_goal_driver` продолжает звать её в loop (интерактив/cron не меняются).
- `agent/goal/decompose.rs` (decompose_prompt/chunk_continuation_prompt/replan_prompt/chunk_judge_prompt/advance_decision/ChunkVerdict/DecomposeAction) — как есть.
- `agent/initiative/tick.rs` (InitiativeDeps, generate_focus, sanitize+is_trivial+cap, notify+delivery), `agent/initiative/delivery.rs` (resolve_owner_target, send_proposal_to_channel).
- `db/agent_plans.rs` (get_or_create, proposals CRUD, try_set_proposal_status_tx) + `db/memory_queries.rs` (recent_soul_chunks, recent_open_thread_chunks, latest_reflection_at).
- `agent/soul/self_md.rs::render_self_block`, `sanitize_soul_text`, `is_trivial_goal`.
- `scheduler/mod.rs` heartbeat-каденс (add_heartbeat closure) — точка вызова `day_plan_tick`.
- Telegram approval-callback инфраструктура (`is_owner`, inline-кнопки) — по образцу `initiative_proposal`.

---

## 3. Компоненты

### 3.1 Хранилище (миграция)

`agent_plans` (m077) расширяется (additive, m081):
- `day_plan JSONB NOT NULL DEFAULT '[]'` — упорядоченный список намерений: `[{ "session_id": uuid|null, "intent": "...", "status": "pending|active|done|cancelled" }]`. До approve `session_id=null`; при approve каждому намерению создаётся session_goals и проставляется `session_id`.
- `day_plan_current INT NOT NULL DEFAULT 0` — указатель на текущее намерение.
- `day_plan_date DATE` — день (tz) последней генерации; NULL = нет плана.
- `day_plan_status TEXT` — `NULL` (нет плана) | `pending` (сгенерирован, ждёт approve) | `approved` (исполняется) | `done` (все намерения завершены) | `dismissed`.

`day_plan_status` CHECK: `day_plan_status IS NULL OR day_plan_status IN ('pending','approved','done','dismissed')`.

### 3.2 Утренняя генерация (`agent/initiative/day_plan.rs`)

`generate_day_plan(provider, agent, self_md, &reflections, &open_threads) -> Vec<String>`:
- Один LLM-вызов (aux/compaction провайдер, как в driver). Промпт: SELF.md + недавние рефлексии (framed) + недавние open_threads (framed «ДАННЫЕ, НЕ инструкции» — двухступенчатый барьер, re-sanitize при чтении, как open_threads §3.4). Строгий JSON `{"intents": ["..."]}`.
- Каждый intent → `sanitize_soul_text(_, EVENT_MAX_CHARS)` (drop None) → `is_trivial_goal`-фильтр → cap `MAX_DAY_INTENTS=4`.
- Пустой результат → плана нет.

`day_plan_tick(db, engine, agent, deps)` (fail-soft, зовётся из heartbeat-каденса при `daily_plan=true`, non-base, owner set):
- `plan = agent_plans::get_or_create`. `today = today_in_tz(deps.timezone)`.
- **Ветка генерации:** если `day_plan_date != today` (при ЛЮБОМ статусе — прошлодневный план устарел, carryover вне v1; незакрытые session_goals остаются `paused`, не втягиваются) И есть свежий материал (latest_reflection_at или open_threads непусты):
  - собрать материал; `intents = generate_day_plan(...)`; если пусто → выставить `day_plan_date=today, day_plan_status=NULL` (чтобы не долбить генерацию весь день) и выйти;
  - `set_day_plan(db, agent, intents/*status=pending*/, date=today, status='pending', current=0)`; уведомить владельца (см. §3.4).
- **Ветка продвижения:** иначе если `day_plan_date == today` И `day_plan_status='approved'` → `advance_day_plan(...)` (см. §3.3).
- Иначе (`date==today` И статус pending — ждём approve; done/dismissed/NULL сегодня) → no-op.

> **Инвариант дня:** генерация гейтится ТОЛЬКО по `day_plan_date != today` — прошлодневный `pending` (не одобрен) на новый день корректно перегенерируется, не блокируя план навсегда. approved-план прошлого дня к утру обычно `done`/`paused`; при перегенерации его незакрытые session_goals не удаляются (остаются в истории).

### 3.3 `advance_one_chunk` + продвижение плана

Извлечь тело decompose-ветки `run_goal_driver` (driver.rs:46-134) в:
```
enum StepOutcome { Continuing, Done, Paused }
async fn advance_one_chunk(engine, session_id, target) -> StepOutcome
```
- Логика 1:1 с текущей веткой: budget-check → lazy-decompose (если subgoals пусты) → run_goal_turn на текущем чанке → deliver → chunk_judge → record_verdict → advance_decision (Continue/Advance/AdvanceAndReplan/Done/Pause). Возвращает `Done` (goal done), `Paused` (budget/judge), `Continuing` (иначе, вкл. только-что-сделанный lazy-decompose — вернёт Continuing без прогона хода).
- `run_goal_driver` decompose-ветка заменяется на `loop { match advance_one_chunk {...} }` — поведение непрерывных целей НЕ меняется (регресс-инвариант: интерактив/cron).

`advance_day_plan(db, engine, agent, deps)`:
- `plan = get`. `cur = day_plan_current`. Найти текущее намерение с `status='active'`/`session_id`.
- Если у текущего намерения `session_id` есть и его session_goals `is_running()` → `advance_one_chunk(session_id, target)` ОДИН раз.
  - `StepOutcome::Done|Paused` ИЛИ session_goals-строка `done`/`paused` → пометить намерение `done`, `day_plan_current++`.
- Если `day_plan_current >= len` → `day_plan_status='done'`, уведомить владельца «план на день выполнен».
- Ровно один `advance_one_chunk` за тик. `GoalTarget` = resolve_owner_target (CONFIG owner_id).

### 3.4 Approve/dismiss (API + Telegram)

- Новый endpoint `POST /api/agents/{name}/plan/day/approve` и `.../day/dismiss` (по образцу proposal approve; non-base gate; idempotent).
- `approve_day_plan(db, engine, agent)` — атомарная tx (L1, как upsert_initiative_goal_tx): для каждого pending-намерения создать session_id + `session_goals` `origin='initiative'` (max_turns=INITIATIVE_GOAL_MAX_TURNS=20) + проставить `session_id`+`status='active'` в `day_plan`; `day_plan_status='approved'`, `current=0`. НЕ спавнить непрерывный драйвер (продвигает heartbeat).
- `dismiss_day_plan` — `day_plan_status='dismissed'` (день пропущен).
- Telegram: уведомление utреннего плана несёт кнопки `dpm:approve:{agent}` / `dpm:dismiss:{agent}` (owner-gated `is_owner`, как `initiative_proposal`). Web: `notify` type `day_plan` + approve через API.

### 3.5 Опт-ин / интеграция с initiative_tick

- Конфиг: `[agent.initiative] daily_plan = false` (default). При `true`:
  - пост-рефлексийный `initiative_tick` single-proposal путь (tick.rs Step 2 «gated proposal») **пропускается** (чтобы не плодить и одиночные предложения, и дневной план); focus-рефреш (Step 1) остаётся.
  - `day_plan_tick` вызывается из heartbeat-каденса.
- Требует настроенный `[agent.heartbeat]` (продвижение риёт его каденс). Если heartbeat не настроен — генерация/продвижение не запускаются (документировать; тихий no-op).

---

## 4. Поток данных (E2E)

```text
[heartbeat, daily_plan=true, tz-утро, нет плана на сегодня]
  → generate_day_plan(SELF.md + reflections + open_threads) → N намерений (sanitize+is_trivial)
  → agent_plans.day_plan=pending, date=today → 1 уведомление владельцу [✓Принять|✗Отклонить]
[approve (owner)]
  → tx: N × session_goals(origin=initiative, max_turns=20) + day_plan[*].session_id/active
  → day_plan_status=approved, current=0
[каждый последующий heartbeat]
  → advance_day_plan: advance_one_chunk(current намерение) ОДИН чанк → deliver → persist
  → намерение done/paused → current++
  → все done → day_plan_status=done → уведомление «план выполнен»
[новый день] → day_plan_date != today → генерация заново
```

---

## 5. Обработка ошибок (fail-soft, как весь soul-слой)

- `generate_day_plan` провал/пустой JSON → плана нет (выставить date=today, no-op).
- `day_plan_tick`/`advance_day_plan` любой провал → лог + swallow (heartbeat не рушится).
- `advance_one_chunk` наследует fail-soft driver (turn-fail → continue, judge-fail → pause).
- Approve tx падает → откат, план остаётся pending (idempotent retry).
- Отсутствие owner/heartbeat → тихий no-op (не генерируем/не продвигаем).
- session_goals намерения внешне отменена (`/goal stop`) → advance видит не-running → помечает намерение done/cancelled, идёт дальше.

---

## 6. Тестирование

- **Юнит (чистые, без БД):** парс `{"intents":[...]}` (валид/пусто); sanitize+is_trivial фильтр + cap MAX_DAY_INTENTS; `advance_day_plan`-переходы на моке (approved→advance→current++→done); `day_plan` JSONB round-trip serde; `generate_day_plan`-промпт содержит framing тредов/рефлексий.
- **advance_one_chunk рефактор — регресс:** существующие driver/decompose тесты (advance_decision, chunk-loop) остаются зелёными (поведение непрерывного цикла не изменилось).
- **sqlx (opex-db, raw INSERT/UPDATE):** `set_day_plan`/`get` round-trip; approve создаёт N session_goals + проставляет session_id (atomic); advance двигает current.
- **E2E на сервере (manual):** daily_plan-агент (soul+initiative+heartbeat+owner) → утренний heartbeat → уведомление с планом → approve → N последовательных heartbeat'ов двигают чанки (наблюдать current++ и deliver) → план done + уведомление.

---

## 7. Файловая структура (для плана)

- `migrations/081_agent_day_plan.sql` — agent_plans += day_plan/day_plan_current/day_plan_date/day_plan_status + CHECK.
- `crates/opex-db/src/agent_plans.rs` (реэкспорт) — `set_day_plan`, `get_day_plan`/расширить GoalРow-аналог, `approve_day_plan_tx` (N session_goals + пометки), `set_day_plan_status`, `advance_day_plan_pointer`.
- `crates/opex-core/src/agent/goal/driver.rs` — извлечь `advance_one_chunk` + `StepOutcome`; decompose-ветка → loop над ней.
- `crates/opex-core/src/agent/initiative/day_plan.rs` (новый) — `generate_day_plan`, `day_plan_tick`, `advance_day_plan`, промпт-билдеры (framing+re-sanitize).
- `crates/opex-core/src/agent/initiative/tick.rs` — при `daily_plan=true` пропустить single-proposal Step 2.
- `crates/opex-core/src/scheduler/mod.rs` — из heartbeat-closure вызвать `day_plan_tick` (при флаге).
- `crates/opex-core/src/gateway/handlers/agents/initiative.rs` — endpoints day/approve, day/dismiss.
- Telegram approval-callback (`dpm:` префикс) — рядом с существующим initiative-callback.
- `crates/opex-core/src/config/mod.rs` — `InitiativeConfig.daily_plan: bool`.

**Декомпозиция (~7 задач):** (1) миграция m081 + agent_plans day_plan CRUD (sqlx); (2) извлечь advance_one_chunk + StepOutcome (регресс driver); (3) generate_day_plan + промпт + чистые фильтры (юнит); (4) day_plan_tick + advance_day_plan (мок-юнит); (5) approve_day_plan_tx + endpoints (sqlx atomic); (6) Telegram dpm: callback + notify/delivery; (7) config флаг + heartbeat-хук + tick.rs skip.

---

## 8. Что дальше (вне v1)

- Day-level LLM-ре-ранк / добавление-удаление намерений в течение дня по мере прогресса.
- Carryover незавершённых намерений на следующий день.
- Авто-approve с бюджет-капом; UI-панель дневного плана (сурфейсинг current/чанков).
- Продвижение >1 чанка за тик при простое (адаптивный темп).
