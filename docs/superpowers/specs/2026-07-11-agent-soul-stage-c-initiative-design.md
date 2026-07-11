# Этап C «Инициатива» — Design Spec

**Дата:** 2026-07-11
**Статус:** проектирование (ревизия 1)
**Предшественники (задеплоены на прод):** этап A (soul-фундамент: memory stream + рефлексия + SELF.md), этап B (анти-дрифт-детектор, detect-only).
**База:** `docs/research/2026-07-09-agent-soul-research.md` (находки 4 «инициатива = plan-decompose-react» и 289 «рефлексии как источник целей»); `docs/superpowers/specs/2026-07-09-agent-soul-foundation-design.md` (§ зависимостей, этап C).

---

## 1. Цель и не-цели

**Цель:** дать агентам минимальную, безопасную **инициативу** — способность из собственной души (рефлексии + SELF.md) сформулировать конкретную цель, персистентно её удержать и **предложить** владельцу; при одобрении — реально её преследовать, переиспользуя существующий `/goal`-движок.

**Ключевой инвариант (gated v1):** агент НИКОГДА не запускает автономную работу без явного одобрения владельца. Инициатива = «предложить + при approve исполнить», не «действовать самому». Это прямое продолжение консервативной линии этапов A (opt-in) и B (detect-only).

**Не-цели v1 (отложено):**
- Полное автономное дневное планирование Smallville-стиля (дневной→часовой→чанк декомпозиция без гейта) — фаза 2+.
- Реактивное перепланирование остатка плана — фаза 2.
- Telegram inline-кнопка одобрения — фаза 2 (v1: UI + HTTP-эндпоинт; уведомление в Telegram текстом можно, но кнопка-callback позже).
- Источники целей помимо рефлексий/SELF.md (открытые треды, wishlist владельца) — не в v1.
- Возможность агенту редактировать план-объект произвольными тулами — план мутирует ТОЛЬКО движок инициативы.

---

## 2. Что переиспользуется (не переписываем)

OPEX уже содержит **execution-субстрат** — не трогаем, только засеваем:

- `session_goals` (m056/m057): `goal_text`, `status`, `turn_count`/`max_turns` (бюджет ходов), `subgoals` JSONB, `last_verdict`, durable re-drive (`origin`, `next_redrive_at`).
- `agent/goal/driver.rs` — фоновый цикл: автономный ход → доставка → LLM-судья (`done`/`continue`) → бюджет → отмена; сериализуется против пользовательских ходов через `goal_locks`.
- `agent/goal/pool.rs` — per-session хэндлы драйверов.
- `notify()` (`gateway/handlers/notifications.rs`) — запись в БД + WS-бродкаст; типы `access_request`/`tool_approval`/`agent_error`/`watchdog_alert`; UI-колокольчик.
- `/api/channels/notify` — текст владельцу через канал по `agent.access.owner_id`.
- Soul: `agent/soul/reflection.rs::maybe_reflect` (вызывается из `knowledge_extractor.rs` после обработки завершённой сессии), `self_md.rs` (рендер SELF.md-блока), `memory_chunks` со строками `kind='reflection'`.

Этап C добавляет **слой инициативы** поверх: откуда берутся цели (душа) + персистентный план-объект + гейт одобрения.

---

## 3. Архитектура и компоненты

### 3.1 Данные — таблица `agent_plans` (migration, additive)

Один персистентный, кросс-сессионный объект на агента (не per-session — план это постоянное намерение агента, живущее между сессиями).

```sql
CREATE TABLE agent_plans (
    agent_id         TEXT PRIMARY KEY,
    current_focus    TEXT,                          -- «чем агент занят/озабочен сейчас»; сурфейсится в контекст
    proposals        JSONB NOT NULL DEFAULT '[]',   -- [{id:uuid, text, status, created_at, acted_at?}]
    last_proposal_at TIMESTAMPTZ,                    -- маркер «нового материала»
    proposals_today  INT  NOT NULL DEFAULT 0,        -- счётчик дневного cap
    proposal_day     DATE,                           -- маркер суток для сброса счётчика
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);
```

`proposals[]` элемент: `{ "id": uuid, "text": "…", "status": "pending"|"approved"|"dismissed", "created_at": ts, "acted_at": ts|null }`.

**Расширение существующего:** `session_goals.origin` CHECK `('goal','cron')` → `('goal','cron','initiative')`. Одобренная инициативная цель создаёт строку `origin='initiative'`. По аналогии с `origin='cron'` она durable-re-drive-eligible (владелец одобрил автономный прогон), но НЕ приклеивается к живой пользовательской сессии.

### 3.2 Конфиг — `[agent.initiative]` (opt-in)

```toml
[agent.initiative]
enabled = false           # по умолчанию выключено (как soul/drift)
daily_proposal_cap = 1    # максимум предложений в сутки
```

`validate()`: `daily_proposal_cap ∈ [1, 10]`. Исполнение одобренной цели наследует бюджет goal-движка (`max_turns` из существующего пути) и **обычный deny-list агента** — base-права НЕ выдаются автоматически неinitiative-целям.

Поле `initiative: InitiativeConfig` на `AgentSettings` (как `drift`/`soul`); `AgentConfig::load()` зовёт `validate()`. Правятся breaking-литералы `AgentSettings{}` (как в этапе B).

### 3.3 Движок — `agent/initiative/` (чистая логика + хук)

Модуль чистых, юнит-тестируемых функций (как `agent/drift/`):

- `pub fn should_propose(plan: &PlanRow, now: DateTime<Utc>, latest_reflection_at: Option<DateTime<Utc>>, cap: u32) -> bool`
  — true, если: есть новая рефлексия позже `plan.last_proposal_at` **И** дневной счётчик (с учётом сброса по `proposal_day` vs `now`) < `cap`.
- `pub fn reset_daily_if_new_day(plan: &PlanRow, today: NaiveDate) -> (u32 /*proposals_today*/, NaiveDate /*proposal_day*/)`
  — чистый сброс счётчика при смене суток.
- `pub fn render_focus_block(current_focus: &str, active_goals: &[String]) -> String`
  — рамочный read-only блок «Текущие занятия и цели» для контекста.

**Хук (fail-soft, не ломает основной путь):** `initiative_tick(deps, agent)` вызывается в `knowledge_extractor.rs` **сразу после того, как `maybe_reflect` записала новую рефлексию** — это и есть событие «нового материала» (не отдельный таймер, не LLM-heartbeat-прогон):

1. `enabled`? иначе выход. `owner_id` задан? иначе выход (некому одобрять — предложения бессмысленны).
2. Загрузить/создать `agent_plans` строку; применить `reset_daily_if_new_day`.
3. **Обновить `current_focus`** (дёшево): один LLM-вызов, «сожми SELF.md + свежие рефлексии в 1-2 фразы текущего фокуса». Выполняется только здесь (по новому материалу), не на каждый heartbeat.
4. **Гейт предложения** `should_propose(...)`: если true → один LLM-вызов «предложи ОДНУ конкретную цель, которую стоит преследовать, обоснуй одной фразой» → добавить `{status:'pending'}` в `proposals[]`, `notify(type='initiative_proposal', …)`, `proposals_today++`, `last_proposal_at = now`.
5. Всё в одной транзакции обновления строки; ошибки логируются и проглатываются (инициатива не критична).

LLM-вызовы идут через тот же raw-LLM путь, что и рефлексия (таймаут, как `reflection.rs`).

### 3.4 Гейт → одобрение → исполнение

- **Уведомление:** `notify(db, ui_event_tx, "initiative_proposal", title, body, data={agent, proposal_id, text})` — колокольчик + WS. Опционально текст владельцу через `/api/channels/notify` (без inline-кнопки в v1).
- **Эндпоинты** (`gateway/handlers/agents/…` или новый под-роутер `initiative.rs`):
  - `GET /api/agents/{name}/plan` — план-объект (current_focus + proposals + активные initiative-цели).
  - `POST /api/agents/{name}/plan/proposals/{id}/approve` — пометить proposal `approved`, создать `session_goals(origin='initiative', goal_text=proposal.text)` для новой сессии агента и заспавнить существующий goal-driver. Идемпотентно (повторный approve на уже-approved → no-op).
  - `POST /api/agents/{name}/plan/proposals/{id}/dismiss` — пометить `dismissed`.
  - Валидация: `{name}` — существующий агент; `{id}` — UUID из его `proposals[]`; изменение статуса разрешено только из `pending`.
- **Исполнение:** переиспользует `goal/driver.rs` как есть. Никакого нового execution-кода — только конструирование `GoalTarget` + spawn (зеркало пути `origin='cron'`).

### 3.5 Сурфейсинг в контекст

Блок «Текущие занятия и цели» в `context_builder.rs` (по образцу soul `self_block`): рендерит `current_focus` + список активных `origin='initiative'` целей в статусе running. Read-only инъекция в промпт; НЕ через `WORKSPACE_FILES`; агент видит свой фокус, но не пишет его тулами.

### 3.6 Безопасность

- Работает только при `enabled=true` **И** заданном `owner_id`.
- Дневной cap предложений (`should_propose`).
- Исполнение одобренной цели — под обычным бюджетом (`max_turns`) и deny-list агента; base-права не эскалируются.
- `agent_plans.current_focus`/`proposals` мутирует ТОЛЬКО движок инициативы и approve/dismiss-эндпоинты — не `workspace_write`/`workspace_edit` (план это таблица, не файл; произвольные тулы к ней доступа не имеют).
- Инъекция блока фокуса не расширяет права и не несёт недоверенного контента (текст сгенерирован самим агентом из своей же души, как SELF.md).

---

## 4. Поток данных (E2E)

```
завершённая сессия → knowledge_extractor → maybe_reflect() пишет reflection
   └─(новый материал)→ initiative_tick():
        refresh current_focus (LLM)                    → agent_plans.current_focus
        should_propose()? → generate 1 goal (LLM)      → proposals[] += {pending}
                          → notify('initiative_proposal') → колокольчик/Telegram
владелец: GET /plan → approve proposal
   └─→ session_goals(origin='initiative') + spawn goal-driver
        → автономные ходы под бюджетом → judge done/continue → done
контекст любой будущей сессии агента: блок «Текущие занятия и цели» (focus + активные цели)
```

---

## 5. Обработка ошибок

- `initiative_tick` полностью fail-soft: любая ошибка (LLM-таймаут, БД) логируется `warn` и проглатывается; рефлексия и extraction не затрагиваются.
- LLM-вызовы фокуса/предложения — с таймаутом (как `reflection.rs`); при провале — фокус/предложение просто не обновляются в этот раз.
- approve-эндпоинт при уже-approved/dismissed proposal → идемпотентный no-op (не двойной spawn).
- Отсутствие `agent_plans` строки → ленивое создание при первом тике/GET.
- Смена суток между тиками → `reset_daily_if_new_day` корректно обнуляет счётчик.

---

## 6. Тестирование

- **Юнит (чистые функции, как этап B):** `should_propose` (нет нового материала / cap исчерпан / оба ок), `reset_daily_if_new_day` (тот же день / новый день / NULL day), `render_focus_block` (пустой focus / с целями).
- **Интеграция:** approve-эндпоинт создаёт `session_goals(origin='initiative')` и не даёт двойного spawn; dismiss меняет статус; валидация `{name}`/`{id}`.
- **E2E на сервере:** включить `[agent.initiative]` на одном агенте с `owner_id`; породить рефлексии (прогнать сессии до порога рефлексии); наблюдать строку `agent_plans` (current_focus заполнен), `initiative_proposal`-уведомление; вызвать approve → убедиться, что goal-driver отработал (session_goals `origin='initiative'`, судья довёл до `done`).

---

## 7. Файловая структура (для плана реализации)

- `migrations/077_agent_plans.sql` — таблица + ALTER `session_goals.origin` CHECK (следующий свободный номер после m076).
- `crates/opex-core/src/config/mod.rs` — `InitiativeConfig` + `validate()` + поле на `AgentSettings`.
- `crates/opex-core/src/agent/initiative/mod.rs` — чистые функции + `initiative_tick`.
- `crates/opex-core/src/db/agent_plans.rs` — CRUD `agent_plans` (get/upsert/update proposals/counter).
- `crates/opex-core/src/agent/knowledge_extractor.rs` — вызов `initiative_tick` после `maybe_reflect`.
- `crates/opex-core/src/agent/context_builder.rs` (+ `engine/context_builder.rs`) — блок «Текущие занятия и цели».
- `crates/opex-core/src/gateway/handlers/agents/initiative.rs` (новый под-роутер) — GET plan / approve / dismiss; `notify` тип `initiative_proposal`.
- `crates/opex-core/src/db/session_goals.rs` — конструктор initiative-цели (origin='initiative') + spawn goal-driver (зеркало cron-пути).
- UI: `agent_plans`-панель + обработка `initiative_proposal`-уведомления (навигация к плану) — минимальная вкладка.

---

## 8. Что дальше (фаза 2+, вне v1)

- Telegram inline-кнопка approve/dismiss (переиспользуя approval-callback intercept).
- Иерархическая декомпозиция плана (дневной → чанки) и реактивное перепланирование.
- Дополнительные источники целей (открытые треды, wishlist).
- Авто-approve с бюджет-капом (после периода наблюдения gated-режима).
