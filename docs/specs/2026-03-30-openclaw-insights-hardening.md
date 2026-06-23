# OPEX Hardening: Insights from OpenClaw

> **STATUS: COMPLETED 2026-04-02** — все пункты P0/P1/P2 реализованы.
> Spec по результатам анализа 600 коммитов OpenClaw.
> Только реальные пробелы — всё, что уже реализовано, исключено.

## Контекст

Анализ 600 последних коммитов OpenClaw выявил паттерны и улучшения, часть которых
уже реализована в OPEX (LLM idle timeout, word-boundary splitting, error
classification, FTS fallback, PII redaction, CJK-safe chunking, timing-safe auth,
typing indicators). Этот spec покрывает **только недостающие или частично
реализованные** возможности.

---

## P0 — Критично (надёжность и безопасность)

### P0.1 — Malformed tool-call JSON recovery

**Проблема:** LLM (особенно MiniMax M2.5) генерируют tool call arguments с:
- Markdown fences (` ```json ... ``` `)
- Prefix text перед JSON (`Here is the result: {...}`)
- Trailing commas (`{"a": 1, "b": 2,}`)
- Unquoted keys

Сейчас OPEX ловит orphan tool calls (transcript repair в `engine.rs:877`),
но **не ремонтит сам JSON**.

**Решение:**
- Добавить `fn repair_json(raw: &str) -> Result<serde_json::Value>` в
  `crates/opex-core/src/agent/` (новый модуль `json_repair.rs`)
- Pipeline: strip markdown fences → strip prefix/suffix text → fix trailing commas
  → try `serde_json::from_str` → если не парсится, попробовать `serde_json::from_str`
  после удаления comments
- Вызывать в `parse_tool_calls()` перед десериализацией аргументов
- Логировать `tracing::warn!` когда ремонт был необходим (для мониторинга качества LLM)

**Файлы:**
- Новый: `crates/opex-core/src/agent/json_repair.rs`
- Изменить: `crates/opex-core/src/agent/mod.rs` (pub mod)
- Изменить: `crates/opex-core/src/agent/providers_openai.rs` (вызов repair)

**Тесты:** unit-тесты с реальными примерами битого JSON от MiniMax.

---

### P0.2 — Fail closed при недоступности Docker/skills

**Проблема:** Когда Docker-контейнер скилла не поднимается, агент логирует `warn`
и продолжает работу — пользователь не знает, что инструмент недоступен.

**Решение:**
- В `handlers/agents.rs` (~строка 650): если `ensure_container` провалился,
  вернуть tool result с `is_error: true` и человекочитаемым сообщением
  `"Skill '{name}' is temporarily unavailable: {error}"`
- Агент увидит ошибку в контексте и сможет сообщить пользователю / выбрать
  альтернативный инструмент
- НЕ крашить весь запрос — только пометить конкретный tool call как failed

**Файлы:**
- Изменить: `crates/opex-core/src/gateway/handlers/agents.rs`
- Изменить: `crates/opex-core/src/agent/engine.rs` (обработка tool error)

---

### P0.3 — Self-chat dedupe (Telegram)

**Проблема:** В групповом чате бот может получить своё же сообщение через
webhook/polling и ответить на него → бесконечный цикл.

**Решение:**
- В Telegram driver (`channels/src/drivers/telegram.ts`): на входе проверять
  `message.from?.id === bot.botInfo.id` → skip
- Также проверять `message.via_bot?.id === bot.botInfo.id` для inline results

**Файлы:**
- Изменить: `channels/src/drivers/telegram.ts` (входной обработчик сообщений)

**Тесты:** unit-тест с mock message от bot_id.

---

### P0.4 — Model fallback chain

**Проблема:** Если MiniMax API недоступен (500/502/timeout), агент полностью
мёртв. Нет механизма переключения на резервную модель.

**Решение:**
- В конфиге агента (`config/agents/main.toml`):
  ```toml
  [llm]
  model = "MiniMax-M1"
  fallback_models = ["claude-sonnet-4-20250514", "qwen3.5:4b"]
  ```
- В `providers.rs`: после исчерпания retry (3 попытки) на primary model,
  переключиться на следующую модель из `fallback_models`
- Сохранять system prompt при переключении
- Логировать `tracing::warn!("model fallback: {} -> {}", primary, fallback)`
- После успешного ответа от fallback — НЕ возвращаться автоматически на primary
  (только по таймеру через N минут или по следующей сессии)

**Файлы:**
- Изменить: `crates/opex-core/src/config.rs` (поле `fallback_models`)
- Изменить: `crates/opex-core/src/agent/providers.rs` (логика fallback)
- Изменить: `crates/opex-core/src/agent/providers_openai.rs` (retry + fallback)
- Изменить: `config/agents/main.toml` (добавить fallback_models)

---

## P1 — Важно (качество и observability)

### P1.1 — Sub-agent tool isolation

**Проблема:** Sub-agent наследует ВСЕ инструменты родителя. Может рекурсивно
вызвать `spawn_subagent` или записать в workspace без контроля.

**Решение:**
- В `spawn_subagent` tool definition добавить опциональный параметр `allowed_tools`:
  ```json
  {"allowed_tools": ["memory_search", "searxng_search", "brave_search"]}
  ```
- По умолчанию (если не указано): все инструменты кроме `spawn_subagent` и
  `workspace_write`
- В `engine_subagent.rs`: фильтровать tool definitions перед передачей sub-agent'у

**Файлы:**
- Изменить: `crates/opex-core/src/agent/engine_subagent.rs`
- Изменить: `crates/opex-core/src/agent/engine_tool_defs.rs`

---

### P1.2 — Delivery error classification (Telegram)

**Проблема:** `retryTg()` ретраит все ошибки одинаково. "Forbidden: bot was
blocked by the user" ретраить бессмысленно.

**Решение:**
- Классификация ошибок Telegram API:
  - **Permanent (не ретраить):** 403 Forbidden, 400 Bad Request (chat not found,
    message too long), 401 Unauthorized
  - **Transient (ретраить с backoff):** 429 Too Many Requests, 500/502/503,
    network errors
- Обновить `retryTg()` в `telegram.ts`: перед retry проверять HTTP status code
- Для permanent errors — логировать `error` и возвращать результат без retry

**Файлы:**
- Изменить: `channels/src/drivers/telegram.ts` (функция `retryTg`)

---

### P1.3 — Secret redaction в логах и ошибках

**Проблема:** PII redaction (`pii.rs`) маскирует телефоны/email/карты, но API
ключи и токены в error messages/stack traces не маскируются.

**Решение:**
- Расширить `pii.rs` или создать `redact.rs`:
  - Маскировать строки, похожие на API ключи (`sk-...`, `Bearer ...`, длинные hex)
  - Маскировать значения env vars из secrets table
- Применять на уровне `tracing` subscriber (layer, который фильтрует spans/events)
- Как минимум — применять к error messages перед отправкой в channel

**Файлы:**
- Изменить или новый: `crates/opex-core/src/agent/pii.rs` → `redact.rs`
- Изменить: `crates/opex-core/src/gateway/middleware.rs` (tracing layer)

---

### P1.4 — Config hot-reload

**Проблема:** Изменение `opex.toml` или agent TOML требует полный рестарт.

**Решение:**
- Watch `config/` через `notify` crate (уже есть в экосистеме tokio)
- При изменении TOML: reload в `Arc<RwLock<Config>>`, без перезапуска listener'ов
- Agent TOML: перезагрузить system prompt, tools, model без пересоздания сессий
- Отправить event в `/api/doctor` что config был перезагружен
- SIGHUP как альтернативный триггер reload

**Файлы:**
- Изменить: `crates/opex-core/src/config.rs` (Arc<RwLock<>>)
- Изменить: `crates/opex-core/src/main.rs` (SIGHUP handler, file watcher)
- Изменить: `crates/opex-core/src/agent/engine.rs` (reload agent config)

---

## P2 — Полезно (quality of life)

### P2.1 — Tool namespacing

**Проблема:** YAML tool и MCP tool с одинаковым именем вызовут коллизию.

**Решение:**
- Внутренние tools: без префикса (`memory_search`, `workspace_write`)
- YAML tools: без префикса (обратная совместимость), но при коллизии с MCP —
  YAML побеждает + warn в лог
- MCP tools: префикс `mcp_{server}_{tool}` (как в OpenClaw)
- Resolver: system → yaml → mcp (приоритет)

**Файлы:**
- Изменить: `crates/opex-core/src/agent/engine_tool_defs.rs`
- Изменить: `crates/opex-core/src/mcp/` (namespace при регистрации)

---

### P2.2 — Task audit endpoint

**Проблема:** Нет истории выполнения задач с результатами. Сложно отлаживать
cron jobs.

**Решение:**
- `GET /api/tasks/audit?limit=50&agent=main` — возвращает историю выполнения:
  task_id, started_at, finished_at, status (ok/error), result_preview (первые 200 символов)
- Данные уже есть в таблице `tasks` + `task_steps` — нужен только handler + query

**Файлы:**
- Изменить: `crates/opex-core/src/gateway/handlers/tasks.rs`

---

### P2.3 — Search cache per-session

**Проблема:** Агент может вызвать `searxng_search("bitcoin price")` дважды за
сессию. Каждый вызов = HTTP запрос.

**Решение:**
- `HashMap<(session_id, query_hash), (results, timestamp)>` в `AgentEngine`
- TTL: 5 минут (в рамках сессии)
- Только для `searxng_search` и `brave_search`

**Файлы:**
- Изменить: `crates/opex-core/src/agent/engine.rs` (search cache field)
- Изменить: YAML tools execution path (проверка кэша перед HTTP)

---

## Порядок реализации

```
Phase 1 (safety):  P0.3 → P0.1 → P0.2
Phase 2 (uptime):  P0.4 → P1.2
Phase 3 (quality): P1.1 → P1.3 → P1.4
Phase 4 (polish):  P2.1 → P2.2 → P2.3
```

P0.3 (self-chat dedupe) первым — минимальный diff, максимальный risk reduction.

---

## Не включено (уже реализовано)

| Фича | Статус в OPEX |
|-------|-------------------|
| LLM idle timeout (30s) | `providers_openai.rs:376` |
| Word-boundary message split | `channels/common.ts:12` |
| Transient/permanent error classification | `error_classify.rs` |
| FTS fallback при недоступности embeddings | `memory.rs:159` |
| PII redaction (phone/email/card) | `pii.rs` |
| CJK-safe UTF-8 chunking | `chunker.rs:13` |
| Task pressure monitoring | `/api/status` handler |
| Timing-safe auth comparison | `subtle::ConstantTimeEq` в middleware |
| Typing + emoji progress indicators | `telegram.ts:455` |
