# Server-Authoritative Chat Stream Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Один код-путь подключения к чату: `POST /api/chat` только стартует ход (202), единый `GET /{session}/stream` на каждом подключении шлёт авторитетный конверт `sync_begin → полный replay → sync_end → live`; клиент пересобирает состояние хода из конверта идемпотентно — resume/dedup/Last-Event-ID-машина клиента удаляется.

**Architecture:** Сервер: `StreamRegistry` получает `boundary_message_id` и ёмкость 10k; регистрация стрима переезжает из конвертера в POST-хендлер (синхронный bootstrap → 202 → spawn execute); `sse.rs`+`resume.rs` сливаются в единый GET-хендлер с конвертом. Клиент: новый тонкий `chat-stream.ts` (fetch-стрим, batch-apply конверта), boundary-фильтр истории, id-based handoff на finish; overlay-дедуп/reconnect-машина/`activeSessionIds`/`lastEventId` удаляются.

**Tech Stack:** Rust (axum, tokio, StreamRegistry), Next.js 16 + Zustand + React Query, vitest.

**Spec:** [docs/superpowers/specs/2026-07-15-server-authoritative-chat-stream-design.md](../specs/2026-07-15-server-authoritative-chat-stream-design.md)

## Global Constraints

- Rust 2024, rustls. НЕТ `make` на Windows → `cargo` напрямую. Rust-тесты ЛОКАЛЬНО НЕ гонять (Windows крашится; DATABASE_URL пуст) → верификация `cargo check --all-targets -p opex-core`; авторитетный прогон + `cargo clippy --bin opex-core -- -D warnings` на серверном гейте (clippy ловит string-slice/collapsible-if, которые check пропускает — писать идиоматично сразу).
- После изменения `opex-types` SSE-типов: `cargo run --features ts-gen --bin gen_ts_types -p opex-core` → коммитить `ui/src/types/sse.generated.ts` (CI ловит drift).
- vitest ТОЛЬКО из `ui/` (`cd ui && npm test`); `npx tsc --noEmit` должен быть exit-0 в конце каждой UI-задачи, КРОМЕ задокументированного красного окна внутри T7→T8 (координированный cutover).
- Инварианты спеки: разрыв GET-стрима ≠ abort движка (отписка = detach); abort только `POST /api/chat/{id}/abort`; `/v1/*` openai-compat и channel_ws НЕ трогаются; модель `ChatMessage`/`MessagePart` НЕ меняется — существующие 1356+ vitest (включая голосовые/T4-очередь) зелёные = критерий приёмки.
- `?agent=` owner-гейт (IDOR, `verify_session_agent`) сохраняется на GET-стриме.
- Дефолты спеки: буфер = 10 000 событий; staleness = 15 сек (конфигурируемо константой).
- Коммиты без Co-Authored-By, master, push/deploy только с явного подтверждения.

---

### Task 1: Wire-типы конверта (`opex-types` + gen-types)

**Files:**
- Modify: `crates/opex-types/src/sse.rs` (enum `SseEvent` — там же, где `Sync`/`SyncStatus`, используемые в resume.rs:27)
- Modify: `ui/src/types/sse.generated.ts` (регенерация)

**Interfaces:**
- Produces (потребляют T3, T6):
```rust
SseEvent::SyncBegin { boundary_message_id: Option<Uuid>, run_status: SyncStatus, truncated: bool }
SseEvent::SyncEnd { last_seq: Option<u64> }
```
Wire-имена: `"sync_begin"` / `"sync_end"` (serde-тегирование как у остальных вариантов enum — прочитать существующий `#[serde(tag=…)]`/`rename_all` в sse.rs и следовать ему).

- [ ] **Step 1:** Прочитать `crates/opex-types/src/sse.rs` — существующий стиль enum (`SseEvent::Sync { content, tool_calls, status, error }`, `SyncStatus`). Добавить два варианта:
```rust
    /// Открывает авторитетный snapshot-конверт: всё, что придёт до SyncEnd,
    /// клиент применяет батчем (без анимации). boundary_message_id — id
    /// user-сообщения активного хода: история рендерится ВПЛОТЬ ДО него
    /// включительно, всё после — live-состояние. None + finished = активного
    /// хода нет, конверт пуст (клиент рисует чисто REST-историю).
    #[serde(rename = "sync_begin")]
    SyncBegin {
        boundary_message_id: Option<uuid::Uuid>,
        run_status: SyncStatus,
        /// Буфер переполнился — replay неполон; клиент берёт частичный текст
        /// из REST (streaming_db персистит инкрементально) + хвост буфера.
        truncated: bool,
    },
    /// Закрывает конверт. last_seq — seq последнего replay-события (None при
    /// пустом конверте). После него идут live-события.
    #[serde(rename = "sync_end")]
    SyncEnd { last_seq: Option<u64> },
```
(Точные имена полей/атрибутов подогнать под фактический стиль enum — например если варианты используют `rename_all = "kebab-case"`, то rename-атрибуты не нужны, но wire-строки должны получиться `sync_begin`/`sync_end` — проверить сериализацией в юнит-тесте.)

- [ ] **Step 2:** Юнит-тест сериализации в том же файле (по образцу существующих):
```rust
#[test]
fn sync_envelope_wire_format() {
    let b = SseEvent::SyncBegin { boundary_message_id: None, run_status: SyncStatus::Finished, truncated: false };
    let s = serde_json::to_string(&b).unwrap();
    assert!(s.contains("\"sync_begin\""), "{s}");
    let e = SseEvent::SyncEnd { last_seq: Some(41) };
    assert!(serde_json::to_string(&e).unwrap().contains("\"sync_end\""));
}
```

- [ ] **Step 3:** `cargo check --all-targets -p opex-core` → 0 ошибок. Затем `cargo run --features ts-gen --bin gen_ts_types -p opex-core` → перегенерирован `ui/src/types/sse.generated.ts` (новые типы в нём).

- [ ] **Step 4: Commit**
```bash
git add crates/opex-types/src/sse.rs ui/src/types/sse.generated.ts
git commit -m "feat(stream): SyncBegin/SyncEnd envelope wire types"
```

---

### Task 2: `StreamRegistry` — boundary, ёмкость 10k, truncated

**Files:**
- Modify: `crates/opex-core/src/gateway/stream_registry.rs`

**Interfaces:**
- Produces (потребляют T3, T4):
  - `register_with_token(&self, session_id: Uuid, agent_id: &str, cancel_token: CancellationToken, boundary_message_id: Uuid) -> Option<Uuid>` — новый 4-й параметр.
  - `subscribe(&self, session_id: &str) -> Option<StreamSubscription>` где
```rust
pub struct StreamSubscription {
    pub events: Vec<(u64, String)>,
    pub rx: broadcast::Receiver<(u64, String)>,
    pub finished: bool,
    pub boundary_message_id: Uuid,
    pub truncated: bool,
}
```
  - `MAX_BUFFER_SIZE: usize = 10_000` (было 1_000), `BROADCAST_CAPACITY = 10_000` (было 1_024).

- [ ] **Step 1: Failing-тесты** (в существующий `#[cfg(test)]`; DB-независимые части — как `cancel_nonexistent_returns_false`; register требует DB → `#[sqlx::test(migrations = "../../migrations")]`):
```rust
#[sqlx::test(migrations = "../../migrations")]
async fn subscribe_carries_boundary_and_truncated(pool: sqlx::PgPool) {
    let registry = StreamRegistry::new(pool);
    let sid = Uuid::new_v4();
    let boundary = Uuid::new_v4();
    let token = CancellationToken::new();
    registry.register_with_token(sid, "A", token, boundary).await.expect("register");
    let sub = registry.subscribe(&sid.to_string()).await.expect("subscribed");
    assert_eq!(sub.boundary_message_id, boundary);
    assert!(!sub.truncated);
    assert!(sub.events.is_empty());
}

#[sqlx::test(migrations = "../../migrations")]
async fn overflow_sets_truncated(pool: sqlx::PgPool) {
    let registry = StreamRegistry::new(pool);
    let sid = Uuid::new_v4();
    registry.register_with_token(sid, "A", CancellationToken::new(), Uuid::new_v4()).await.unwrap();
    let key = sid.to_string();
    for i in 0..(10_000 + 5) {
        registry.push_event(&key, &format!("{{\"i\":{i}}}")).await;
    }
    let sub = registry.subscribe(&key).await.unwrap();
    assert_eq!(sub.events.len(), 10_000);
    assert!(sub.truncated);
}
```

- [ ] **Step 2: Реализация.** В `ActiveStream` добавить `boundary_message_id: Uuid` и в `ActiveStreamInner` — `truncated: bool` (ставится в `push_event` в ветке «Buffer full: broadcast only», :153-155). Поднять константы (`MAX_BUFFER_SIZE`/`BROADCAST_CAPACITY` = 10_000; комментарий «single-user система, буфер обязан вмещать полный tool-ход — спека §5.3»). `subscribe` возвращает новую структуру `StreamSubscription` (боундари/truncated читаются под тем же per-stream lock, что и snapshot — атомарность сохранена). Обновить единственных вызывающих: `resume.rs:107` (деструктуризация — этот файл умирает в T4, здесь только починить компиляцию) и `sse_converter.rs:236` (передать boundary — ВРЕМЕННО `Uuid::nil()`; T3 переносит регистрацию в POST-хендлер и удалит этот вызов).

- [ ] **Step 3:** `cargo check --all-targets -p opex-core` → 0 ошибок.

- [ ] **Step 4: Commit**
```bash
git add crates/opex-core/src/gateway/stream_registry.rs crates/opex-core/src/gateway/handlers/chat/resume.rs crates/opex-core/src/gateway/handlers/chat/sse_converter.rs
git commit -m "feat(stream): registry carries boundary_message_id; 10k buffer with truncated flag"
```

---

### Task 3: `POST /api/chat` → 202 (синхронный bootstrap + регистрация ДО ответа)

**Files:**
- Modify: `crates/opex-core/src/agent/engine/run.rs:92-181` (`handle_sse` — разрез на bootstrap/execute)
- Modify: `crates/opex-core/src/gateway/handlers/chat/sse.rs` (хендлер: 202 вместо стрима)
- Modify: `crates/opex-core/src/gateway/handlers/chat/sse_converter.rs:225-240` (убрать регистрацию из конвертера)

**Interfaces:**
- Consumes: `register_with_token(…, boundary_message_id)` из T2; `BootstrapOutcome { session_id, user_message_id, … }` (`pipeline/bootstrap.rs:19-32`).
- Produces (потребляет T6):
  - `POST /api/chat` → `202 {"session_id": "<uuid>", "user_message_id": "<uuid>"}` (тело запроса НЕ меняется: `ChatSseRequest` как есть).
  - Движок: `pub async fn bootstrap_sse(&self, msg, session_id, force_new_session) -> anyhow::Result<BootstrapOutcome>` + `pub async fn execute_sse(&self, boot: BootstrapOutcome, tx: EngineEventSender, cancel: CancellationToken) -> anyhow::Result<()>` — разрез `handle_sse` по границе «после `bootstrap::bootstrap(...)` (:135-146) / до execute».

- [ ] **Step 1: Разрез `handle_sse`.** Прочитать run.rs:92-181 целиком. Существующее тело: построение sink/контекста → `bootstrap::bootstrap(...)` → `boot_for_execute` → execute → finalize. Вынести:
  - `bootstrap_sse`: всё до и включая `bootstrap::bootstrap` + получение `BootstrapOutcome`; возвращает outcome (sink для bootstrap-этапа — тот же, каким handle_sse пользуется до execute; если bootstrap пишет события в sink (running-фаза) — событmän уйдут в registry после регистрации, см. Step 2 порядок).
  - `execute_sse(boot, …)`: остаток (construct `boot_for_execute` → execute → finalize).
  - `handle_sse` сохранить как тонкую обёртку `bootstrap_sse` + `execute_sse` (её используют другие вызыватели — grep `handle_sse(` по crate и НЕ ломать их).
- [ ] **Step 2: Хендлер POST.** В `sse.rs::api_chat_sse` (после mention-routing и построения `msg`, :162-174):
```rust
    // 1. Синхронный bootstrap: session_id + user_message_id (= boundary).
    let pipeline_cancel = CancellationToken::new();
    let boot = match engine.bootstrap_sse(&msg, session_id, force_new_session).await {
        Ok(b) => b,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
    };
    // 2. Регистрация стрима ДО ответа — GET после 202 гарантированно найдёт его.
    if bus.stream_registry.register_with_token(
        boot.session_id, engine.name(), pipeline_cancel.clone(), boot.user_message_id,
    ).await.is_none() {
        return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"error": "stream registry at capacity"}))).into_response();
    }
    // 3. Спавн движка + конвертера — как раньше (raw/coalescer/converter),
    //    только вместо handle_sse — execute_sse(boot, …), и sse_tx-приёмник
    //    конвертеру больше не нужен для КЛИЕНТА (клиент читает GET) — конвертер
    //    продолжает писать в registry (send_and_buffer уже это делает).
    // 4. Ответ:
    return (StatusCode::ACCEPTED, Json(json!({
        "session_id": boot.session_id,
        "user_message_id": boot.user_message_id,
    }))).into_response();
```
  Конкретика по п.3: сохранить существующие спавны (engine task :249-266 с `execute_sse`, converter :274-284), но `ConverterCtx.sse_tx` заменить на sink-в-никуда ЛИБО (чище) сделать `sse_tx: Option<…>` в `ConverterCtx` и в `send_and_buffer!` слать клиенту только при `Some` — прочитать `sse_converter.rs` и выбрать минимальную правку; registry-путь (буферизация) обязан работать без клиента (он уже так спроектирован — sse.rs:206-217). Регистрацию внутри конвертера (:225-240) удалить — стрим уже зарегистрирован хендлером; конвертеру `job_id`/registry-хэндл передать через `ConverterCtx` при необходимости (прочитать, что он делает после register — mark_finished/error — эти вызовы остаются).
- [ ] **Step 3: Тест** (сериализация 202-формы — юнит на shape; полный HTTP-тест не поднимаем, по конвенции соседей):
```rust
#[test]
fn accepted_body_shape() {
    let v = serde_json::json!({"session_id": uuid::Uuid::nil(), "user_message_id": uuid::Uuid::nil()});
    assert!(v.get("session_id").is_some() && v.get("user_message_id").is_some());
}
```
  Поведенческое покрытие даёт серверный гейт существующих pipeline-тестов + E2E (T10).
- [ ] **Step 4:** `cargo check --all-targets -p opex-core` → 0. Grep: `handle_sse(` — все прежние вызыватели компилируются; `x-vercel-ai-ui-message-stream` header из POST-ответа удалён (стрима больше нет).
- [ ] **Step 5: Commit**
```bash
git add crates/opex-core/src/agent/engine/run.rs crates/opex-core/src/gateway/handlers/chat/sse.rs crates/opex-core/src/gateway/handlers/chat/sse_converter.rs
git commit -m "feat(stream): POST /api/chat returns 202; bootstrap sync + registry registration before response"
```

---

### Task 4: Единый GET-стрим с конвертом (слияние sse-stream + resume)

**Files:**
- Modify: `crates/opex-core/src/gateway/handlers/chat/resume.rs` → переименовать в `stream.rs` (единый хендлер; `git mv`)
- Modify: `crates/opex-core/src/gateway/handlers/chat/mod.rs` (роуты: `GET /api/chat/{id}/stream` → новый хендлер; имя роута не меняется)

**Interfaces:**
- Consumes: `StreamSubscription` (T2), `SseEvent::SyncBegin/SyncEnd` (T1).
- Produces (потребляет T6): контракт GET-стрима из спеки §4.2:
  - активный стрим: `sync_begin{boundary_message_id, run_status:"running", truncated}` → replay ВСЕХ buffered (SSE id=seq) → `sync_end{last_seq}` → live (id=seq) → `finish`/`[DONE]`;
  - завершённый/отсутствующий: `sync_begin{boundary_message_id:null, run_status:<finished|error|interrupted>, truncated:false}` → (для interrupted — существующий `Sync{content…}` из stream_jobs как единственное событие конверта) → `sync_end{last_seq:null}` → `[DONE]`;
  - `Last-Event-ID` игнорируется (полный replay всегда); `204` больше не возвращается.

- [ ] **Step 1: Реализация.** Взять текущий `api_chat_resume_stream` за основу:
  - owner-гейт (`?agent=` + `verify_session_agent`, :43-54) — БЕЗ изменений;
  - удалить парсинг `last_event_id` (:56-60) и фильтрацию (:109-115);
  - ветка `Some(sub)`: yield `sync_begin` (из `sub.boundary_message_id`, `run_status = if sub.finished {Finished} else {Running}`, `sub.truncated`) → replay `sub.events` (как :122-126) → yield `sync_end{last_seq: sub.events.last().map(|(s,_)|*s)}` → если `sub.finished` → `[DONE]`; иначе live-цикл (:138-176) с cutoff = last replayed seq (логика сохраняется, только `highest_replayed` инициализируется из replay);
  - ветка `None`: вместо 204 — конверт: `sync_begin{boundary:None, run_status: <из stream_jobs как сейчас :66-94>}` → (если job есть — существующий `Sync{…}`-event внутрь конверта) → `sync_end{last_seq:None}` → `[DONE]`. Если и job нет — `run_status: Finished`, пустой конверт.
- [ ] **Step 2: Тесты.** Чистые юнит-тесты на построение конверта выносом хелпера:
```rust
/// События конверта для отсутствующего/завершённого стрима.
fn empty_envelope(status: SyncStatus, sync_payload: Option<SseEvent>) -> Vec<String> { … }

#[test]
fn empty_envelope_orders_begin_payload_end() {
    let ev = empty_envelope(SyncStatus::Finished, None);
    assert!(ev[0].contains("sync_begin") && ev.last().unwrap().contains("sync_end"));
    assert_eq!(ev.len(), 2);
}
#[test]
fn empty_envelope_includes_interrupted_sync() {
    let s = SseEvent::Sync { content: "partial".into(), tool_calls: vec![], status: SyncStatus::Interrupted, error: None };
    let ev = empty_envelope(SyncStatus::Interrupted, Some(s));
    assert_eq!(ev.len(), 3);
    assert!(ev[1].contains("partial"));
}
```
- [ ] **Step 3:** `cargo check --all-targets -p opex-core` → 0. Инвариант-проверка глазами: разрыв GET (drop stream future) НЕ трогает cancel_token (никаких вызовов cancel в хендлере) — спека §5.4.
- [ ] **Step 4: Commit**
```bash
git add crates/opex-core/src/gateway/handlers/chat/
git commit -m "feat(stream): unified GET stream with sync envelope (replaces resume; full replay, no Last-Event-ID)"
```

---

### Task 5: Серверная чистка + серверный гейт

**Files:**
- Modify: `crates/opex-core/src/gateway/handlers/chat/mod.rs`, `sse.rs`, docs-комментарии

- [ ] **Step 1:** Grep-чистка: упоминания `Last-Event-ID`/`last_event_id` в chat-хендлерах удалены; `204` semantics удалены; комментарии sse.rs/stream.rs обновлены под новую архитектуру (POST=202, GET=единственный стрим). CLAUDE.md-таблица SSE-событий дополняется `sync_begin`/`sync_end` (двумя строками, не переписывая раздел).
- [ ] **Step 2:** `cargo check --all-targets -p opex-core` → 0 И `cargo clippy --bin opex-core -- -D warnings` локально может не работать (Windows) — прогнать на СЕРВЕРЕ вместе с тестами: `ssh … cargo clippy --bin opex-core -- -D warnings && cargo test --bin opex-core -- --test-threads=4` (throttled, `PATH=$HOME/.cargo/bin:$PATH`, `DATABASE_URL=postgres://opex_test:opex_test@127.0.0.1:5434/opex_test`). Ожидаемо: clippy clean, тесты зелёные (включая новые T1-T4). ЭТО ГЕЙТ: до него UI-задачи не начинать.
- [ ] **Step 3: Commit**
```bash
git add -A crates/opex-core CLAUDE.md
git commit -m "chore(stream): server cleanup + docs for envelope protocol"
```

---

### Task 6: Клиент — новый транспорт `chat-stream.ts` (standalone, пока не подключён)

**Files:**
- Create: `ui/src/stores/stream/chat-stream.ts`
- Modify: `ui/src/stores/stream/stream-processor.ts` (batch-режим: копить, коммитить на sync_end)
- Test: `ui/src/stores/stream/__tests__/chat-stream.test.ts`

**Interfaces:**
- Consumes: `POST /api/chat → 202 {session_id, user_message_id}` (T3); GET-конверт (T4); существующий `processSSEStream`/`StreamSession`.
- Produces (потребляет T7):
```ts
export interface TurnStreamCallbacks {
  onBoundary(boundaryMessageId: string | null, runStatus: string, truncated: boolean): void;
  onEnvelopeApplied(): void;      // после sync_end (батч закоммичен)
  onFinished(): void;             // finish/[DONE]/пустой конверт
  onConnectionLost(): void;       // сеть/обрыв БЕЗ finish — вызывающий решает re-open
}
export function openTurnStream(agent: string, sessionId: string, session: StreamSession, cb: TurnStreamCallbacks): void;
export function startTurn(agent: string, body: Record<string, unknown>): Promise<{ session_id: string; user_message_id: string }>; // POST → 202
```

- [ ] **Step 1: Failing-тесты** (fixture-SSE как в существующих `stream/__tests__/fixtures/*.sse` — построить строку конверта руками):
```ts
import { openTurnStream } from "../chat-stream";
// хелпер: ReadableStream из строк — скопировать из существующего stream-processor.test.ts

it("applies envelope as a single batch and fires callbacks in order", async () => {
  const events = [
    `data: ${JSON.stringify({ type: "sync_begin", boundary_message_id: "b-1", run_status: "running", truncated: false })}\n\n`,
    `data: ${JSON.stringify({ type: "start", messageId: "m-1" })}\n\n`,
    `data: ${JSON.stringify({ type: "text-start", id: "t1" })}\n\n`,
    `data: ${JSON.stringify({ type: "text-delta", delta: "Привет" })}\n\n`,
    `data: ${JSON.stringify({ type: "sync_end", last_seq: 3 })}\n\n`,
  ];
  // мокнуть fetch → Response со стримом; собрать порядок колбэков
  // assert: onBoundary("b-1","running",false) ДО onEnvelopeApplied;
  // batch: до sync_end в store НОЛЬ коммитов (parts не видны), после — один коммит с текстом "Привет".
});

it("empty finished envelope fires onFinished without touching live state", async () => {
  // sync_begin{boundary:null, run_status:"finished"} + sync_end → onBoundary(null,...), onFinished(); onConnectionLost НЕ вызван
});

it("network error before finish fires onConnectionLost", async () => {
  // стрим обрывается после sync_end без finish → onConnectionLost
});

it("envelope with tool events rebuilds tool parts in one batch (spec §8: tool-ход)", async () => {
  const events = [
    `data: ${JSON.stringify({ type: "sync_begin", boundary_message_id: "b-1", run_status: "running", truncated: false })}\n\n`,
    `data: ${JSON.stringify({ type: "start", messageId: "m-1" })}\n\n`,
    `data: ${JSON.stringify({ type: "tool-input-start", toolCallId: "tc1", toolName: "search_web" })}\n\n`,
    `data: ${JSON.stringify({ type: "tool-input-available", toolCallId: "tc1", input: { query: "q" } })}\n\n`,
    `data: ${JSON.stringify({ type: "tool-output-available", toolCallId: "tc1", output: "ok" })}\n\n`,
    `data: ${JSON.stringify({ type: "sync_end", last_seq: 4 })}\n\n`,
  ];
  // assert: после onEnvelopeApplied в store ровно ОДИН коммит, содержащий
  // tool-part {toolCallId:"tc1", state:"output-available"}; повторное
  // подключение с ТЕМ ЖЕ конвертом даёт идентичное состояние (идемпотентность).
});
```
- [ ] **Step 2: batch-режим stream-processor.** В `StreamSession`/processSSEStream добавить флаг `batchMode`: `case "sync_begin"` → `session.beginBatch()` (scheduleCommit становится no-op, буфер копится) + колбэк boundary; `case "sync_end"` → `session.endBatch()` (один `commit()`), колбэк onEnvelopeApplied. Прочитать `stream-buffer.ts`/`stream-session.ts` — реализовать минимально (флаг + guard в scheduleCommit + форс-commit). ВАЖНО: на `sync_begin` буфер/parts сбрасываются (rebuild-from-log — спека §4.2), т.е. `session.buffer.reset()` перед батчем.
- [ ] **Step 3: `chat-stream.ts`.** `startTurn` — тонкий `apiPost` (возвращает 202-тело). `openTurnStream` — fetch GET `/api/chat/{sid}/stream?agent=…` (Bearer, `signal: session.signal`), `processSSEStream(session, resp.body, { callbacks: … })` с новыми колбэками; никаких reconnect-петель ВНУТРИ модуля — обрыв без finish → `onConnectionLost()`, решение у вызывающего (T8 visibility). 401 → `handleUnauthorized()` (как в renderer:207-209).
- [ ] **Step 4:** `cd ui && npm test -- chat-stream` → PASS; `npx tsc --noEmit` → exit-0 (модуль ещё не подключён — старый путь живёт); eslint по новым файлам.
- [ ] **Step 5: Commit**
```bash
git add ui/src/stores/stream/chat-stream.ts ui/src/stores/stream/stream-processor.ts ui/src/stores/stream/__tests__/chat-stream.test.ts
git commit -m "feat(stream-ui): chat-stream transport — 202 start + envelope batch-apply (standalone)"
```

---

### Task 7: Клиент — cutover отправки/подключения (единый код-путь)

**Files:**
- Modify: `ui/src/stores/chat/actions/stream-control.ts` (sendMessage/interruptAndSend/resumeStream → единый путь)
- Modify: `ui/src/stores/streaming-renderer.ts` (startStream/resumeStream → тонкие обёртки над chat-stream)
- Modify: `ui/src/stores/chat-types.ts` (`boundaryMessageId: string | null` в AgentState; фазы сжать до `"idle" | "submitted" | "streaming"`)
- Test: `ui/src/stores/chat/actions/__tests__/stream-control.single-path.test.ts`

**Interfaces:**
- Consumes: `startTurn`/`openTurnStream` (T6).
- Produces (потребляет T8): `renderer.connect(agent, sessionId)` — единственная точка подключения (и после POST, и на mount, и после обрыва); `AgentState.boundaryMessageId`.

**⚠️ Красное окно:** между T7 и T8 часть старых тестов (resume/dedup) может быть красной — это координированный cutover, зелёность восстанавливает T8/T9. tsc держать exit-0 в конце КАЖДОЙ задачи.

- [ ] **Step 1:** `sendMessage`: optimistic user-echo (как startStream:384-401, сохраняется) → `startTurn(agent, body)` (тело собирается как startStream:407-443 — leaf_message_id/attachments/force_new логика НЕ меняется) → из 202 взять `session_id` → `saveLastSession` → `renderer.connect(agent, session_id)`. `interruptAndSend` сохраняет abort-POST + затем тот же путь. `resumeStream(agent, sid)` → `renderer.connect(agent, sid)`.
- [ ] **Step 2:** `renderer.connect`: dispose прежней StreamSession (generation bump, как сейчас) → новая session → `openTurnStream` с колбэками: `onBoundary` → `update(agent,{boundaryMessageId})`; `onEnvelopeApplied` → phase "streaming"; `onFinished` → phase idle + invalidate sessions/messages (id-based handoff — T8 доводит рендер); `onConnectionLost` → phase "submitted" + (T8 повесит staleness/visibility-переоткрытие; в T7 — немедленный одноразовый reconnect `connect()` без счётчиков).
- [ ] **Step 3: Тест** `stream-control.single-path.test.ts` (реальный store, мок `chat-stream`):
```ts
it("sendMessage posts then connects with returned session id", async () => { /* startTurn mock → {session_id:"s1"}; assert connect called with ("main","s1") */ });
it("refresh path uses the SAME connect", () => { /* resumeStream("main","s1") → connect("main","s1") */ });
```
- [ ] **Step 4:** `cd ui && npm test -- single-path chat-stream` → PASS; `npx tsc --noEmit` exit-0.
- [ ] **Step 5: Commit**
```bash
git add ui/src/stores
git commit -m "feat(stream-ui): single connect path — POST 202 then GET envelope; boundary in agent state"
```

---

### Task 8: Клиент — boundary-рендер, handoff, visibility; удаление машины

**Files:**
- Delete: `ui/src/stores/chat-overlay-dedup.ts`, `ui/src/stores/stream/stream-reconnect.ts`
- Modify: `ui/src/stores/chat-history.ts` (live-merge → boundary-фильтр), `ui/src/app/(authenticated)/chat/ChatThread.tsx` (auto-resume эффект → connect; merge-вызовы), `ui/src/stores/chat/actions/navigation.ts` (`activeSessionIds` — удалить), `ui/src/stores/chat-types.ts` (`lastEventId`, `reconnectAttempt`, `activeSessionIds`, `MAX_RECONNECT_ATTEMPTS` — удалить), `ui/src/stores/streaming-renderer.ts` (visibility-хендлер упростить)
- Test: `ui/src/stores/__tests__/boundary-render.test.ts`

**Interfaces:**
- Consumes: `boundaryMessageId` (T7), `renderer.connect` (T7).
- Produces: рендер-контракт: `visibleHistory = boundaryMessageId ? historyUpToIncluding(history, boundaryMessageId) : history; rendered = [...visibleHistory, ...liveTurnMessages]` — контентного дедупа НЕТ.

- [ ] **Step 1: Failing-тест boundary-фильтра** (чистая функция в chat-history.ts):
```ts
import { historyUpToIncluding } from "../chat-history";
import type { ChatMessage } from "../chat-types";
const msg = (id: string): ChatMessage =>
  ({ id, role: "assistant", parts: [{ type: "text", text: id }], createdAt: "", status: "done" }) as ChatMessage;
const h = [msg("a"), msg("b"), msg("c")];
it("cuts history strictly after boundary id", () => {
  expect(historyUpToIncluding(h, "b").map((m) => m.id)).toEqual(["a", "b"]);
});
it("boundary id not found → full history (safe)", () => {
  expect(historyUpToIncluding(h, "zzz")).toHaveLength(3);
});
```
- [ ] **Step 2:** Реализовать `historyUpToIncluding` (позиционный срез по индексу найденного id; not found → вся история). Подключить в точке, где сейчас вызывается `mergeLiveOverlay` (grep по chat-history/ChatThread/MessageList) — заменить merge+dedup на `boundary-фильтр + конкатенация live`. Удалить `chat-overlay-dedup.ts` и его импорты/тесты.
- [ ] **Step 3: handoff на finish:** в `onFinished`-колбэке (T7): invalidate `sessionMessages`; после успешного refetch, если история содержит СВЕЖИЕ сообщения хода (по id из live turn — сравнить последний assistant message id), — `boundaryMessageId = null` + очистить live-сообщения (`messageSource: {mode:"history"}`); если ещё нет — оставить live-рендер до следующего refetch (идемпотентно). Реализовать как маленький эффект/подписку на query-результат в ChatThread (там уже есть подписки на sessionMessages).
- [ ] **Step 4: Удаления.** `stream-reconnect.ts` + reconnect-счётчики/`MAX_RECONNECT_ATTEMPTS`; `lastEventId` полностью; `activeSessionIds` (navigation.ts:226-246, ChatThread:62-97 auto-resume эффект → безусловный `connect(agent, activeSessionId)` на смене сессии — сервер сам ответит пустым конвертом, если хода нет); `isSessionFinishedInCache`/`settleAsFinished`/`scheduleReconnect`-вызовы из renderer. Visibility-хендлер (renderer:523-553) упростить: staleness 15s (константа `VISIBILITY_STALE_MS = 15_000`) + foreground → `connect(agent, sid)` (тот же путь, без abortLocalOnly-каскадов — connect сам dispose'ит прежнюю session). `onConnectionLost` (T7) тоже ведёт в staleness-переоткрытие с этой константой.
- [ ] **Step 5:** Обновить/удалить затронутые тесты (grep `lastEventId|activeSessionIds|overlay-dedup|scheduleReconnect|mergeLiveOverlay` в `ui/src`). `cd ui && npm test` → ПОЛНЫЙ набор зелёный (включая голосовые/T4-очередь — модель parts не менялась); `npx tsc --noEmit` exit-0; eslint чист.
- [ ] **Step 6: Commit**
```bash
git add -A ui/src
git commit -m "feat(stream-ui): boundary render + id-based handoff; delete resume/dedup/reconnect machinery"
```

---

### Task 9: Полная локальная верификация + правка интеграционных хвостов

**Files:** по факту находok.

- [ ] **Step 1:** `cd ui && npx tsc --noEmit` (exit-0) && `npm test` (все зелёные) && `npm run build` (exit-0). `cargo check --all-targets -p opex-core` → 0.
- [ ] **Step 2:** Grep-инварианты: `Last-Event-ID` — 0 вхождений в ui/src и chat-хендлерах; `chat-overlay-dedup` — файла нет; `activeSessionIds` — 0; `voiceTurnPending`/`tts-speaker` — не тронуты (голосовой контур цел).
- [ ] **Step 3:** Починить всё, что всплыло (это задача-гейт). Commit остатков: `fix(stream-ui): integration tails after cutover`.

---

### Task 10: Серверный гейт → деплой → E2E (по подтверждению пользователя)

- [ ] **Step 1 (после push с одобрения):** сервер: `git pull` → `cargo clippy --bin opex-core -- -D warnings` (clean) → `cargo test --bin opex-core -- --test-threads=4` (зелёный, throttled).
- [ ] **Step 2:** `bash scripts/server-deploy.sh` + локально `bash scripts/deploy-ui.sh` (билд локальный). Деплоить ПАРОЙ (протокол несовместим со старым UI — спека §7).
- [ ] **Step 3: E2E-чеклист (спека §8):**
  - отправка хода: POST→202→стрим, ответ печатается live;
  - **refresh ПОСЛЕ завершённого хода → мгновенно чистая история, реплей не «шляется» (исходный баг);**
  - refresh ПОСРЕДИ tool-хода → конверт восстановил текст+tool-карточки без дублей, live продолжился;
  - закрыть вкладку на 2 мин посреди хода → вернуться → ход дорешался (движок не абортился);
  - телефон/фоновая вкладка ≥1 мин → foreground → догнал (visibility);
  - вторая вкладка той же сессии — обе живые;
  - Stop работает (abort-POST); `/v1/chat/completions` не тронут (curl-проба);
  - голосовой ход со стриминговой озвучкой работает (tts-speaker питается прежней parts-моделью).
- [ ] **Step 4:** Ledger/память. Следующим релизом (отдельно) — снос уже-ненужных серверных легаси-веток, если что-то осталось.

---

## Порядок и зависимости

```text
T1 (wire types) → T2 (registry) → T3 (POST=202) → T4 (GET envelope) → T5 (server gate)
T6 (chat-stream standalone) ← T1..T4 контракт          [после T5]
T7 (single path cutover) ← T6
T8 (boundary/handoff/deletions) ← T7                    [красное окно тестов T7→T8 допустимо]
T9 (полная верификация) → T10 (деплой парой + E2E, по одобрению)
```
