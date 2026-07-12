# Инициатива — B-wide: персистентный дневной план + heartbeat-продвижение — Design Spec

**Дата:** 2026-07-13
**Статус:** проектирование (ревизия 2 — после тройного ревью: сверка-с-кодом / безопасность / полнота)
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

- `session_goals` (`crates/opex-core/src/db/session_goals.rs` — НЕ opex-db): goal_text, status, origin, subgoals JSONB, current_chunk, turn_count/max_turns, `is_running()` (=status=='active'), `budget_left()`. Reuse `upsert_initiative_goal_tx`, `get`, `set_status`, `set_current_chunk`, `set_subgoals`, `bump_turn`, `record_verdict`, `create_new_session_tx`.
- `agent/goal/driver.rs` `run_goal_driver` (стр. 21-204) — тело **обеих** веток одного хода (decompose стр. 44-134 + flat стр. 137-199 + pre-loop budget/running-check стр. 31-42) извлекается в `advance_one_chunk` (см. §3.3); `run_goal_driver` становится тонким `loop { match advance_one_chunk … }` (интерактив/cron поведение НЕ меняется — регресс-инвариант).
- `agent/goal/decompose.rs` (decompose_prompt/chunk_continuation_prompt/replan_prompt/chunk_judge_prompt/advance_decision/ChunkVerdict/DecomposeAction) — как есть.
- `agent/initiative/tick.rs` (InitiativeDeps, generate_focus, sanitize+is_trivial+cap, notify+delivery), `agent/initiative/delivery.rs` (resolve_owner_target, send_proposal_to_channel).
- `crates/opex-core/src/db/agent_plans.rs` (НЕ opex-db, НЕ реэкспорт — плоский модуль) — get_or_create (обновить SELECT + tuple-decode на новые колонки, m082-стиль как proposals_today в m077), try_set_proposal_status_tx (образец CAS-флипа). `db/memory_queries.rs` (recent_soul_chunks, recent_open_thread_chunks, latest_reflection_at — эти в opex-db, реэкспорт).
- `agent/soul/self_md.rs::render_self_block` (pub), `sanitize_soul_text` (pub), `is_trivial_goal` (`agent/initiative/mod.rs`, pub).
- `agent/initiative/tick.rs::today_in_tz` — сейчас private; поднять до `pub(crate)` для реюза из `day_plan.rs` (sibling).
- `scheduler/mod.rs` heartbeat-каденс: `add_heartbeat` closure сериализует тики агента через `agent_lock_for` + 30-мин wait — `day_plan_tick` вызывается ВНУТРИ того же guard (не отдельным spawn'ом), чтобы наследовать сериализацию (закрывает heartbeat×heartbeat гонку).
- Telegram approval-callback (`inline.rs::handle_initiative_callback`, `is_owner`-гейт, `iappr:`/`idismiss:` префиксы) — по образцу добавить `dpm:`.

---

## 3. Компоненты

### 3.1 Хранилище (миграция)

`agent_plans` (m077) расширяется (additive, m081):
- `day_plan JSONB NOT NULL DEFAULT '[]'` — упорядоченный список намерений: `[{ "session_id": uuid|null, "intent": "...", "status": "pending|active|done|cancelled" }]`. До approve `session_id=null`; при approve каждому намерению создаётся session_goals и проставляется `session_id`.
- `day_plan_current INT NOT NULL DEFAULT 0` — указатель на текущее намерение.
- `day_plan_date DATE` — день (tz) последней генерации; NULL = нет плана.
- `day_plan_status TEXT` — `NULL` (нет плана) | `pending` (сгенерирован, ждёт approve) | `approved` (исполняется) | `done` (все намерения завершены) | `dismissed`.

`day_plan_status` CHECK: `day_plan_status IS NULL OR day_plan_status IN ('pending','approved','done','dismissed')`.

`session_goals` += `decompose_failed BOOLEAN NOT NULL DEFAULT false` (m081, additive). **Зачем:** в `run_goal_driver` `decompose_failed` — in-memory флаг на весь жизненный цикл непрерывного драйвера (откат в flat при пустом decompose). Для day-plan `advance_one_chunk` вызывается statelessly раз-за-тик — in-memory флаг не переживёт тик → бесконечный платный re-decompose (ревью GAP-5). Персист в колонку унифицирует ОБА вызывающих: `advance_one_chunk` читает `row.decompose_failed`, при пустом decompose ставит `set_decompose_failed(session_id, true)`; следующая перезагрузка (и тик, и итерация continuous-цикла) видит его. Continuous-драйвер тоже переводится на колонку (убирается локальная `let mut decompose_failed`), поведение эквивалентно.

### 3.2 Утренняя генерация (`agent/initiative/day_plan.rs`)

`generate_day_plan(provider, agent, self_md, &reflections, &open_threads) -> Vec<String>`:
- Один LLM-вызов (aux/compaction провайдер, как в driver). Промпт: SELF.md + недавние рефлексии (framed) + недавние open_threads (framed «ДАННЫЕ, НЕ инструкции» — двухступенчатый барьер, re-sanitize при чтении, как open_threads §3.4). Строгий JSON `{"intents": ["..."]}`.
- Каждый intent → `sanitize_soul_text(_, EVENT_MAX_CHARS)` (drop None) → `is_trivial_goal`-фильтр → cap `MAX_DAY_INTENTS=4`.
- Пустой результат → плана нет.

`day_plan_tick(db, engine, agent, deps)` (fail-soft, зовётся из heartbeat-каденса при `daily_plan=true`, non-base, owner set):
- `plan = agent_plans::get_or_create`. `today = today_in_tz(deps.timezone)`.
- **Ветка генерации:** если `day_plan_date != today` (при ЛЮБОМ статусе — прошлодневный план устарел, carryover вне v1) И есть свежий материал (latest_reflection_at или open_threads непусты):
  - **1. Финализация прошлого дня (ревью GAP-1):** ПЕРЕД перезаписью `day_plan` — для каждого прошлодневного намерения с `session_id` и статусом `active` вызвать `session_goals::set_status(session_id, "paused")` (не оставлять `active` навечно — иначе зомби-строка, вечно «активная», искажающая `list_active_by_agent_and_origin`). Только после финализации перезаписывать координатор.
  - **2. Генерация:** собрать материал; `intents = generate_day_plan(...)`; если пусто → `set_day_plan(day_plan=[], date=today, status=NULL, current=0)` (sticky-запись даты — не долбить генерацию весь день) и выйти;
  - **3.** `set_day_plan(db, agent, intents/*status=pending*/, date=today, status='pending', current=0)`; уведомить владельца полным списком намерений (см. §3.4).
- **Ветка продвижения:** иначе если `day_plan_date == today` И `day_plan_status='approved'` → `advance_day_plan(...)` (см. §3.3).
- Иначе (`date==today` И статус pending — ждём approve; done/dismissed/NULL сегодня) → no-op.

> **Сериализация (ревью GAP-3):** `day_plan_tick` вызывается ВНУТРИ heartbeat agent-lock guard (§2), поэтому два тика одного агента не гоняются. Гонка approve (HTTP) × генерация (heartbeat) закрывается CAS в approve (§3.4).
> **Инвариант дня:** генерация гейтится ТОЛЬКО по `day_plan_date != today` — прошлодневный `pending`/`approved` на новый день перегенерируется (после финализации active-намерений шага 1), не блокируя план навсегда.
> **Каденс (ревью AMBIGUITY):** «утро» = первый heartbeat нового календарного дня (tz). Рекомендуемый heartbeat для `daily_plan` — достаточно частый (напр. ежечасный), чтобы (а) генерация попала близко к утру и (б) за день реально продвинулось несколько чанков (по 1/тик). Документируется; редкий heartbeat = поздний план + мало прогресса (не баг, ожидаемо).

### 3.3 `advance_one_chunk` + продвижение плана

**Извлечение (self-contained — ревью B1/B2/GAP-4):** `advance_one_chunk` делает РОВНО один ход цели (decompose-aware) и возвращает исход. Сам грузит row и делает pre-check (это стр. 31-42 driver, ВНЕ decompose-блока — их тоже вносим):
```rust
enum StepOutcome { Continuing, Done, Paused }
async fn advance_one_chunk(
    engine: &AgentEngine,
    session_id: Uuid,
    target: &GoalTarget,
    cancel: &CancellationToken,   // ревью B2: cancel — реальный вход в run_goal_turn
) -> StepOutcome
```
Тело (1:1 с одной итерацией текущего цикла):
1. `row = get(session_id)`; если нет/`!is_running()` → `StepOutcome::Done` (нечего двигать).
2. `!budget_left()` → `set_status(paused)` + deliver «budget» → `StepOutcome::Paused` (был pre-loop guard стр. 37-42).
3. `is_decompose = row.origin=='initiative' && (cfg.decompose || cfg.daily_plan) && !row.decompose_failed` (ревью UNDERSPECIFIED: `daily_plan` ПОДРАЗУМЕВАЕТ decompose — намерения дня всегда декомпозируются; см. §3.5).
4. Decompose-ветка: lazy-decompose (subgoals пусты) → если пусто `set_decompose_failed(true)` + `StepOutcome::Continuing` (следующий вызов пойдёт flat); иначе chunk-ход → deliver → chunk_judge → record_verdict → `advance_decision`.
   Flat-ветка (не decompose): continuation-ход → deliver → judge → record_verdict → `next_action`.
5. **Маппинг `DecomposeAction`/`DriverAction` → `StepOutcome` (ревью AMBIGUITY, зафиксировать 1:1):**
   `Continue → Continuing`; `Advance → Continuing`; `AdvanceAndReplan → Continuing` (после set_current_chunk/replan); `Done → set_status(done)+deliver → Done`; `Pause(_) → set_status(paused)+deliver → Paused`. Lazy-decompose-итерация (subgoals только что записаны) → `Continuing` без прогона хода.
- `run_goal_driver` → тонкий `loop { if cancel { break } match advance_one_chunk(&engine, sid, &target, &cancel).await { Continuing => continue, Done|Paused => break } }`. Локальная `decompose_failed` УДАЛЯЕТСЯ (теперь колонка). Поведение непрерывных целей эквивалентно (регресс-инвариант) — покрывается характеризационными тестами (§6).

`advance_day_plan(db, engine, agent, deps)` (ровно один `advance_one_chunk` за тик):
- `plan = get`. `cur = day_plan_current`. Если `cur >= len(day_plan)` → `day_plan_status='done'` + уведомить «план на день выполнен», выход.
- `intent = day_plan[cur]`. Три ветки по текущему намерению:
  - **нет `session_id`** (не должно при approved) → пометить `done`, `current++` (защитно).
  - **`session_id` есть, session_goals НЕ `is_running()`** (внешне отменён/уже done/paused — ревью GAP-6) → пометить намерение `done`, `current++`. БЕЗ вызова advance_one_chunk. (Закрывает «застревание после /goal stop».)
  - **`session_id` есть И `is_running()`** → `outcome = advance_one_chunk(&engine, sid, &target, &CancellationToken::new()).await` ОДИН раз. `outcome==Done|Paused` → пометить намерение `done`, `current++`; `Continuing` → оставить `current` (продолжим на след. тике).
- Пойнтер-логика (переходы current/status по outcome + is_running) выносится в ЧИСТУЮ `advance_pointer(day_plan, current, outcome, running) -> (day_plan, current, new_status)` — тестируется без БД/LLM (как `advance_decision`, ревью §6). IO-обёртка `advance_day_plan` зовёт её + персистит.
- `cancel` для day-plan — свежий `CancellationToken::new()` за тик (нет /goal stop-пути; внешняя отмена = флип статуса, виден ветке «не is_running» на след. тике). `GoalTarget` = resolve_owner_target (CONFIG owner_id).

### 3.4 Approve/dismiss (API + Telegram)

- Новый endpoint `POST /api/agents/{name}/plan/day/approve` и `.../day/dismiss` (по образцу proposal approve; non-base gate).
- `approve_day_plan_tx(db, engine, agent)` — атомарная tx с **CAS-guard (ревью GAP-2/MEDIUM)**:
  - `UPDATE agent_plans SET day_plan_status='approved' WHERE agent_id=$1 AND day_plan_status='pending'` — если 0 affected rows → идемпотентный no-op, откат, НЕ создавать session_goals (двойной клик / гонка approve×regenerate безопасны, как `try_set_proposal_status_tx`).
  - **N=0 guard (ревью GAP):** если `day_plan` пуст → откат в `day_plan_status=NULL` (или `dismissed`), НЕ выставлять `approved` с 0 целями.
  - иначе для каждого pending-намерения: `create_new_session_tx` + `upsert_initiative_goal_tx` (`origin='initiative'`, max_turns=INITIATIVE_GOAL_MAX_TURNS=20) + проставить `session_id`+`status='active'` в `day_plan` JSONB; `current=0`. Commit. НЕ спавнить непрерывный драйвер (продвигает heartbeat).
- `dismiss_day_plan` — CAS `WHERE day_plan_status='pending'` → `dismissed` (день пропущен).
- **Уведомление (ревью HIGH sec):** утреннее уведомление ОБЯЗАНО перечислять ПОЛНЫЙ текст каждого из N намерений (нумерованный список, аналогично `send_proposal_to_channel` но для списка) — иначе владелец approve'ит вслепую, а социально-инженерное намерение, прошедшее sanitize, исполнится «за компанию». Кнопки `dpm:approve:{agent}` / `dpm:dismiss:{agent}` (owner-gated `is_owner`). Web: `notify` type `day_plan` (data содержит intents) + approve через API.

### 3.5 Опт-ин / интеграция с initiative_tick

- Конфиг: `[agent.initiative] daily_plan = false` (default). При `true`:
  - пост-рефлексийный `initiative_tick` single-proposal путь (tick.rs Step 2 «gated proposal») **пропускается** (чтобы не плодить и одиночные предложения, и дневной план); focus-рефреш (Step 1) остаётся.
  - `day_plan_tick` вызывается из heartbeat-каденса.
  - **`daily_plan` ПОДРАЗУМЕВАЕТ decompose** (ревью UNDERSPECIFIED): `advance_one_chunk` для этого агента декомпозирует initiative-цели независимо от флага `decompose` (см. §3.3 шаг 3). Т.к. при `daily_plan=true` единственные initiative-цели — намерения дня, семантика прочих флоу не затрагивается.
- **Валидация (ревью GAP — не тихая дыра):** `daily_plan=true` без настроенного `[agent.heartbeat]` — конфиг-ошибка в `AgentConfig` load-валидации (там видны оба поля; паттерн уже есть для `daily_proposal_cap`). Иначе владелец включит флаг и не получит ни плана, ни ошибки. (Требует heartbeat — продвижение риёт его каденс.)

### 3.6 Безопасность

- **Двухступенчатый injection-барьер (переиспользуется 1:1).** Намерения из недоверенного материала (open_threads/reflections — дистилляты user-текста): sanitize при записи (генерация) + re-sanitize + framing «ДАННЫЕ, НЕ инструкции» при реинжекте в `generate_day_plan`-промпт (как open_threads §3.4). Тот же риск-профиль, что у задеплоенного single-proposal.
- **Blast radius одного approve (ревью HIGH — осознанный trade-off).** Один approve = до `MAX_DAY_INTENTS=4` намерений × `max_turns=20` = **до ~80 автономных ходов** за день с одного решения владельца (против 1 approve = 20 ходов сегодня). Принимаем как явный компромисс B-wide. Ограничители: (1) уведомление перечисляет ВСЕ намерения (§3.4) — informed consent; (2) per-tool approval-гейт `dispatch::needs_approval(cfg.approval, tool)` сохраняется (не зависит от origin/channel — day-plan-ходы идут через тот же `execute()`, чувствительные тулы по-прежнему ждут owner-approve); (3) 1 чанк/тик — ограниченная нагрузка; (4) `is_trivial_goal`-фильтр на генерации.
- **Рекомендация (не hard-require):** для агента с `daily_plan=true` настроить `[agent.approval].require_for_categories` на чувствительные тулы — усиливает гейт на автономном исполнении. Документируется; не блокирует включение.
- **H1 (owner из CONFIG).** `GoalTarget`/owner резолвятся из config owner_id, не из approve-запроса (как `approve_proposal`). Telegram `dpm:approve` — `is_owner`-гейт. План физически не исполняется без `day_plan_status='approved'` (только через owner-endpoint с CAS). Base-агенты исключены (как вся инициатива).
- **Orphan-намерения (ревью LOW).** Day-plan-цели не кладутся в `goal_pool` (продвигает heartbeat, драйвер не спавнится). Generic cancel флипает БД-статус → advance видит «не is_running» (§3.3 GAP-6 ветка) → помечает done, идёт дальше. Незакрытые прошлодневные намерения финализируются в `paused` на rollover (§3.2 шаг 1) — не зомби. Ожидаемое поведение, задокументировано.

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

- `generate_day_plan` провал/пустой JSON → плана нет (sticky-запись date=today, status=NULL, no-op).
- `day_plan_tick`/`advance_day_plan` любой провал → лог + swallow (heartbeat не рушится).
- `advance_one_chunk` наследует fail-soft driver (turn-fail → continue, judge-fail → pause). Пустой decompose → `set_decompose_failed(true)` → следующий тик идёт flat (не бесконечный re-decompose — GAP-5).
- Approve tx падает → откат (CAS не сработал или ошибка), план остаётся pending, idempotent retry безопасен.
- Отсутствие owner → тихий no-op. Отсутствие heartbeat → конфиг-ошибка на load (§3.5), не тихо.
- Намерение внешне отменено/приостановлено → `advance_day_plan` видит `!is_running()` (§3.3 ветка GAP-6) → помечает намерение done, `current++`, план не застревает.
- Смена дня во время исполнения → финализация active-намерений в `paused` перед перегенерацией (§3.2 шаг 1) — не active-зомби.

---

## 6. Тестирование

- **Юнит (чистые, без БД):** парс `{"intents":[...]}` (валид/пусто); sanitize+is_trivial фильтр + cap MAX_DAY_INTENTS; **чистая `advance_pointer(day_plan,current,outcome,running)` — все переходы** (Continuing→current стоит; Done/Paused→current++; не-running→current++/skip; N=0/конец→status=done); `DecomposeAction/DriverAction→StepOutcome` маппинг; `day_plan` JSONB round-trip serde; `generate_day_plan`-промпт содержит framing тредов/рефлексий.
- **advance_one_chunk рефактор — характеризация (ревью §6):** driver.rs НЕ имеет тестов цикла; существующие тесты покрывают только чистые `next_action`/`advance_decision`/`parse_chunk_verdict`. Task 2 ОБЯЗАН: (а) сохранить эти чистые тесты зелёными; (б) вынести маппинг `DecomposeAction→StepOutcome` в чистую функцию и покрыть её (сетка безопасности рефактора — TDD, `feedback_tdd`). Сам IO-цикл проверяется на сервере E2E (обычные `/goal`-флоу интерактив/cron не сломаны).
- **sqlx (opex-core `#[sqlx::test]`, raw INSERT/UPDATE):** `agent_plans` day_plan set/get round-trip + decode новых колонок в get_or_create; `approve_day_plan_tx` CAS (pending→approved один раз, повторный вызов no-op) + создаёт N session_goals + проставляет session_id (atomic); N=0 → no-op; `set_decompose_failed` персистит.
- **E2E на сервере (manual):** daily_plan-агент (soul+initiative+heartbeat+owner) → утренний heartbeat → уведомление со ВСЕМИ намерениями → approve → N последовательных heartbeat'ов двигают чанки (наблюдать current++ и deliver) → план done + уведомление; + проверить: смена дня финализирует незакрытое намерение в paused (не active-зомби).

---

## 7. Файловая структура (для плана)

- `migrations/081_agent_day_plan.sql` — agent_plans += day_plan/day_plan_current/day_plan_date/day_plan_status + CHECK; session_goals += `decompose_failed BOOLEAN DEFAULT false` (additive).
- `crates/opex-core/src/db/agent_plans.rs` (плоский модуль, НЕ opex-db) — `set_day_plan`, get_or_create decode новых колонок, `approve_day_plan_tx` (CAS + N session_goals), `set_day_plan_status`, `advance_day_plan_pointer_row`. `crates/opex-core/src/db/session_goals.rs` — `set_decompose_failed`.
- `crates/opex-core/src/agent/goal/driver.rs` — извлечь `advance_one_chunk(engine, sid, target, cancel) -> StepOutcome` (обе ветки + pre-check) + чистый маппинг; `run_goal_driver` → тонкий loop; убрать локальную decompose_failed (→ колонка).
- `crates/opex-core/src/agent/initiative/day_plan.rs` (новый) — `generate_day_plan`, `day_plan_tick` (генерация+финализация прошлого дня+продвижение), `advance_day_plan`, чистая `advance_pointer`, промпт-билдеры (framing+re-sanitize).
- `crates/opex-core/src/agent/initiative/tick.rs` — при `daily_plan=true` пропустить single-proposal Step 2; `today_in_tz` → `pub(crate)`.
- `crates/opex-core/src/scheduler/mod.rs` — из heartbeat-closure (ВНУТРИ agent-lock guard) вызвать `day_plan_tick` (при флаге).
- `crates/opex-core/src/gateway/handlers/agents/initiative.rs` — endpoints day/approve, day/dismiss.
- Telegram: `inline.rs::handle_initiative_callback` += `dpm:` префикс; `delivery.rs` + `channels/src/drivers/telegram.ts` — day_plan ChannelAction со списком намерений.
- `crates/opex-core/src/config/mod.rs` — `InitiativeConfig.daily_plan: bool` + cross-field валидация (`daily_plan` требует heartbeat).

**Декомпозиция (~8 задач, порядок 1 → 2 ∥ 3 → 4 → 5 → 6 → 7 → 8):**
1. Миграция m081 (agent_plans day_plan колонки + session_goals.decompose_failed) + agent_plans day_plan CRUD + `set_decompose_failed` (sqlx).
2. Извлечь `advance_one_chunk` + `StepOutcome` + чистый маппинг из driver (характеризация чистой части; убрать локальный decompose_failed → колонка). Независим от 1 по коду, НО использует колонку decompose_failed (зависит от миграции 1 для рантайма — в тестах колонка есть после m081).
3. `generate_day_plan` + промпт-билдеры + чистые фильтры sanitize/is_trivial/cap (юнит). Независим от 2.
4. `day_plan_tick` + `advance_day_plan` + чистая `advance_pointer` (мок/юнит + финализация прошлого дня GAP-1 + ветки GAP-6). Зависит от 2 (StepOutcome) + 1 (колонки).
5. `approve_day_plan_tx` (CAS + N session_goals + N=0) + endpoints day/approve,dismiss (sqlx atomic). Зависит от 1.
6. Telegram `dpm:` callback + notify/delivery со списком намерений. Зависит от 5.
7. config `daily_plan` + cross-field валидация + tick.rs skip Step 2 + `today_in_tz` pub(crate).
8. heartbeat-хук: вызов `day_plan_tick` из scheduler под agent-lock. Зависит от 4, 7.

---

## 8. Что дальше (вне v1)

- Day-level LLM-ре-ранк / добавление-удаление намерений в течение дня по мере прогресса.
- Carryover незавершённых намерений на следующий день.
- Авто-approve с бюджет-капом; UI-панель дневного плана (сурфейсинг current/чанков).
- Продвижение >1 чанка за тик при простое (адаптивный темп).
