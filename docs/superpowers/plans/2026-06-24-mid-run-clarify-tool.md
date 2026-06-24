# Mid-run clarify tool — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Дать агенту инструмент `clarify`, которым он задаёт уточняющий вопрос пользователю посреди хода (блокирующе ждёт ответа) и продолжает тот же tool-loop.

**Architecture:** Отдельный `ClarifyManager` — зеркало `approval_manager.rs` (oneshot-waiter + timeout + heartbeat + reverse-index для clear_session). `clarify` — system-tool: детектит интерактивный канал, доставляет вопрос (web `StreamEvent`/`SseEvent` + channel inline-кнопки/текст), блокирующе ждёт ответ, возвращает JSON. Резолв: `POST /api/clarify/{id}` (web) + reader/inline intercept (channel).

**Tech Stack:** Rust 2024, tokio (oneshot, time::timeout), dashmap, sqlx, axum, ts-rs (SSE codegen), Next.js (ClarifyCard). Тесты: `cargo test`, DB-тесты `#[sqlx::test]`.

## Global Constraints

- rustls-tls only, без OpenSSL.
- Блокирующая семантика (как approval); clarify — sequential-only (НЕ добавлять в `is_system_tool_parallel_safe`).
- Detection канала ОБЯЗАТЕЛЬНА до блокировки: нет интерактивного канала → немедленно «not available» (иначе cron/openai-compat зависнут на timeout).
- Heartbeat (`touch_session_activity`) поллингом при ожидании.
- `clarify_timeout_secs` default 600; для channel-пути не срубаться `request_timeout_secs`.
- `clarify` в `SUBAGENT_DENIED_TOOLS`.
- Коммиты без `Co-Authored-By`. master. Не пушить без явного разрешения.
- Верификация (make нет): `cargo check --all-targets`; `cargo clippy --all-targets -- -D warnings`; DB-тесты `DATABASE_URL=postgres://opex_test:opex_test@127.0.0.1:5434/opex_test cargo test --bin opex-core <filter>`. UI: `cd ui && npx vitest run <path>` + `npm run build`.

---

## File Structure

- **Create** `crates/opex-core/src/agent/clarify_manager.rs` — `ClarifyManager`, `ClarifyWaitersMap`, `ClarifyOutcome`, create_and_wait/resolve/clear_session/has_pending_text + unit-тесты.
- **Modify** `crates/opex-core/src/agent/mod.rs` — `pub mod clarify_manager;`
- **Modify** `crates/opex-core/src/agent/agent_config.rs` — поле `clarify_manager: Arc<ClarifyManager>` (рядом с `approval_manager`).
- **Modify** `crates/opex-core/src/agent/tool_registry.rs` — регистрация `clarify` system-tool handler.
- **Create** `crates/opex-core/src/agent/tool_handlers/clarify.rs` — handler (detection + choices-норм + вызов менеджера).
- **Modify** `crates/opex-core/src/agent/pipeline/tool_defs.rs` — `ToolDefinition` для `clarify` (схема).
- **Modify** `crates/opex-core/src/agent/stream_event.rs` — `StreamEvent::ClarifyNeeded`.
- **Modify** `crates/opex-types/src/sse.rs` — wire `SseEvent::ClarifyNeeded` (ts-gen).
- **Modify** `crates/opex-core/src/gateway/handlers/chat/sse_converter.rs` — arm `ClarifyNeeded`.
- **Create** `crates/opex-core/src/gateway/handlers/clarify.rs` — `POST /api/clarify/{id}` + `routes()`.
- **Modify** `crates/opex-core/src/gateway/mod.rs` — merge clarify routes.
- **Modify** `crates/opex-core/src/gateway/handlers/channel_ws/inline.rs` — `handle_clarify_callback` (button).
- **Modify** `crates/opex-core/src/gateway/handlers/channel_ws/reader.rs` — clarify text-intercept (после approval/fse, перед dispatch).
- **Modify** `crates/opex-core/src/agent/pipeline/finalize.rs` (или `SessionLifecycleGuard`) — `clear_session` на teardown.
- **Modify** `crates/opex-core/src/agent/pipeline/subagent.rs` — `clarify` в `SUBAGENT_DENIED_TOOLS`.
- **Modify** `crates/opex-core/src/config/mod.rs` — `clarify_timeout_secs` (+default 600).
- **Create** `ui/src/components/chat/ClarifyCard.tsx` + регистрация рядом с ApprovalCard.

---

### Task 1: `ClarifyManager` ядро (без доставки)

**Files:**
- Create: `crates/opex-core/src/agent/clarify_manager.rs`
- Modify: `crates/opex-core/src/agent/mod.rs` (`pub mod clarify_manager;`)
- Test: в `clarify_manager.rs` (`#[cfg(test)]`)

**Interfaces:**
- Produces:
  - `pub type ClarifyWaitersMap = Arc<DashMap<Uuid, (tokio::sync::oneshot::Sender<String>, Instant)>>`
  - `pub enum ClarifyOutcome { Answered(String), NoResponse(NoResponseReason) }`
  - `pub enum NoResponseReason { TimedOut, Cancelled }`
  - `pub struct ClarifyManager { db, waiters, by_session }`
  - `pub fn new(db, waiters) -> Self`; `register(session_id, choices_empty) -> (Uuid, oneshot::Receiver<String>)`; `async fn wait(id, session_id, timeout, db) -> ClarifyOutcome`; `fn resolve(id, response) -> bool`; `fn clear_session(session_id) -> usize`; `fn has_pending_text(session_id) -> Option<Uuid>`; `fn waiters() -> &ClarifyWaitersMap`.

> Разделяем на `register` (создать waiter+rx, не блокируя) и `wait` (блокирующий с heartbeat) — чтобы доставка (Task 5/6) встроилась между ними в handler. `awaiting_text` (open-ended) трекается через by_session-флаг: храним `by_session: DashMap<Uuid, Vec<(Uuid, bool /*awaiting_text*/)>>`.

- [ ] **Step 1: Написать тесты ядра.**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn mgr() -> ClarifyManager {
        // db не нужен для register/resolve/clear; для wait-heartbeat тесты используют短 timeout.
        ClarifyManager::new_for_test()
    }

    #[tokio::test]
    async fn register_resolve_returns_answer() {
        let m = mgr();
        let sid = Uuid::new_v4();
        let (id, rx) = m.register(sid, true);
        assert!(m.resolve(id, "blue".into()));
        let out = m.wait_rx(rx, sid, Duration::from_secs(5)).await;
        assert!(matches!(out, ClarifyOutcome::Answered(a) if a == "blue"));
    }

    #[tokio::test]
    async fn timeout_yields_no_response() {
        let m = mgr();
        let sid = Uuid::new_v4();
        let (_id, rx) = m.register(sid, true);
        let out = m.wait_rx(rx, sid, Duration::from_millis(50)).await;
        assert!(matches!(out, ClarifyOutcome::NoResponse(NoResponseReason::TimedOut)));
    }

    #[tokio::test]
    async fn clear_session_cancels_pending() {
        let m = mgr();
        let sid = Uuid::new_v4();
        let (_id, rx) = m.register(sid, false);
        assert_eq!(m.clear_session(sid), 1);
        let out = m.wait_rx(rx, sid, Duration::from_secs(5)).await;
        assert!(matches!(out, ClarifyOutcome::NoResponse(NoResponseReason::Cancelled)));
    }

    #[test]
    fn has_pending_text_returns_open_ended_only() {
        let m = mgr();
        let sid = Uuid::new_v4();
        let (_btn, _rx1) = m.register(sid, false); // choices present → not awaiting_text
        let (open, _rx2) = m.register(sid, true);  // open-ended → awaiting_text
        assert_eq!(m.has_pending_text(sid), Some(open));
    }
}
```

- [ ] **Step 2: Запустить — убедиться, что не компилируется/падает.**

Run: `cargo test -p opex-core clarify_manager -- --nocapture`
Expected: FAIL — типы/методы не существуют.

- [ ] **Step 3: Реализовать ядро.** (heartbeat в `wait_rx`: цикл `tokio::time::timeout(min(1s,remaining), &mut rx)` + `touch_session_activity` между итерациями; `db: Option<PgPool>` — в тестах None, touch пропускается).

```rust
use std::sync::Arc;
use std::time::{Duration, Instant};
use dashmap::DashMap;
use sqlx::PgPool;
use uuid::Uuid;

pub type ClarifyWaitersMap = Arc<DashMap<Uuid, (tokio::sync::oneshot::Sender<String>, Instant)>>;

#[derive(Debug)]
pub enum NoResponseReason { TimedOut, Cancelled }
#[derive(Debug)]
pub enum ClarifyOutcome { Answered(String), NoResponse(NoResponseReason) }

pub struct ClarifyManager {
    db: Option<PgPool>,
    waiters: ClarifyWaitersMap,
    by_session: Arc<DashMap<Uuid, Vec<(Uuid, bool)>>>, // session → [(clarify_id, awaiting_text)]
}

impl ClarifyManager {
    pub fn new(db: PgPool, waiters: ClarifyWaitersMap) -> Self {
        Self { db: Some(db), waiters, by_session: Arc::new(DashMap::new()) }
    }
    #[cfg(test)]
    pub fn new_for_test() -> Self {
        Self { db: None, waiters: Arc::new(DashMap::new()), by_session: Arc::new(DashMap::new()) }
    }
    pub fn waiters(&self) -> &ClarifyWaitersMap { &self.waiters }

    /// Create a waiter; returns (clarify_id, receiver). `awaiting_text` = open-ended.
    pub fn register(&self, session_id: Uuid, awaiting_text: bool)
        -> (Uuid, tokio::sync::oneshot::Receiver<String>) {
        let id = Uuid::new_v4();
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.waiters.insert(id, (tx, Instant::now()));
        self.by_session.entry(session_id).or_default().push((id, awaiting_text));
        (id, rx)
    }

    /// Block on rx until resolved or timeout, touching session activity each ~1s.
    pub async fn wait_rx(
        &self,
        mut rx: tokio::sync::oneshot::Receiver<String>,
        session_id: Uuid,
        timeout: Duration,
    ) -> ClarifyOutcome {
        let deadline = Instant::now() + timeout;
        let out = loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() { break ClarifyOutcome::NoResponse(NoResponseReason::TimedOut); }
            match tokio::time::timeout(remaining.min(Duration::from_secs(1)), &mut rx).await {
                Ok(Ok(answer)) => break ClarifyOutcome::Answered(answer),
                Ok(Err(_)) => break ClarifyOutcome::NoResponse(NoResponseReason::Cancelled),
                Err(_) => {
                    if let Some(db) = &self.db {
                        let _ = crate::db::sessions::touch_session_activity(db, session_id).await;
                    }
                }
            }
        };
        self.forget(session_id);
        out
    }

    pub fn resolve(&self, id: Uuid, response: String) -> bool {
        if let Some((_, (tx, _))) = self.waiters.remove(&id) {
            tx.send(response).is_ok()
        } else { false }
    }

    pub fn clear_session(&self, session_id: Uuid) -> usize {
        let ids: Vec<Uuid> = self.by_session.get(&session_id)
            .map(|v| v.iter().map(|(id, _)| *id).collect()).unwrap_or_default();
        let mut n = 0;
        for id in ids {
            if self.waiters.remove(&id).is_some() { n += 1; } // drop sender → Cancelled
        }
        self.by_session.remove(&session_id);
        n
    }

    pub fn has_pending_text(&self, session_id: Uuid) -> Option<Uuid> {
        self.by_session.get(&session_id)
            .and_then(|v| v.iter().find(|(id, at)| *at && self.waiters.contains_key(id)).map(|(id, _)| *id))
    }

    fn forget(&self, session_id: Uuid) {
        if let Some(mut v) = self.by_session.get_mut(&session_id) {
            v.retain(|(id, _)| self.waiters.contains_key(id));
        }
    }
}
```

Добавить в `mod.rs`: `pub mod clarify_manager;`.

- [ ] **Step 4: Запустить тесты.**

Run: `cargo test -p opex-core clarify_manager -- --nocapture`
Expected: PASS (4 теста).

- [ ] **Step 5: Commit.**

```bash
git add crates/opex-core/src/agent/clarify_manager.rs crates/opex-core/src/agent/mod.rs
git commit -m "feat(clarify): ClarifyManager ядро (waiter + heartbeat + clear_session)"
```

---

### Task 2: Wire ClarifyManager в AgentConfig

**Files:**
- Modify: `crates/opex-core/src/agent/agent_config.rs` (поле + конструктор, рядом с `approval_manager`)
- Modify: место создания `AgentConfig` (где создаётся `approval_manager` + `ApprovalWaitersMap` — grep `ApprovalManager::new`)

**Interfaces:**
- Produces: `cfg.clarify_manager: Arc<ClarifyManager>` + общий `ClarifyWaitersMap` (как approval).

- [ ] **Step 1: Прочитать, как создаётся `approval_manager`.** `grep -rn "ApprovalManager::new\|ApprovalWaitersMap\|approval_manager:" crates/opex-core/src` — найти поле в `agent_config.rs` (~42) и конструктор.

- [ ] **Step 2: Добавить поле и инициализацию по образцу approval.**

```rust
// agent_config.rs (рядом с approval_manager)
pub clarify_manager: Arc<crate::agent::clarify_manager::ClarifyManager>,
```
В конструкторе (где создаётся approval): создать `let clarify_waiters: ClarifyWaitersMap = Arc::new(DashMap::new()); let clarify_manager = Arc::new(ClarifyManager::new(db.clone(), clarify_waiters));` и присвоить.

- [ ] **Step 3: Проверка.** Run: `cargo check -p opex-core` → зелёный.

- [ ] **Step 4: Commit.**

```bash
git add crates/opex-core/src/agent/agent_config.rs <constructor-file>
git commit -m "feat(clarify): ClarifyManager в AgentConfig"
```

---

### Task 3: `clarify` system-tool — схема, detection, choices-нормализация

**Files:**
- Create: `crates/opex-core/src/agent/tool_handlers/clarify.rs`
- Modify: `crates/opex-core/src/agent/tool_registry.rs` (регистрация handler)
- Modify: `crates/opex-core/src/agent/pipeline/tool_defs.rs` (ToolDefinition `clarify`)
- Test: в `clarify.rs`

**Interfaces:**
- Consumes: `ClarifyManager` (Task 1), `ToolDeps { cfg, state, tex, db, session_id, agent_name }`.
- Produces: `pub fn normalize_choices(raw: &serde_json::Value) -> Vec<String>`; `pub fn channel_available(ctx: &serde_json::Value) -> bool`.

- [ ] **Step 1: Тесты detection + choices.**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn channel_available_true_for_chat_id_or_ui() {
        assert!(channel_available(&json!({"chat_id": "123"})));
        assert!(channel_available(&json!({"_channel": "ui"})));
    }
    #[test]
    fn channel_available_false_for_cron_or_inter_agent() {
        assert!(!channel_available(&json!({"_channel": "inter_agent"})));
        assert!(!channel_available(&json!({})));
    }
    #[test]
    fn normalize_choices_flattens_and_caps() {
        let v = json!(["a", {"label": "b"}, {"description": "c"}, "d", "e"]);
        assert_eq!(normalize_choices(&v), vec!["a","b","c","d"]); // >4 → 4, dict→str
    }
    #[test]
    fn normalize_choices_empty_for_none() {
        assert!(normalize_choices(&json!(null)).is_empty());
    }
}
```

- [ ] **Step 2: Запустить — FAIL.** Run: `cargo test -p opex-core clarify::tests` → FAIL.

- [ ] **Step 3: Реализовать helpers + handler.**

```rust
// channel detection: chat_id present OR _channel == "ui"
pub fn channel_available(ctx: &serde_json::Value) -> bool {
    if ctx.get("chat_id").and_then(|v| v.as_str()).is_some_and(|s| !s.is_empty() && s != "null") {
        return true;
    }
    ctx.get("_channel").and_then(|v| v.as_str()) == Some("ui")
}

// port of Hermes _flatten_choice + cap 4
pub fn normalize_choices(raw: &serde_json::Value) -> Vec<String> {
    let Some(arr) = raw.as_array() else { return Vec::new() };
    arr.iter().filter_map(|c| match c {
        serde_json::Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
        serde_json::Value::Object(o) => ["label","description","text","title"].iter()
            .find_map(|k| o.get(*k).and_then(|v| v.as_str()).map(|s| s.trim().to_string()))
            .filter(|s| !s.is_empty()),
        _ => None,
    }).take(4).collect()
}
```

Handler (по образцу существующих system-tool handlers, напр. `tool_handlers/agent_tool.rs`: `#[async_trait] impl SystemTool { async fn handle(&self, deps: ToolDeps<'_>, args: &Value) -> String }`):
1. `question` из args (required, non-empty → иначе error).
2. `ctx = args["_context"]`; если `!channel_available(ctx)` → вернуть `json!({"error":"clarify not available in this execution context"}).to_string()`.
3. `choices = normalize_choices(&args["choices"])`; `session_id = deps.session_id` (если None → «not available»).
4. `(id, rx) = deps.cfg.clarify_manager.register(session_id, choices.is_empty())`.
5. Доставка (Task 5/6 — пока заглушка: TODO-маркер заменится; на этом шаге доставить только web через sse_event_tx если есть, channel — в Task 6). Для прохождения Task 3 достаточно: emit web event (Task 4 даст событие) ИЛИ временно лог. **NB:** доставка наполняется в Task 5/6; здесь handler структурно готов.
6. `outcome = deps.cfg.clarify_manager.wait_rx(rx, session_id, Duration::from_secs(clarify_timeout)).await`.
7. Результат: `Answered(a)` → `json!({"question":question,"choices_offered":choices,"user_response":a})`; `NoResponse(_)` → `json!({"question":question,"user_response":"","note":"user did not respond; proceed with a reasonable default"})`.

Регистрация в `tool_registry.rs` (по образцу прочих handlers) + `ToolDefinition` в `tool_defs.rs` (name `clarify`, описание-порт из Hermes CLARIFY_SCHEMA, params question+choices≤4). **НЕ добавлять в `static_core_tool_names` parallel-safe.**

- [ ] **Step 4: Запустить тесты.** Run: `cargo test -p opex-core clarify -- --nocapture` + `cargo check -p opex-core` → PASS/зелёный.

- [ ] **Step 5: Commit.**

```bash
git add crates/opex-core/src/agent/tool_handlers/clarify.rs crates/opex-core/src/agent/tool_registry.rs crates/opex-core/src/agent/pipeline/tool_defs.rs
git commit -m "feat(clarify): system-tool (detection + choices-норм + waiter)"
```

---

### Task 4: Web SSE событие `ClarifyNeeded` (internal + wire + converter + codegen)

**Files:**
- Modify: `crates/opex-core/src/agent/stream_event.rs` (вариант `ClarifyNeeded`)
- Modify: `crates/opex-types/src/sse.rs` (wire `SseEvent::ClarifyNeeded`, ts-gen)
- Modify: `crates/opex-core/src/gateway/handlers/chat/sse_converter.rs` (arm)
- Modify: `ui/src/types/sse.generated.ts` (через codegen) + `ui/src/stores/sse-events.ts` (re-export, авто)

**Interfaces:**
- Produces: `StreamEvent::ClarifyNeeded { clarify_id: Uuid, question: String, choices: Vec<String>, timeout_ms: u64 }`; wire `SseEvent::ClarifyNeeded {...}`.

- [ ] **Step 1: internal StreamEvent.** В `stream_event.rs` рядом с `ApprovalNeeded` (~92):

```rust
    ClarifyNeeded {
        clarify_id: uuid::Uuid,
        question: String,
        choices: Vec<String>,
        timeout_ms: u64,
    },
```

- [ ] **Step 2: wire SseEvent + ts-gen.** В `crates/opex-types/src/sse.rs` рядом с `ToolApprovalNeeded` (~132):

```rust
    ClarifyNeeded {
        #[serde(rename = "clarifyId")]
        #[cfg_attr(feature = "ts-gen", ts(type = "string"))]
        clarify_id: uuid::Uuid,
        question: String,
        choices: Vec<String>,
        #[serde(rename = "timeoutMs")]
        timeout_ms: u64,
    },
```

- [ ] **Step 3: converter arm.** В `sse_converter.rs` рядом с arm `StreamEvent::ApprovalNeeded` (~373-381) добавить маппинг `StreamEvent::ClarifyNeeded {..} → SseEvent::ClarifyNeeded {..}` через `SseStreamWriter::build_pure` (тот же приём, что approval — non-text must-deliver).

- [ ] **Step 4: codegen TS.** Прочитать, как генерится `ui/src/types/sse.generated.ts` (grep Makefile/scripts `gen-types`/`ts-rs`). Запустить ту же команду (напр. `cargo test -p opex-types export_bindings` или `make gen-types`-аналог `cargo run ...`). Убедиться, что `ClarifyNeeded` появился в `sse.generated.ts`. `sse-events.ts` руками не трогать.

- [ ] **Step 5: Проверка.** Run: `cargo check --all-targets` → зелёный; `cd ui && npx tsc --noEmit` → зелёный (тип появился).

- [ ] **Step 6: Commit.**

```bash
git add crates/opex-core/src/agent/stream_event.rs crates/opex-types/src/sse.rs crates/opex-core/src/gateway/handlers/chat/sse_converter.rs ui/src/types/sse.generated.ts
git commit -m "feat(clarify): SSE-событие ClarifyNeeded (internal+wire+converter+codegen)"
```

---

### Task 5: Web-доставка + resolve endpoint

**Files:**
- Modify: `crates/opex-core/src/agent/tool_handlers/clarify.rs` (emit `ClarifyNeeded` через `deps.tex.sse_event_tx`)
- Create: `crates/opex-core/src/gateway/handlers/clarify.rs` (`POST /api/clarify/{id}` + `routes()`)
- Modify: `crates/opex-core/src/gateway/mod.rs` (merge `clarify::routes()`)

**Interfaces:**
- Consumes: `ClarifyManager.resolve` (Task 1), `StreamEvent::ClarifyNeeded` (Task 4).

- [ ] **Step 1: emit в handler.** В clarify-handler (Task 3, шаг доставки) для web: если `deps.tex.sse_event_tx` имеет sender — `send_async(StreamEvent::ClarifyNeeded { clarify_id: id, question, choices: choices.clone(), timeout_ms })` (must-deliver, как approval emit в approval_manager.rs:214).

- [ ] **Step 2: resolve endpoint.** `gateway/handlers/clarify.rs` (зеркало approval-resolve, `agents/crud.rs:1021` + route `agents/mod.rs:26`):

```rust
pub(crate) fn routes() -> axum::Router<crate::gateway::AppState> {
    axum::Router::new().route("/api/clarify/{id}", axum::routing::post(api_resolve_clarify))
}
// handler: extract id: Uuid, body {response: String}; найти agent/clarify_manager
// (через AppState — clarify_manager общий? см. NB ниже) и .resolve(id, response).
```

> NB: approval resolve находит waiter через `engine.resolve_approval` (per-agent engine). clarify аналогично — нужно достучаться до `clarify_manager`. Сверить, как approval-resolve route получает менеджер (через какой AppState-доступ к engine/waiters) и зеркалить точно.

- [ ] **Step 3: merge routes.** В `gateway/mod.rs` (рядом с `handlers::agents::routes()` ~99) добавить `.merge(handlers::clarify::routes())`.

- [ ] **Step 4: Integration-тест (web).** `#[sqlx::test]`: создать pending clarify (register), эмуляция `POST /api/clarify/{id}` → resolve → `wait_rx` возвращает Answered. (Если полный HTTP-тест тяжёл — unit на resolve-handler-логику + проверка route зарегистрирован.)

- [ ] **Step 5: Проверка + Commit.**

Run: `cargo check --all-targets && cargo test -p opex-core clarify`
```bash
git add crates/opex-core/src/agent/tool_handlers/clarify.rs crates/opex-core/src/gateway/handlers/clarify.rs crates/opex-core/src/gateway/mod.rs
git commit -m "feat(clarify): web-доставка (emit ClarifyNeeded) + POST /api/clarify/{id}"
```

---

### Task 6: Channel-доставка + button-callback + text-intercept

**Files:**
- Modify: `crates/opex-core/src/agent/tool_handlers/clarify.rs` (channel send через `deps.state.channel_router`)
- Modify: `crates/opex-core/src/gateway/handlers/channel_ws/inline.rs` (`handle_clarify_callback`)
- Modify: `crates/opex-core/src/gateway/handlers/channel_ws/reader.rs` (text-intercept)

**Interfaces:**
- Consumes: `ChannelActionRouter` (как approval channel send), `ClarifyManager.resolve`/`has_pending_text`.

- [ ] **Step 1: channel send в handler.** Если `chat_id` present — `deps.state.channel_router.send(action)`: choices → inline-кнопки с callback-data `clarify:{id}:{idx}` (+ «Other»-кнопка `clarify:{id}:other`); open-ended → текстовый вопрос. По образцу approval channel-action (approval_manager.rs:165-185).

- [ ] **Step 2: button-callback intercept.** В `inline.rs` добавить `handle_clarify_callback(&ctx, &engine, &agent_name, &request_id, &msg, &out_tx) -> bool` (зеркало `handle_approval_callback`): если `is_callback` и data `clarify:{id}:{idx}` → owner-gate → `clarify_manager.resolve(id, choice_text)` (idx→choice; `other` → flip awaiting_text, ждать текст). Зарегистрировать вызов в `reader.rs` рядом с approval/fse intercepts (~115-124).

- [ ] **Step 3: text-intercept.** В `reader.rs` ПЕРЕД `dispatch_message` (~126), ПОСЛЕ approval/fse intercepts: если `clarify_manager.has_pending_text(session)` И НЕТ активного approval-waiter для сессии (приоритет — ревью MED-4) → `resolve(id, msg_text)` + `continue`.

```rust
        // Clarify text-intercept (open-ended / "Other"). Priority: approval > clarify.
        let consumed_clarify = inline::handle_clarify_text(
            &ctx, &engine, &agent_name, &request_id, &msg, &out_tx,
        ).await;
        if consumed_clarify { continue; }
```

- [ ] **Step 4: Тест (mock router).** Integration: вопрос отправлен в router (захват action); text-intercept резолвит самый старый pending; при активном approval — clarify-text НЕ перехватывает.

- [ ] **Step 5: Проверка + Commit.**

Run: `cargo check --all-targets && cargo test -p opex-core clarify`
```bash
git add crates/opex-core/src/agent/tool_handlers/clarify.rs crates/opex-core/src/gateway/handlers/channel_ws/inline.rs crates/opex-core/src/gateway/handlers/channel_ws/reader.rs
git commit -m "feat(clarify): channel-доставка (inline-кнопки + text-intercept, приоритет approval)"
```

---

### Task 7: Teardown hook + config + denylist

**Files:**
- Modify: `crates/opex-core/src/agent/pipeline/finalize.rs` (или `SessionLifecycleGuard`) — `clear_session`
- Modify: `crates/opex-core/src/config/mod.rs` (`clarify_timeout_secs` + default 600 + channel timeout-согласование)
- Modify: `crates/opex-core/src/agent/pipeline/subagent.rs` (`SUBAGENT_DENIED_TOOLS`)

- [ ] **Step 1: teardown.** В `finalize.rs` (где ход завершается/прерывается — `SessionLifecycleGuard` Drop или явный finalize) вызвать `cfg.clarify_manager.clear_session(session_id)`, чтобы висящий waiter не ждал весь timeout при interrupt/re-entry. Прочитать finalize, встроить по факту.

- [ ] **Step 2: config.** В `config/mod.rs` рядом с `request_timeout_secs` (~487) добавить `clarify_timeout_secs: u64` + `default_clarify_timeout() -> u64 { 600 }`. Для channel-пути: где `reader.rs:133` передаёт `request_timeout_secs` в dispatcher — clarify-ожидание не должно срубаться раньше. Реализация: dispatcher-timeout для хода с pending clarify = `max(request_timeout_secs, clarify_timeout_secs)` (детально — прочитать dispatcher timeout-применение и встроить; либо documented-acceptable: `clarify_timeout_secs <= request_timeout_secs`, тогда дефолт clarify сделать 280, но это меняет UX — предпочесть max()).

- [ ] **Step 3: denylist + тест.** `subagent.rs` `SUBAGENT_DENIED_TOOLS` += `"clarify"`. Unit: `assert!(SUBAGENT_DENIED_TOOLS.contains(&"clarify"))`.

- [ ] **Step 4: Проверка + Commit.**

Run: `cargo check --all-targets && cargo test -p opex-core subagent`
```bash
git add crates/opex-core/src/agent/pipeline/finalize.rs crates/opex-core/src/config/mod.rs crates/opex-core/src/agent/pipeline/subagent.rs
git commit -m "feat(clarify): teardown clear_session + config timeout + subagent denylist"
```

---

### Task 8: Web UI `ClarifyCard`

**Files:**
- Create: `ui/src/components/chat/ClarifyCard.tsx`
- Modify: место рендера SSE-карточек (где рендерится `ApprovalCard` — grep `ApprovalCard`)

- [ ] **Step 1: Прочитать `ApprovalCard.tsx`** и точку его рендера (как `data-*`/event-type матчится). Сделать `ClarifyCard` по образцу: рендерит `question`, `choices` как кнопки + «Other» (текст-инпут); на выбор/submit → `POST /api/clarify/{clarifyId}` `{response}`.
- [ ] **Step 2: Подключить** к рендеру по событию `ClarifyNeeded` (как ApprovalCard по `ToolApprovalNeeded`).
- [ ] **Step 3: Проверка.** Run: `cd ui && npx tsc --noEmit && npm run build` → зелёный. (Тест компонента — vitest, если есть паттерн для ApprovalCard.)
- [ ] **Step 4: Commit.**

```bash
git add ui/src/components/chat/ClarifyCard.tsx <render-file>
git commit -m "feat(clarify): web ClarifyCard"
```

---

### Task 9: Полная проверка + регрессии

- [ ] **Step 1:** `cargo check --all-targets && cargo clippy --all-targets -- -D warnings` → зелёные (почини clippy от нового кода, напр. too_many_arguments на create_and_wait — добавить `#[allow]` если нужно).
- [ ] **Step 2:** `DATABASE_URL=postgres://opex_test:opex_test@127.0.0.1:5434/opex_test cargo test --bin opex-core 2>&1 | tail -20` → все зелёные (clarify_manager, clarify, subagent, + регресс approval/channel_ws).
- [ ] **Step 3:** `cd ui && npm run build` → зелёный.
- [ ] **Step 4:** Финальный коммит/сводка.

```bash
git add -A && git commit -m "test(clarify): зелёный прогон" || echo "nothing"
git log --oneline -10
```

---

## Self-Review (выполнено автором плана)

- **Покрытие спеки:** ClarifyManager+heartbeat (T1); AgentConfig wire (T2); tool+detection+choices (T3); SSE codegen (T4); web-доставка+resolve (T5); channel-доставка+intercept+приоритет (T6); teardown+config+denylist (T7); ClarifyCard (T8); проверка (T9). Sequential-only — обеспечено отсутствием в `is_system_tool_parallel_safe` (T3 NB).
- **Плейсхолдеры:** где точная структура неизвестна без чтения (AgentConfig конструктор T2, finalize hook T7, resolve-route доступ к менеджеру T5 NB, ApprovalCard render-точка T8) — даны инструкции «прочитать образец X и зеркалить», с точным file:line образца; код-шаги несут реальный код.
- **Типы:** `ClarifyOutcome`/`NoResponseReason`/`register`/`wait_rx`/`resolve`/`clear_session`/`has_pending_text`/`normalize_choices`/`channel_available` — единые во всех задачах; `StreamEvent::ClarifyNeeded`/`SseEvent::ClarifyNeeded` поля согласованы (clarify_id/question/choices/timeout_ms).
