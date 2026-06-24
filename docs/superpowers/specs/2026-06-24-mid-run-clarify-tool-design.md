# Mid-run clarify tool

**Дата:** 2026-06-24
**Статус:** Design (одобрено в brainstorming; ожидает spec review)
**Hermes-референс:** `D:/GIT/hermes-agent` — `tools/clarify_tool.py` (схема), `tools/clarify_gateway.py` (блокирующий event-waiter). OPEX-аналог паттерна — `crates/opex-core/src/agent/approval_manager.rs`.

## Цель

Дать агенту инструмент `clarify`, которым он задаёт уточняющий вопрос
пользователю **посреди выполнения хода** (вместо угадывания) и продолжает
тот же tool-loop после ответа. Первая из трёх Hermes-фич (далее
checkpoint/rollback, runtime user-hooks).

## Контекст (как сейчас)

- OPEX уже имеет блокирующий «агент ждёт человека» механизм —
  `approval_manager.rs`: `ApprovalWaitersMap` (id → `oneshot::Sender` + `Instant`),
  `create_and_wait` (channel-action + `StreamEvent::ApprovalNeeded` + oneshot +
  `tokio::time::timeout` + opportunistic cleanup), resolve через web-эндпоинт и
  channel-intercept (`channel_ws/inline.rs`), web `ApprovalCard`.
- Approval возвращает бинарный gate (`Approved`/`Rejected`/`ApprovedWithModifiedArgs`).
  Clarify нужен **текстовый/choice-ответ** — это главное отличие, ради которого
  делается ОТДЕЛЬНЫЙ менеджер (как Hermes держит `clarify_gateway` отдельно от
  approval, «same shape as tools.approval»).
- Hermes-семантика: блокирующая, поллинг 1-сек слайсами с `touch_activity`
  (иначе inactivity-watchdog убивает агента при долгом ожидании), timeout 600s,
  `clear_session` разблокирует висящие при `/new`/shutdown/eviction.

## Решения (из brainstorming)

1. **Семантика — блокирующая (вариант A)**, как Hermes и как существующий
   approval: ход реально ждёт ответа на oneshot-waiter и продолжается В ТОМ ЖЕ
   ходу (tool-loop сохраняет состояние).
2. **Отдельный `ClarifyManager`** — зеркало `approval_manager`, не расширение
   (чистые границы: gate ≠ вопрос-ответ).
3. **Tool `clarify {question, choices≤4}`**, single-select + авто-«Other»;
   без choices → open-ended. Возвращает LLM JSON `{question, choices_offered,
   user_response}`.
4. **Доставка по транспортам:** web SSE (`ClarifyNeeded` → `ClarifyCard`) и
   channel (inline-кнопки / текст + intercept). Нет UI-канала → tool возвращает
   «not available».
5. **Heartbeat при ожидании** — периодический `touch_session_activity`.
6. **Timeout** (config, default 600s) → «user did not respond», агент
   продолжает с дефолтом. **Cleanup** на re-entry/teardown — пустой ответ.
7. **Субагентам clarify недоступен** (нет user-канала) — в `SUBAGENT_DENIED_TOOLS`.

## Non-goals (YAGNI)

- multi-select (Hermes single; добавим только при реальной нужде).
- durable/persisted clarify через рестарт (in-memory waiter, как approval —
  timeout/cleanup закрывают висящие).
- clarify в cron/openai-compat/subagent (нет интерактивного канала).

## Компоненты

### 1. `clarify` system tool
- `tool_registry.rs`: регистрация core-tool `clarify`.
- Параметры: `question: string` (required), `choices: string[]` (optional, ≤4).
- Валидация/нормализация choices (порт `_flatten_choice` из Hermes: dict-shaped
  choices `[{"label"|"description"|"text"|"title": ...}]` → строка; >4 → обрезать;
  пусто → open-ended).
- Диспатч в `engine_dispatch.rs` (новый системный путь, как прочие core-tools);
  зовёт `ClarifyManager.create_and_wait(...)`.
- Результат: `serde_json::json!({question, choices_offered, user_response})`
  строкой. Без доступного канала/SSE → `{"error": "clarify not available in
  this execution context"}`.

### 2. `ClarifyManager` (`agent/clarify_manager.rs`)
Зеркало `approval_manager.rs`. Лежит рядом (`tex`/`AppState`).
- Состояние: `waiters: DashMap<Uuid /*clarify_id*/, (oneshot::Sender<String>, Instant)>`.
- `create_and_wait(question, choices, ctx) -> ClarifyOutcome`:
  1. `clarify_id = Uuid::new_v4()`; opportunistic cleanup протухших (>timeout);
     вставить `(tx, now)`.
  2. Доставка (см. §3): web emit `ClarifyNeeded`; channel — `channel_router.send`.
  3. Ожидание с timeout + heartbeat: цикл `tokio::time::timeout(min(1s, remaining), &mut rx)`,
     между итерациями `touch_session_activity`; до `clarify_timeout` или resolve.
  4. Возврат: `Answered(String)` | `TimedOut` | `Cancelled`.
- `resolve(clarify_id, response) -> bool` — `waiters.remove` + `tx.send(response)`.
- `clear_session(session_id)` — разблокировать все pending для сессии пустым
  ответом (цепляется к существующему session-cleanup на re-entry/teardown).

### 3. Доставка
- **Web:** `StreamEvent::ClarifyNeeded { clarify_id, question, choices: Vec<String>, timeout_ms }`
  (non-text, must-deliver через `send_async`, как `ApprovalNeeded`). Зеркалится в
  `ui/src/stores/sse-events.ts`. `ClarifyCard` (рядом с `ApprovalCard`): кнопки
  choices + «Other» (текст-поле) → `POST /api/clarify/{id}` `{response}`.
- **Channel** (`chat_id` в `_context`): `channel_router.send(action)` —
  choices → inline-кнопки (+«Other»); open-ended → текст-вопрос. Ответ:
  - нажатие кнопки → callback → `resolve`;
  - текст (open-ended или «Other») → intercept в `channel_ws/inline.rs` ДО
    обычной обработки (FIFO `has_pending` для сессии) → `resolve`.
- **Resolve-эндпоинт** `POST /api/clarify/{id}` в gateway (зеркало approval
  resolve-роута) → `ClarifyManager.resolve`.

### 4. Конфиг и политика
- `[agent] clarify_timeout_secs` в `opex.toml`, default **600**.
- `clarify` в `SUBAGENT_DENIED_TOOLS`.
- Уважает tool-policy `deny`/`allow` как прочие core-tools.

## Семантика (edge cases)

- **Timeout** → `ClarifyOutcome::TimedOut` → tool-результат «user did not
  respond within {N}s; proceed with a reasonable default» → tool-loop
  продолжается (не падает).
- **Cleanup** (re-entry/teardown/`/new`): `clear_session` → висящие waiters
  получают пустой ответ → tool возвращает «no response».
- **Несколько pending в сессии:** FIFO; text-fallback резолвит самый старый
  `awaiting_text` (open-ended/«Other»).
- **Heartbeat:** периодический `touch_session_activity` при ожидании (иначе
  watchdog reaper убьёт «тихий» долгий ход — см. session-liveness fix #5).

## Тестирование (TDD)

**Unit (`clarify_manager.rs`):**
- create → resolve(text) → `Answered("text")`.
- timeout → `TimedOut`.
- `clear_session` → висящий waiter разблокирован (Cancelled/пустой).
- choices-нормализация: dict-shaped → строка; >4 → 4; пусто → open-ended.

**Integration (mock channel_router / SSE):**
- web: `create_and_wait` эмитит `ClarifyNeeded`; `POST /api/clarify/{id}` будит
  waiter; tool возвращает JSON с `user_response`.
- channel: вопрос отправлен в router; text-intercept резолвит самый старый
  pending; tool возвращает ответ.

**Negative:**
- нет канала и нет SSE → tool возвращает «not available».
- субагент: `clarify` в denylist (unit-тест `SUBAGENT_DENIED_TOOLS.contains`).

## Open questions / future

- multi-select choices — если появится потребность.
- durable clarify через рестарт сервера — сейчас in-memory + timeout/cleanup.
