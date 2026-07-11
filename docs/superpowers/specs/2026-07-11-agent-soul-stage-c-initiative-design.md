# Этап C «Инициатива» — Design Spec

**Дата:** 2026-07-11
**Статус:** проектирование (ревизия 2 — после тройного ревью: сверка-с-кодом / безопасность / полнота)
**Предшественники (задеплоены на прод):** этап A (soul-фундамент: memory stream + рефлексия + SELF.md), этап B (анти-дрифт-детектор, detect-only).
**База:** `docs/research/2026-07-09-agent-soul-research.md` (находки 4 «инициатива = plan-decompose-react» и 289 «рефлексии как источник целей»); `docs/superpowers/specs/2026-07-09-agent-soul-foundation-design.md`.

---

## 1. Цель и не-цели

**Цель:** дать non-base агентам минимальную, безопасную **инициативу** — способность из собственной души (рефлексии + SELF.md) сформулировать конкретную цель, персистентно её удержать и **предложить** владельцу; при одобрении — реально её преследовать, переиспользуя существующий `/goal`-движок.

**Ключевой инвариант (gated v1):** агент НИКОГДА не запускает автономную работу без явного одобрения владельца через HTTP-эндпоинт. Инициатива = «предложить + при approve исполнить», не «действовать самому». Прямое продолжение консервативной линии этапов A (opt-in) и B (detect-only).

**Не-цели v1 (отложено в фазу 2+):**
- Полное автономное дневное планирование Smallville-стиля (дневной→часовой→чанк декомпозиция без гейта).
- Реактивное перепланирование остатка плана.
- Telegram inline-кнопка одобрения (v1: UI + HTTP-эндпоинт; текст-уведомление в Telegram можно, но callback-кнопка позже).
- Durable re-drive инициативной цели после краха (см. §3.4 — в v1 крашнутая цель не воскрешается).
- Доставка результата цели в канал (v1: `GoalTarget=None`, доставка в web/уведомления).
- Источники целей помимо рефлексий/SELF.md (открытые треды, wishlist владельца).
- `[agent.initiative]` для **base**-агентов (см. §3.6 — для них гейт декоративен, т.к. они admin-эквивалентны).

---

## 2. Что переиспользуется (не переписываем)

- `session_goals` (m056/m057/m058): `goal_text`, `status`, `turn_count`/`max_turns`, `subgoals` JSONB, `last_verdict`, `origin`, `next_redrive_at`, `cron_job_id`. Строка ссылается на `sessions(id) ON DELETE CASCADE` (PK = `session_id`) — инициативной цели ОБЯЗАТЕЛЬНО нужна свежесозданная `sessions`-строка (как у cron).
- `agent/goal/{driver,pool}.rs` — фоновый цикл (ход → доставка → судья → бюджет → отмена); `GoalTarget = Option<(String /*channel*/, i64 /*chat_id*/)>`; `spawn_goal_driver`.
- **Cron-путь как образец** (`scheduler/mod.rs::bootstrap_cron_goal`): create_new_session → upsert `session_goals` с `origin` → `spawn_goal_driver` → вставка в pool. Это точная механическая форма, которую копирует approve-эндпоинт.
- `notify(db, ui_event_tx, type, title, body, data) -> Result<()>` (`gateway/handlers/notifications.rs:148`); таблица `notifications.type TEXT` без CHECK — новый тип `'initiative_proposal'` не требует правки схемы.
- Soul: `agent/soul/reflection.rs::maybe_reflect` (вызывается из `knowledge_extractor.rs`, гейтится `soul_deps.cfg.enabled`), `self_md.rs::render_self_block` (framing + пофразовый `sanitize_soul_text`), `sanitize.rs::sanitize_soul_text`, `memory_queries::latest_reflection_at`.
- Raw-LLM путь рефлексии: `reflection.rs::llm_text` (`tokio::time::timeout(60s, provider.chat(...))`) + `json_repair::repair_json` для парса JSON-контракта. Переиспользуется инициативой.
- Config-паттерн `DriftConfig`/`SoulConfig` (serde default + `validate()` из `AgentConfig::load()`; 3 breaking-литерала `AgentSettings{}` — `config/mod.rs:2419`, `:2490`, `gateway/handlers/agents/schema.rs:196`).

---

## 3. Архитектура и компоненты

### 3.1 Данные — таблица `agent_plans` (migration 077, additive)

Один персистентный, кросс-сессионный объект на агента.

```sql
CREATE TABLE agent_plans (
    agent_id         TEXT PRIMARY KEY,
    current_focus    TEXT,
    proposals        JSONB NOT NULL DEFAULT '[]',
    last_proposal_at TIMESTAMPTZ,
    proposals_today  INT  NOT NULL DEFAULT 0,
    proposal_day     DATE,
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);
```

`proposals[]` элемент: `{ "id": uuid, "text": "…", "status": "pending"|"approved"|"dismissed", "created_at": ts, "acted_at": ts|null }`.

**Расширение CHECK** (той же миграцией) — констрейнт `session_goals.origin` авто-именован Postgres `session_goals_origin_check`; механизм drop+add (образец `062_uploads_relax_cap.sql`):
```sql
ALTER TABLE session_goals DROP CONSTRAINT session_goals_origin_check;
ALTER TABLE session_goals ADD CONSTRAINT session_goals_origin_check
    CHECK (origin IN ('goal','cron','initiative'));
```

**Rename/delete-гигиена (ревью I1):** `agent_plans` keyed по имени агента (не через `sessions`). Добавить `agent_plans` в rename-транзакцию (`gateway/handlers/agents/crud.rs`, рядом с `agent_channels.agent_name`-обработкой — это НЕ nullable-`agent_id`-паттерн, а прямой UPDATE имени) и в delete-очистку. Иначе строка осиротеет при переименовании.

### 3.2 Конфиг — `[agent.initiative]` (opt-in, non-base only)

```toml
[agent.initiative]
enabled = false           # по умолчанию выключено (как soul/drift)
daily_proposal_cap = 1    # максимум предложений в сутки
```

`InitiativeConfig { enabled: bool, daily_proposal_cap: u32 }`, serde default; `validate()`: `daily_proposal_cap ∈ [1, 10]`. Поле `initiative` на `AgentSettings`; `AgentConfig::load()` зовёт `validate()`; правятся 3 breaking-литерала.

**Жёсткие предусловия рантайма (ревью I2, HIGH-1):**
- Инициатива работает ТОЛЬКО если `soul.enabled = true` (хук стоит после `maybe_reflect`, который сам гейтится soul). Если `initiative.enabled=true` при `soul.enabled=false` — логировать `warn` один раз и быть no-op.
- Инициатива работает ТОЛЬКО для non-base агентов. Для `base=true` — движок и эндпоинты no-op/refuse (base admin-эквивалентен: хост, полный FS, self-drive через heartbeat уже сейчас — гейт для него не имеет смысла).
- Требует заданного `agent.access.owner_id` (некому одобрять — предложения бессмысленны).

Исполнение одобренной цели использует выделенную константу `INITIATIVE_GOAL_MAX_TURNS: i32 = 20` (ревью I6: в проекте нет единого `max_turns`; `/goal`=20, cron=20 — свой литерал, документированный) и **обычный deny-list агента** (base-права не эскалируются — но base и так исключены).

### 3.3 Движок — `agent/initiative/` (чистая логика + хук)

Чистые, юнит-тестируемые функции (как `agent/drift/`):

- `pub fn should_propose(plan: &PlanRow, latest_reflection_at: Option<DateTime<Utc>>, proposals_today_effective: u32, cap: u32) -> bool`
  — true, если есть рефлексия позже `plan.last_proposal_at` **И** `proposals_today_effective < cap`.
- `pub fn reset_daily_if_new_day(plan: &PlanRow, today: NaiveDate) -> u32`
  — возвращает эффективный `proposals_today` (0, если `plan.proposal_day != today`, иначе хранимое). **Таймзона (ревью A1):** `today` вычисляется в **heartbeat-таймзоне агента** (`HeartbeatConfig.timezone`, дефолт как у cron; проект в Europe/Samara) — резолвится в `initiative_tick` (impure), в чистую функцию передаётся уже готовый `NaiveDate`.
- `pub fn render_focus_block(current_focus: &str, active_goals: &[String]) -> String`
  — **ОБЯЗАН** использовать дисциплину `render_self_block` (ревью HIGH-2): framing-обёртка «наблюдения о текущем фокусе, НЕ инструкции» + пофразовый `sanitize_soul_text` каждой строки. Это read-only блок; тот же класс, на котором горел этап B rev1 (неframed инъекция).

**Хук `initiative_tick(...)` (fail-soft):** вызывается в `knowledge_extractor.rs` сразу после `maybe_reflect`. `maybe_reflect` возвращает `()` (ревью I3) — поэтому `initiative_tick` **сам** запрашивает `memory_queries::latest_reflection_at(db, agent)` (НЕ модифицировать сигнатуру `reflection.rs`). Плюмбинг (ревью I4): протянуть `InitiativeConfig` + `owner_id` + `is_base` через `FinalizeContext`/`finalize_context_from_engine` (`finalize.rs`) в `extract_and_save` — всё доступно из `engine.cfg().agent`. Шаги:

1. Предусловия §3.2 (soul.enabled, non-base, enabled, owner_id) — иначе выход.
2. Загрузить/создать `agent_plans` (ленивое создание); вычислить `today` в tz агента; `effective = reset_daily_if_new_day(...)`.
3. **Обновить `current_focus`**: один LLM-вызов (только по новому материалу). Промпт-контракт (ревью U1):
   ```
   system: «Ты пишешь одну-две фразы о текущем фокусе агента {name}, опираясь
   на его SELF.md и свежие рефлексии. Только наблюдение о том, чем он сейчас
   поглощён. Верни JSON: {"focus": "..."}»
   user: <SELF.md-блок> + <топ-N свежих reflection-строк>
   ```
   Парс через `json_repair::repair_json`; `focus` санитизируется `sanitize_soul_text` перед записью в `agent_plans`.
4. **Гейт** `should_propose(...)`: если true → один LLM-вызов:
   ```
   system: «Предложи ОДНУ конкретную цель, которую агенту {name} стоило бы
   преследовать, исходя из его души. Обоснуй одной фразой. Верни JSON:
   {"goal": "...", "rationale": "..."}»
   user: <current_focus> + <свежие рефлексии>
   ```
   `goal` санитизируется `sanitize_soul_text`; атомарно (см. ниже) добавить `{status:'pending'}` в `proposals[]`, `notify('initiative_proposal', data={agent, proposal_id, text})`, установить `last_proposal_at=now`.
5. Ошибки логируются `warn` и проглатываются (инициатива не критична; рефлексия/extraction не затрагиваются).

**Атомарность (ревью HIGH-3/A2):** и инкремент счётчика (шаг 4), и flip статуса в approve/dismiss (§3.4) — конкурентны (два тика из параллельно финиширующих сессий; тик vs approve). JSONB-мутация — whole-array replace в Rust (образец `session_goals::set_subgoals`), НО обёрнутая в атомарный условный `UPDATE agent_plans SET ... WHERE agent_id=$1 AND proposal_day=$today AND proposals_today < $cap RETURNING` (счётчик) — предложение генерится/пишется только если UPDATE затронул строку. Для строки использовать `SELECT … FOR UPDATE` на время read-modify-write, чтобы тик не перезаписал одобренный статус устаревшей копией массива.

### 3.4 Гейт → одобрение → исполнение

- **Уведомление:** `notify(type='initiative_proposal', data={agent, proposal_id, text})` — колокольчик + WS. Опц. текст владельцу через `/api/channels/notify` (без inline-кнопки в v1).
- **Эндпоинты** — новый под-роутер `gateway/handlers/agents/initiative.rs` (merge через `mod.rs`, как `crud`/`lifecycle`):
  - `GET /api/agents/{name}/plan` — current_focus + proposals + активные initiative-цели.
  - `POST /api/agents/{name}/plan/proposals/{id}/approve`.
  - `POST /api/agents/{name}/plan/proposals/{id}/dismiss`.
- **Валидация (ревью MED-2):** `{name}` через `validate_agent_name` + `agents.map.contains_key`; `{id}` — parse UUID; изменение статуса разрешено ТОЛЬКО из `pending`.
- **approve — серверный резолв текста (ревью MED-1):** `goal_text` берётся ИСКЛЮЧИТЕЛЬНО из хранимого `proposals[id].text`; любой `text`/`goal_text` в теле запроса ИГНОРИРУЕТСЯ (иначе подмена автономной цели мимо показанного человеку).
- **approve — исполнение (зеркало `bootstrap_cron_goal`, ревью G2/G3):**
  1. Атомарный `UPDATE`: перевести proposal `pending→approved` только если ещё `pending` (RETURNING; пусто → идемпотентный no-op, без двойного spawn).
  2. `create_new_session(db, agent_name, "system", channel)` → новая `sessions`-строка.
  3. Вставить `session_goals(session_id, goal_text=proposal.text, origin='initiative', max_turns=INITIATIVE_GOAL_MAX_TURNS)`.
  4. `spawn_goal_driver(engine, session_id, GoalTarget::None)` + вставка в pool.
  - **`GoalTarget=None` в v1 (ревью B2/G3):** резолвера `owner_id→(channel,chat_id)` в проекте нет, а cron-crash-путь сам спавнит с `None`. Результат цели виден в web/уведомлениях; доставка в канал — фаза 2.
- **Durable re-drive НЕ поддерживается в v1 (ревью B1/G1):** `list_redrivable` хардкодит `origin='cron'`; `origin='initiative'` туда НЕ добавляется. Крашнутая инициативная цель просто останавливается (как интерактивный `origin='goal'`); владелец пере-одобряет. Не трогаем `list_redrivable`/`resume_autonomous_goals`.
- **Доверие (ревью MED-3):** web-auth = единый admin-токен, без per-user принципала. Любой держатель токена одобряет предложение любого агента; «владелец» (channel `owner_id`) на HTTP-границе НЕ сверяется. Это не новый IDOR (весь API таков), но зафиксировано как ограничение модели доверия; настоящий per-principal гейт = Telegram owner-callback (фаза 2).

### 3.5 Сурфейсинг в контекст

**Отдельный trait-метод** `initiative_block(&self, agent) -> Option<String>` в `ContextBuilderDeps` (ревью A3: как `session_todo_block`/`drift_probe`, НЕ графтить в кортеж `soul_blocks` — иначе ломаются call-сайты). Trait-метод в `agent/context_builder.rs`, DB-реализация в `agent/engine/context_builder.rs`. Активные цели тянутся новой функцией `db::session_goals::list_active_by_agent_and_origin(db, agent_id, "initiative")` (join через `sessions.agent_id`, ревью G2 — сейчас такой функции нет). Блок вставляется в `system_prompt` после soul `self_block`. Read-only; агент не пишет план тулами.

### 3.6 Безопасность (сводка)

- **Только non-base + soul.enabled + enabled + owner_id** (§3.2). Для base — no-op (HIGH-1: иначе гейт декоративен, т.к. base читает `.env` на хосте и сам вызывает approve; но base и так admin-эквивалентен — новой поверхности нет).
- **Санитайз выхода LLM (HIGH-2):** `current_focus` и `goal_text` → `sanitize_soul_text` перед записью; `render_focus_block` → framing + пофразовый sanitize.
- **Атомарность (HIGH-3):** условные `UPDATE ... WHERE` для cap и статуса; spawn только при затронутой строке.
- **approve резолвит текст серверно** (MED-1); **валидация name/id** (MED-2); **plan мутирует только движок + эндпоинты**, не `workspace_write` (это таблица, тулов к ней нет).
- **Non-base не может само-одобриться** (подтверждено ревью: sandbox сетево-изолирован, токена в контейнере нет, `.env` не смонтирован, host-секреты вырезаются `strip_host_secrets`).
- **Автономный ход под deny-list + fail-closed approval** (подтверждено: `run_isolated_pipeline` не эскалирует, approval-таймаут → reject).
- **Доверие HTTP-approve** = admin-токен (MED-3, задокументировано).

---

## 4. Поток данных (E2E)

```
завершённая сессия → knowledge_extractor → maybe_reflect() пишет reflection
   └─→ initiative_tick() (non-base, soul+initiative enabled, owner_id):
        latest_reflection_at() (сам запрашивает)
        refresh current_focus (LLM, sanitize)         → agent_plans.current_focus
        should_propose()? → generate goal (LLM, sanitize) → атомарный UPDATE proposals[] += {pending}
                          → notify('initiative_proposal')   → колокольчик/Telegram-текст
владелец: GET /plan → approve → атомарный pending→approved
   └─→ create_new_session → session_goals(origin='initiative') → spawn_goal_driver(None)
        → автономные ходы (deny-list, INITIATIVE_GOAL_MAX_TURNS) → judge done/continue → done (результат в web)
контекст будущих сессий агента: initiative_block (focus + активные цели, read-only, framed+sanitized)
```

---

## 5. Обработка ошибок

- `initiative_tick` fail-soft: любая ошибка → `warn` + проглотить; рефлексия/extraction целы.
- LLM focus/proposal с таймаутом (как `reflection.rs`); провал → не обновляем в этот раз.
- approve при не-`pending` proposal → идемпотентный no-op (атомарный UPDATE вернёт пусто → без двойного spawn).
- Отсутствие `agent_plans` строки → ленивое создание.
- Смена суток (в tz агента) → `reset_daily_if_new_day` обнуляет счётчик.
- Гонки cap/approve → атомарные условные UPDATE (§3.3).

---

## 6. Тестирование

- **Юнит (чистые функции):** `should_propose` (нет нового материала / cap исчерпан / оба ок), `reset_daily_if_new_day` (тот же день / новый день / NULL day), `render_focus_block` (пустой / с целями / **инъекция в focus вырезается sanitize** — как soul-тесты).
- **Интеграция:** approve создаёт `session_goals(origin='initiative')`, атомарность не даёт двойного spawn при конкурентном approve; dismiss меняет статус; approve игнорирует body-text; валидация name/id; base-агент → refuse.
- **E2E на сервере** (Windows не гоняет эти Rust-тесты — bin-таргет): включить `[agent.initiative]` + `[agent.soul]` на одном non-base агенте с `owner_id`; породить рефлексии (порог рефлексии — см. этап A spec §3, `reflection_threshold`); наблюдать `agent_plans` (current_focus заполнен), `initiative_proposal`-уведомление; approve → goal-driver дошёл до `done` (`session_goals origin='initiative'`).

---

## 7. Файловая структура (для плана реализации)

- `migrations/077_agent_plans.sql` — таблица + drop+add CHECK `session_goals_origin_check`.
- `crates/opex-core/src/config/mod.rs` — `InitiativeConfig` + `validate()` + поле на `AgentSettings` + 3 литерала.
- `crates/opex-core/src/agent/initiative/mod.rs` — чистые функции (`should_propose`/`reset_daily_if_new_day`/`render_focus_block`) + `initiative_tick`.
- `crates/opex-core/src/db/agent_plans.rs` — CRUD + атомарные условные UPDATE (счётчик/статус).
- `crates/opex-core/src/db/session_goals.rs` — `list_active_by_agent_and_origin` + конструктор initiative-цели.
- `crates/opex-core/src/agent/knowledge_extractor.rs` + `crates/opex-core/src/agent/pipeline/finalize.rs` — плюмбинг `InitiativeConfig`/`owner_id`/`is_base` + вызов `initiative_tick`.
- `crates/opex-core/src/agent/context_builder.rs` (+ `engine/context_builder.rs`) — `initiative_block` trait-метод + реализация.
- `crates/opex-core/src/gateway/handlers/agents/initiative.rs` (новый под-роутер) + merge в `mod.rs`; `notify` тип `initiative_proposal`.
- `crates/opex-core/src/gateway/handlers/agents/crud.rs` — `agent_plans` в rename/delete.
- **UI (отдельная задача, ревью U3):** тип `initiative_proposal` в `ui/src/types/api.ts` И `ui/src/types/ws.ts`; клик-навигация в `notification-bell.tsx` + `notification-store.ts`; страница/вкладка плана (current_focus + proposals + approve/dismiss) + API-клиент (GET plan / approve / dismiss) + nav-вход.

**Предлагаемая декомпозиция (8 задач):** (1) миграция 077; (2) InitiativeConfig; (3) db/agent_plans CRUD + атомарика + list_active_by_agent_and_origin; (4) чистые функции initiative/; (5) initiative_tick + LLM-промпты + плюмбинг finalize; (6) approve/dismiss/GET эндпоинты + session-spawn; (7) initiative_block сурфейсинг; (8) UI. Задачи 1-4 параллелятся; 5-7 зависят от 1-4.

---

## 8. Что дальше (фаза 2+, вне v1)

- Telegram inline-кнопка approve/dismiss с per-proposal-секретом (настоящий per-principal гейт, закрывает HIGH-1/MED-3).
- Durable re-drive инициативных целей (расширить `list_redrivable`, если сочтём безопасным авто-воскрешение агент-инициированной цели).
- Доставка результата цели в канал владельца (резолвер `owner_id→(channel,chat_id)`).
- Иерархическая декомпозиция плана (дневной → чанки) + реактивное перепланирование.
- Дополнительные источники целей; авто-approve с бюджет-капом (после наблюдения gated-режима).
- Принудительный approval-гейт на чувствительные тулы для `origin='initiative'` (MED-4, если базы когда-либо будут допущены).
