# Mid-run clarify tool

**Дата:** 2026-06-24
**Статус:** Design v2 (одобрено в brainstorming + адверсариальное ревью; ожидает финального spec review)
**Hermes-референс:** `D:/GIT/hermes-agent` — `tools/clarify_tool.py` (схема), `tools/clarify_gateway.py` (блокирующий event-waiter). OPEX-аналог паттерна — `crates/opex-core/src/agent/approval_manager.rs`.

## Цель

Дать агенту инструмент `clarify`, которым он задаёт уточняющий вопрос
пользователю **посреди выполнения хода** (вместо угадывания) и продолжает тот же
tool-loop после ответа. Первая из трёх Hermes-фич (далее checkpoint/rollback,
runtime user-hooks).

## Контекст (как сейчас)

- OPEX уже имеет блокирующий «агент ждёт человека» механизм —
  `approval_manager.rs`: `ApprovalWaitersMap = Arc<DashMap<ApprovalId,
  (oneshot::Sender<ApprovalResult>, Instant)>>`; `request_approval(agent_name,
  tool_name, arguments, context, timeout_secs, channel_router, ui_event_tx,
  sse_event_tx) -> ApprovalOutcome` (approval_manager.rs:78). Вызывается из
  `engine_dispatch.rs:107-116`; доставка = channel-action + emit
  `StreamEvent::ApprovalNeeded` + oneshot + `tokio::time::timeout`; opportunistic
  cleanup (DashMap::retain), `prune_stale` на входе в `engine/run.rs:285`.
- Approval возвращает бинарный gate. Clarify нужен **текстовый/choice-ответ** —
  главное отличие, ради которого делается ОТДЕЛЬНЫЙ менеджер (как Hermes держит
  `clarify_gateway` отдельно от approval).
- Hermes-семантика: блокирующая, поллинг 1-сек слайсами с `touch_activity`,
  timeout 600s, `clear_session` разблокирует висящие.

## Решения (brainstorming + ревью)

1. **Семантика — блокирующая (A)**, как Hermes и approval: ход ждёт ответа на
   oneshot-waiter, продолжается в том же tool-loop.
2. **Отдельный `ClarifyManager`** — зеркало `approval_manager` (чистые границы).
3. **Tool `clarify {question, choices≤4}`**, single-select + авто-«Other»;
   без choices → open-ended. Возвращает LLM JSON `{question, choices_offered,
   user_response}`.
4. **Detection канала ОБЯЗАТЕЛЬНА** (ревью HIGH-3): до блокировки tool проверяет
   наличие интерактивного канала по `_context`. Нет канала → немедленно
   возвращает «not available» (НЕ блокирует) — иначе cron/openai-compat зависнут
   на весь timeout.
5. **Heartbeat при ожидании** — `touch_session_activity` поллингом (ревью HIGH-1).
6. **Timeout согласован с транспортом** (ревью HIGH-2): web — engine decoupled от
   HTTP; channel — `clarify_timeout` НЕ должен срубаться `request_timeout_secs`.
7. **clarify sequential-only** (ревью MED-5): не в `is_system_tool_parallel_safe`.
8. **Приоритет intercept** (ревью MED-4): активный approval-waiter сессии →
   clarify text-intercept НЕ перехватывает.
9. **`clear_session`** требует нового reverse-index (ревью HIGH/clear): у approval
   такого нет — проектируем (см. §Компоненты).
10. **Субагентам clarify недоступен** — в `SUBAGENT_DENIED_TOOLS`.

## Non-goals (YAGNI)

- multi-select; durable clarify через рестарт сервера (in-memory + timeout/cleanup).
- clarify в cron/openai-compat/subagent — детектится и возвращает «not available».
- Починка латентного approval-heartbeat-бага (ревью HIGH-1) — отдельная задача;
  здесь только отмечаем, что clarify делает heartbeat правильно с самого начала.

## Компоненты

### 1. `clarify` system tool
- Регистрация core-tool в `tool_registry.rs`; диспатч-ветка в `engine_dispatch.rs`
  (как прочие system-tools, доступ к `deps.cfg.clarify_manager`,
  `deps.state.channel_router`, `deps.tex.sse_event_tx`, `deps.db`, `_context`).
- Параметры: `question: string` (required), `choices: string[]` (optional, ≤4).
- Нормализация choices (порт `_flatten_choice`): dict-shaped
  `[{"label"|"description"|"text"|"title": ...}]` → строка; >4 → обрезать; пусто
  → open-ended.
- **Detection канала (до вызова менеджера):** из `_context` извлечь `chat_id` и
  `_channel`. Канал доступен ⟺ `chat_id` присутствует (channel) ИЛИ
  `_channel == "ui"` (web SSE). Иначе (`inter_agent` openai_compat
  [openai_compat.rs:195], cron NoopSink, отсутствие признаков) → вернуть
  `{"error":"clarify not available in this execution context"}` БЕЗ блокировки.
- Результат: `serde_json::json!({question, choices_offered, user_response})`
  строкой.

### 2. `ClarifyManager` (`agent/clarify_manager.rs`)
Зеркало `approval_manager.rs`. В `AgentConfig` рядом с `approval_manager`.
- Состояние:
  - `waiters: DashMap<Uuid /*clarify_id*/, (oneshot::Sender<String>, Instant)>`.
  - **reverse-index** `by_session: DashMap<Uuid /*session_id*/, Vec<Uuid /*clarify_id*/>>`
    (нужен для `clear_session` — у approval его нет, проектируем здесь).
- `create_and_wait(question, choices, session_id, transport_ctx) -> ClarifyOutcome`:
  1. `clarify_id = Uuid::new_v4()`; opportunistic cleanup протухших
     (cutoff = `clarify_timeout + 60s`, НЕ hardcode 300 — ревью LOW-7); вставить в
     `waiters` + `by_session`.
  2. Доставка (см. §3).
  3. Ожидание с heartbeat: `loop { tokio::time::timeout(min(1s, remaining),
     &mut rx) → break on Ok; touch_session_activity(db, session_id) }` до
     `clarify_timeout` или resolve (порт Hermes wait_for_response).
  4. Снять из `waiters`+`by_session`. Возврат: `Answered(String)` |
     `NoResponse { reason: TimedOut | Cancelled }`.
- `resolve(clarify_id, response) -> bool` — `waiters.remove` + `tx.send(response)`.
- `clear_session(session_id)` — по `by_session` разблокировать все pending
  (drop sender → `Cancelled`); вызывается из teardown (см. §4).
- `has_pending_text(session_id) -> Option<clarify_id>` — самый старый open-ended/
  «Other» pending (для text-intercept FIFO).

### 3. Доставка
- **Web:** добавить вариант в ДВУХ местах:
  - internal `StreamEvent::ClarifyNeeded { clarify_id, question, choices, timeout_ms }`
    (`agent/stream_event.rs`);
  - **wire** `SseEvent::ClarifyNeeded {...}` в `crates/opex-types/src/sse.rs`
    (ts-rs codegen → `ui/src/types/sse.generated.ts`; `sse-events.ts` —
    re-export, руками НЕ трогаем — ревью HIGH/SSE).
  - `sse_converter.rs`: arm `StreamEvent::ClarifyNeeded → SseEvent::ClarifyNeeded`
    рядом с `ApprovalNeeded` (~373-381), через `SseStreamWriter::build_pure`.
  - `ClarifyCard` (ui, рядом с `ApprovalCard`): кнопки choices + «Other»-текст →
    `POST /api/clarify/{id}` `{response}`.
- **Channel** (`chat_id` present): `channel_router.send(action)` — choices →
  inline-кнопки с callback-data `clarify:{clarify_id}:{choice_idx}` (+«Other»);
  open-ended → текст-вопрос. Резолв:
  - **button-callback** — перехват в `channel_ws/inline.rs` рядом с
    `handle_approval_callback` (reader.rs:114-124 порядок); парсинг
    `clarify:{id}:{idx}` → `resolve`.
  - **text (open-ended/«Other»)** — перехват в `channel_ws/reader.rs` ДО
    `dispatch_message` (НЕ в inline.rs — обычный текст минует inline; ревью
    HIGH/intercept): если `clarify_manager.has_pending_text(session)` И НЕТ
    активного approval-waiter сессии (приоритет, ревью MED-4) → `resolve` +
    `continue`.
- **Resolve-эндпоинт** `POST /api/clarify/{id}` — новый handler (зеркало
  approval resolve, agents/mod.rs:26 + crud.rs:1021) → `ClarifyManager.resolve`.

### 4. Конфиг, политика, lifecycle
- `[agent] clarify_timeout_secs` в `opex.toml`, default **600**. Для **channel**
  пути: dispatcher (`channel_ws/reader.rs:133` передаёт `request_timeout_secs`)
  не должен срубать clarify-ожидание раньше — реализация прокидывает
  `max(request_timeout_secs, clarify_timeout_secs)` для ходов с pending clarify,
  ЛИБО clarify-ожидание исключается из dispatcher-timeout (детализация — в плане).
- `clarify` в `SUBAGENT_DENIED_TOOLS` (subagent.rs:17-29).
- `clarify` НЕ добавляется в `is_system_tool_parallel_safe` → sequential-ветка
  (parallel.rs) как approval.
- **Teardown hook:** `clear_session(session_id)` вызывается при завершении/
  прерывании хода (`pipeline/finalize.rs` / `SessionLifecycleGuard`), чтобы
  висящий waiter не ждал весь timeout при interrupt/re-entry.

## Семантика (edge cases)

- **NoResponse{TimedOut}** (timeout истёк) и **NoResponse{Cancelled}**
  (clear_session/teardown) → tool-результат «user did not respond; proceed with a
  reasonable default» → tool-loop продолжается (различие reason — для логов; для
  агента поведение одно — ревью MED-6).
- **Несколько pending в сессии:** FIFO; text-fallback резолвит самый старый
  `awaiting_text` через `has_pending_text`.
- **Heartbeat:** см. §2 п.3 (иначе watchdog reaper убьёт «тихий» ход —
  session-liveness fix #5).
- **Приоритет approval > clarify** для text-intercept (ревью MED-4).

## Тестирование (TDD)

**Unit (`clarify_manager.rs`):**
- create → resolve(text) → `Answered("text")`.
- timeout → `NoResponse{TimedOut}`.
- `clear_session` → висящий waiter `NoResponse{Cancelled}`; reverse-index очищен.
- choices-нормализация: dict-shaped → строка; >4 → 4; пусто → open-ended.
- opportunistic cleanup cutoff = timeout+margin (не 300).

**Unit (tool):**
- detection: `chat_id` есть → канал; `_channel=="ui"` → канал; `inter_agent`/нет
  признаков → «not available» БЕЗ блокировки.
- `clarify` в `SUBAGENT_DENIED_TOOLS`.

**Integration (mock channel_router / SSE):**
- web: эмит `ClarifyNeeded`; `POST /api/clarify/{id}` будит waiter; tool
  возвращает JSON с `user_response`.
- channel button: callback `clarify:{id}:{idx}` → resolve.
- channel text: `reader.rs` intercept резолвит самый старый pending; при активном
  approval-waiter clarify text-intercept НЕ срабатывает (приоритет).

**Negative:** нет канала → «not available»; субагент denylist.

## Open questions / future

- multi-select; durable clarify через рестарт.
- Латентный approval-heartbeat-баг (ревью HIGH-1) — отдельная задача.
