# Runtime User-Hooks (decision-webhooks)

**Дата:** 2026-06-24
**Статус:** Design v2 (одобрено в brainstorming + адверсариальное ревью учтено; ожидает финального spec review)
**Hermes-референс:** `D:/GIT/hermes-agent` — `gateway/hooks.py` (event registry), `agent/shell_hooks.py` (stdin/stdout JSON wire protocol, matcher, timeout), `hermes_cli/plugins.py` (VALID_HOOKS).

## Цель

Дать оператору определять **хуки в рантайме** (без пересборки), способные наблюдать/ветировать/модифицировать поток агента — через **синхронные decision-webhooks**. Третья и последняя фича Hermes-parity цикла (после mid-run clarify и checkpoint/rollback).

## Контекст (как сейчас — проверено по коду)

- **`crates/opex-core/src/agent/hooks.rs`** — `HookRegistry`:
  - `handlers: Vec<(String, HookHandler)>`, `http_client: Option<reqwest::Client>`, `webhooks: Vec<WebhookConfig>`.
  - `HookHandler = Box<dyn Fn(&HookEvent) -> HookAction + Send + Sync>` (compile-time closures).
  - `HookEvent { BeforeMessage, AfterResponse, BeforeToolCall{agent,tool_name}, AfterToolResult{agent,tool_name,duration_ms}, OnError }` (реально дёргаются BeforeMessage/BeforeToolCall/AfterToolResult).
  - `HookAction { Continue, Block(String) }` — синхронный результат.
  - `fire(&event) -> HookAction` (sync, closures); `fire_webhooks(&event)` — async **fire-and-forget** через `tokio::spawn` + 5s timeout, **результат игнорируется** (нельзя вернуть block/modify).
  - `set_webhooks(client, vec)` сохраняет SSRF-клиент + конфиг; `register(name, handler)`.
- **Точки fire:** `engine/run.rs:34` (BeforeMessage sync-fire, до bootstrap — здесь контекст ЕЩЁ не построен, inject невозможен); `engine_dispatch.rs:142` (BeforeToolCall, после approval — `arguments: &Value` доступны во inner; результат block используется); `engine_dispatch.rs:50` (AfterToolResult outer, с duration_ms + результат, результат НЕ используется). **Approval уже умеет `ApprovedWithModifiedArgs` (engine_dispatch.rs:121-128) — паттерн rebind args.**
- **Параллельные инструменты:** `parallel.rs:401-443` исполняет tool-batch через `join_all` — BeforeToolCall/AfterToolResult живут ВНУТРИ `execute_tool_call`, т.е. фаерятся конкурентно по инструментам (важно для семантики — см. ниже).
- **Конфиг** (`config/mod.rs`): `HooksConfig { log_all_tool_calls: bool, block_tools: Vec<String>, webhooks: Vec<WebhookConfig> }`, `WebhookConfig { url: String, events: Vec<String> }`. Парсится как `[agent.hooks]`. `lifecycle.rs:136-154` регистрирует: log→`logging_hook()`, block_tools→`block_tools_hook()`, webhooks→`set_webhooks(ssrf_http_client, …)`.
- **Hot-reload:** конфиг агента перечитывается (notify crate) → хуки пере-регистрируются при правке TOML/через UI (PUT /api/agents).
- **SSRF:** webhooks уже идут через `ssrf_http_client` (`net::ssrf`) — приватные IP блокируются кроме `is_internal_endpoint`.
- **Audit:** `AgentConfig.audit_queue: Arc<AuditQueue>` (`db/audit_queue.rs`) — очередь записей аудита.
- Исполнения пользовательского кода для хуков на хосте НЕТ (code_exec — Docker sandbox для агентского tool, не для хуков).

## Решения (brainstorming, 3 секции — одобрены)

### Механизм (Q1) — decision-webhooks

1. **Хук = синхронный webhook** (расширение существующих fire-and-forget). OPEX POSTит контекст события на admin-настроенный URL, парсит JSON-ответ для решения. **НЕТ исполнения кода на хосте** — пользовательская логика живёт во внешнем HTTP-сервисе. Переиспользует webhook-инфру + SSRF-защиту. (Выбрано против shell-команд и гибрида — OPEX-native, минимум поверхности.)

### Возможности (Q2) — полный набор

2. Три события — синхронные decision-точки; webhook может вернуть:
   - **BeforeMessage**: `block` (отклонить ход) ИЛИ `inject_context` (добавить динамический контекст в промпт).
   - **BeforeToolCall**: `block` (вето инструмента) ИЛИ `modified_args` (заменить аргументы).
   - **AfterToolResult**: `transformed_result` (заменить результат до LLM; block неприменим — инструмент отработал).
3. `modified_args`/`transformed_result` = осознанно принятый инъекционный вектор (Q2 «полный»), смягчается audit + provenance (см. Безопасность).

### Failure (Q3) — configurable, default fail-open

4. На decision-webhook: timeout/ошибка/невалидный JSON → `on_failure`: **open** (default) = Continue (warn+audit); **closed** = Block(«hook unavailable»). Для transform-событий closed неприменим → при сбое оригинальный результат + warn.

## Non-goals (YAGNI)

- Исполнение пользовательского кода/скриптов на хосте (shell-hooks, Python handler.py) — выбраны webhooks.
- Полноструктурный provenance (графовый тег) — пока текстовый префикс + анти-spoof санитайз + audit.
- Re-валидация `modified_args` против JSON-схемы инструмента (handler сам валидирует свои аргументы).
- Новые события сверх трёх существующих fire-точек; `AfterResponse`/`OnError` (заявлены, но не дёргаются) — вне рамок.
- Глобальные (cross-agent) хуки — остаются per-agent.

## Компоненты

### 1. Модель решения (`hooks.rs`)

`HookAction` (sync, для closures) остаётся `{Continue, Block(String)}` — не ломаем logging/block_tools. Новый тип для decision-webhooks:

```rust
pub enum HookDecision {
    Continue,
    Block(String),                       // reason
    ModifyArgs(serde_json::Value),       // BeforeToolCall: заменить args (JSON-объект)
    InjectContext(String),               // BeforeMessage: добавить контекст (+ provenance)
    TransformResult(String),             // AfterToolResult: заменить результат (+ provenance)
}
```

Webhook-ответ (JSON) → `HookDecision`:
```json
{"decision": "continue"|"block", "reason": "...", "inject_context": "...", "modified_args": {...}, "transformed_result": "..."}
```
Все поля опциональны: `decision` отсутствует/=="continue" + наличие `modified_args`/`inject_context`/`transformed_result` → соответствующий вариант. `decision`=="block" → `Block(reason)` (short-circuit). Пустой/`{}` ответ → `Continue`.

### 2. Конфиг (`config/mod.rs`)

Расширить `WebhookConfig` (DRY — старые webhooks работают как `async`):
```rust
pub struct WebhookConfig {
    pub url: String,
    pub events: Vec<String>,
    #[serde(default)] pub mode: WebhookMode,           // Async (default) | Decision
    #[serde(default)] pub tool_matcher: Option<String>, // regex на tool_name (*ToolCall/*ToolResult)
    #[serde(default)] pub on_failure: FailureMode,      // Open (default) | Closed
    #[serde(default = "default_hook_timeout_ms")] pub timeout_ms: u64, // 3000 для decision
    #[serde(default)] pub allow_internal: bool,          // true → стандартный http_client (обход SSRF) для localhost/LAN hook (admin opt-in)
}

#[derive(Default)] pub enum WebhookMode { #[default] Async, Decision }
#[derive(Default)] pub enum FailureMode { #[default] Open, Closed }
fn default_hook_timeout_ms() -> u64 { 3000 }
```
serde rename в lowercase (`async`/`decision`, `open`/`closed`). `timeout_ms` cap 30_000.

### 3. `HookRegistry::fire_decision` (`hooks.rs`)

```rust
pub async fn fire_decision(&self, event: &HookEvent, extra: serde_json::Value) -> HookDecision
```
- Выбрать decision-mode webhooks, подходящие под `event` (по `events`) и `tool_matcher` (regex full-match на `tool_name`; для BeforeMessage matcher игнорируется). Не совпало → пропуск.
- **Последовательно** в порядке конфига: построить request JSON (поля события: event/agent/tool_name/timestamp + `extra`: tool_input/message/result), POST через `http_client` (SSRF) с `timeout_ms`, распарсить ответ в `HookDecision`.
  - **first Block wins** — короткое замыкание, вернуть Block.
  - `ModifyArgs`/`TransformResult`/`InjectContext` **чейнятся**: выход хука N → `extra` для N+1 (args/result обновляются; inject_context аккумулируется конкатенацией).
  - timeout/ошибка/невалидный JSON → `on_failure` (open→продолжить как Continue для этого хука; closed→Block).
- Вернуть агрегированный `HookDecision` (накопленные modify/inject/transform или Continue).
- `tool_matcher` компилируется ОДИН раз при `set_webhooks` во внутреннее представление реестра (НЕ в Deserialize-структуру `WebhookConfig`) — напр. `Vec<(WebhookConfig, Option<regex::Regex>)>`. Невалидный regex → хук пропускается + warn при регистрации (не паника).

### 4. Интеграция в fire-точки

Порядок в каждой точке: сначала sync `fire()` (closures — дёшево, block_tools может ветировать без HTTP), если не Block → `fire_decision().await`.

- **BeforeMessage — decision-fire ВНУТРИ `bootstrap` (после построения `enriched_text`, bootstrap.rs:~260), НЕ в run.rs:34** (там контекст ещё не существует, inject некуда применить — CRIT ревью). `extra = {message}`. `Block`→прервать ход с reason; `InjectContext(s)`→добавить санитайзнутый+provenance `s` в контекст/сообщения перед LLM-вызовом. Sync `fire(BeforeMessage)` в run.rs:34 остаётся для observer-closures.
- **`engine_dispatch.rs:142` (BeforeToolCall, inner — `arguments: &Value` доступны):** `extra = {tool_input: arguments}`. `Block`→вето (существующий путь). `ModifyArgs(v)`→продолжить исполнение с `v` через rebind-паттерн approval `ApprovedWithModifiedArgs` (engine_dispatch.rs:121-128), сохранив `_context` (+ audit).
- **`engine_dispatch.rs:50` (AfterToolResult, outer — результат доступен):** `extra = {result}`. `TransformResult(s)`→заменить результат на санитайзнутый+provenance `s` перед добавлением в контекст (+ audit).

### 5. Безопасность

- **SSRF + `allow_internal` opt-in:** по умолчанию decision-webhooks через `ssrf_http_client` (приватные IP блокируются на уровне DNS; `is_internal_endpoint` матчит лишь хардкод toolgate/renderer, произвольный localhost НЕ открывает). Т.к. хуки admin-конфигурируемы (TOML/PUT под auth = доверенная граница), per-hook **`allow_internal: bool` (default false)**: при `true` хук идёт через стандартный `http_client()` (без SSRF-резолвера) — для hook-сервиса рядом с OPEX (типовой деплой). Риск осознанно берёт admin.
- **Audit:** требует НОВОГО варианта `AuditEvent::HookDecision` (`db/audit_queue.rs` — сейчас только `ToolExecution`/`ToolQuality`) + арм воркера. Каждое нетривиальное решение (Block/ModifyArgs/InjectContext/TransformResult) → запись (agent, event, tool_name, hook-host, тип, усечённый reason/дифф ≤512B).
- **Provenance + анти-spoof:** перед обёрткой из ответа УДАЛЯЮТСЯ/экранируются вхождения маркера `[hook:` (внешний сервис не подделает источник), затем `inject_context`/`transformed_result` оборачиваются префиксом `[hook:{host}] …`. Отметка источника закрывает injection-канал (FSE-исследование). Полноструктурный provenance — non-goal.
- **modified_args:** должен десериализоваться в JSON-объект; иначе → on_failure. Безопасность аргументов обеспечивает tool-handler (workspace-handlers валидируют path через `validate_workspace_path`/`is_read_only` — modify НЕ обходит эти проверки). Изменение аудируется.

## Семантика (edge cases)

- **Несколько decision-хуков на ОДНОМ event-instance:** последовательно в одном `fire_decision`; first Block short-circuit; modify/transform/inject чейнятся (выход N → вход N+1).
- **Параллельные tool-calls** (parallel.rs:401-443 join_all) фаерят свои `fire_decision` НЕЗАВИСИМО и конкурентно — cross-tool порядка/чейнинга НЕТ (корректно: инструменты независимы; чейнинг только внутри одного tool-call).
- **Латентность:** hook-timeout входит в бюджет tool-timeout (`default_timeout`, parallel.rs:418) — должно держаться `timeout_ms × N decision-хуков ≤ tool-timeout`, иначе обёртка инструмента сработает раньше хука.
- **mode=async (старые webhooks):** без изменений — fire-and-forget; `on_failure`/`timeout_ms`/`tool_matcher` НЕ применяются (async шлёт как раньше, на все подходящие по `events`).
- **closed на transform-событии:** неприменимо (нет block) → при сбое оригинальный результат + warn.
- **timeout default 3000ms** на горячем пути — оператор тюнит per-hook (cap 30s).
- **Пустой ответ webhook** → Continue (no-op).
- **block_tools (sync) + decision-webhook:** sync block выигрывает первым (HTTP не вызывается).

## Тестирование (TDD)

**Unit (`hooks.rs` — `fire_decision`, мок через локальный HTTP/WireMock):**
- Ответ block→`Block`; continue→`Continue`; modified_args→`ModifyArgs`; inject_context→`InjectContext`; transformed_result→`TransformResult`.
- `tool_matcher`: совпадение → хук срабатывает; несовпадение → пропуск (Continue).
- `on_failure`: timeout/connection-error/невалидный-JSON с `open`→Continue; с `closed`→Block.
- Ordering: два decision-хука, первый Block → второй не вызван; чейнинг: hook1 ModifyArgs → hook2 видит изменённые args.
- mode=async не парсит ответ (fire-and-forget).
- Пустой `{}` ответ → Continue.

**Integration (WireMock-сервер, как в file_scenario):**
- BeforeToolCall block ветирует инструмент (инструмент не исполняется).
- BeforeToolCall modified_args заменяет аргументы (инструмент получает новые).
- AfterToolResult transformed_result заменяет результат до LLM.
- BeforeMessage inject_context добавляет контекст.

**Security:**
- decision-webhook на приватный IP (10.x/127.x) с `allow_internal=false` блокируется SSRF-резолвером; с `allow_internal=true` — доходит (стандартный клиент).
- inject_context/transformed_result несут provenance-префикс `[hook:{host}]`; подделанный `[hook:` во входящем ответе санитайзится (анти-spoof).
- audit-запись (`AuditEvent::HookDecision`) пишется на block/modify/transform/inject.
- параллельные tool-calls: per-tool `fire_decision` независимы (нет cross-tool чейнинга).

**Negative:**
- closed + endpoint down на BeforeToolCall → Block.
- невалидный matcher-regex → ошибка регистрации/skip (не паника).

## Open questions / future

- shell-hooks / on-host исполнение (если потребуется истинный Hermes-parity).
- Полноструктурный provenance-граф для injected/transformed контента.
- UI для управления decision-хуками (сейчас через agent-config TOML/PUT API).
- Доп. события (AfterResponse/OnError — сначала включить их fire-точки).
