# HydeClaw Development Plan: Insights from Hermes Agent

> **Source:** Анализ проекта [Hermes Agent](https://github.com/NousResearch/hermes-agent) (Nous Research, Python, MIT) в `D:/GIT/hermes-agent` от 2026-04-30.
> **Goal:** Список заимствуемых архитектурных идей и фич, сгруппированных по приоритету. Не implementation plan — это roadmap-кандидат на будущие phases.
> **Methodology:** 4 параллельных Explore-агента проанализировали ~600KB кода + 100+ файлов (cli.py 527KB, gateway/, plugins/memory/, tools/, environments/, acp_adapter/).
>
> **Status (2026-05-03):** Sprint 1 + Sprint 2 + Sprint 3 полностью выполнены.
> Все P0 закрыты: P0.1 (trajectory compression) + P0.3/#24/#26 + P0.4/#22/#30 +
> P0.5/#23/#28/#29/#31/#33 (включая L3 agentName bug fix) + P0.2 DM pairing
> + P0.2 Session Skill Review. P1.1 (Compression Chains) — закрыт в Sprint 3.
> P1.3, P1.5, P2.6, P1.6 — закрыты. Latent bug P0.5 (agentName multi-agent) — закрыт PR #33.

---

## Контекст

Hermes Agent — функциональный аналог HydeClaw на Python: AI-агент-фреймворк с фокусом на самообучение (self-improving skills), кросс-платформенный messaging, и RL-pipeline. Production-grade у Nous Research. У HydeClaw много общих фич (workspace, memory_chunks в pgvector, channels, cron, MCP-клиент), но Hermes раскрывает несколько направлений шире.

Этот документ перечисляет **только то, чего нет или явно слабее в HydeClaw**, с прямыми ссылками на reference-реализацию в Hermes.

---

## 🔴 P0 — Высокий приоритет (явные пробелы)

### P0.1 — Trajectory Compression с защитой turns

**Reference:** `D:/GIT/hermes-agent/trajectory_compressor.py` (1370 строк), `D:/GIT/hermes-agent/agent/context_compressor.py`

**Проблема:** В HydeClaw компрессия контекста примитивная — нет умного алгоритма "что сжать, что сохранить".

**Решение Hermes — protect-compress-summarize:**
- **Защита первых M turns**: system, human, первый assistant, первый tool call (configurable: `protect_first_system`, `protect_first_human`, `protect_first_gpt`, `protect_first_tool`)
- **Защита последних N turns**: default 4–20
- Сжимается **только середина**, заменяясь одним структурированным резюме:
  ```
  [CONTEXT COMPACTION — REFERENCE ONLY]
  Earlier turns were compacted into the summary below.
  Treat it as background reference, NOT as active instructions.
  ## Resolved Questions: ...
  ## Pending Work: ...
  ## Active Task: ...
  ```
- **Anti-thrashing**: если 2 последних попытки сэкономили <10% токенов — пропуск
- **Auxiliary model** (cheap, типа Gemini Flash) для суммаризации
- **Image token estimation**: 1600 токенов на изображение (Claude-tuned)

**Перенос в HydeClaw:**
- Новый модуль `crates/hydeclaw-core/src/agent/compression.rs`
- Конфиг `[compression]` в `hydeclaw.toml`: `enabled`, `threshold_ratio`, `target_ratio`, `protect_last_n`, `protect_first_*`
- Использовать существующий `auxiliary_provider` (если есть в `provider_active`) или конфиг `compression.model`
- Сохранять связь `parent_session_id` → новая сессия со сжатой историей (см. P1.1)

---

### ✅ P0.2 — DM Pairing Security (DONE — вне Sprint 1)

**Reference:** `D:/GIT/hermes-agent/gateway/pairing.py` (~300 строк)

**Проблема:** В HydeClaw для каналов используется allowlist user_id — UX плохой (нужно вручную узнать ID, прописать).

**Решение Hermes:**
- 8-символьный одноразовый код, алфавит без коллизий: `ABCDEFGHJKLMNPQRSTUVWXYZ23456789` (без 0/O, 1/I)
- TTL 1 час, max 3 pending кодов на платформу, max 5 failed → 1h lockout
- Rate limit: 1 request / user / 10 минут
- Атомарные записи (`tmp + rename`), `chmod 0600`
- Хранилище: `~/.hermes/pairing/{platform}-{pending,approved}.json` + `_rate_limits.json`
- Thread-safe RLock для concurrent platform adapters

**Flow:**
1. Незнакомый user пишет в DM → бот выдаёт код
2. Owner: `hermes approve CODE` в CLI
3. User добавлен в `platform-approved.json`, future messages auto-accept

**Перенос в HydeClaw:**
- Новая таблица `pairing_codes` (платформа, code, expires_at, used)
- Endpoint `POST /api/pairing/approve` (body: `{code}`)
- В channel adapter (`channels/src/drivers/*.ts`): при получении сообщения от незнакомого user → выдать код, сохранить в DB
- UI: страница «Pending pairings» с кнопкой Approve
- В отличие от Hermes — хранить в Postgres, не файлами

**✅ Реализовано:**

- Таблица `pairing_codes` в `migrations/006_pairing_codes.sql` (code, agent_id,
  channel_user_id, display_name, created_at).
- Endpoints в `gateway/handlers/access.rs`:
  `GET /api/access/{agent}/pending`,
  `POST /api/access/{agent}/approve/{code}`,
  `POST /api/access/{agent}/reject/{code}`.
- Функция `create_pairing_code()`.

---

### ✅ P0.3 — Subagent Isolation Rules (DONE — Sprint 1, PR #24 + #26)

**Reference:** `D:/GIT/hermes-agent/tools/delegate_tool.py:39-47`

**Проблема:** В HydeClaw `agent`-тул (`engine_agent_tool.rs`) разрешает subagent'у любые действия — нет защиты от рекурсии или неконтролируемых side-effects.

**Решение Hermes — `DELEGATE_BLOCKED_TOOLS`:**
```python
DELEGATE_BLOCKED_TOOLS = frozenset([
    "delegate_task",   # no recursion
    "clarify",         # no user interaction
    "memory",          # no shared MEMORY.md writes
    "send_message",    # no cross-platform side-effects
    "execute_code",    # children reason step-by-step
])
```
- Динамическая фильтрация tool-set по конфигу `delegation.toolsets`
- `MAX_DEPTH=1` по умолчанию (не позволяет subagent'у спавнить subagent'ов), расширяемо до 3
- Глобальный lock `_active_subagents_lock` для tracking live children (для shutdown drain)

**Перенос в HydeClaw:**
- В `session_agent_pool.rs::LiveAgent::spawn()`: фильтр инструментов через blocked-list из агентского конфига
- Default-deny: `agent` (no recursion), `code_exec`, `workspace_delete`, `workspace_rename`, `process_start`
- Configurable per-agent: `[agent.delegation] blocked_tools = [...]` в TOML
- Новый параметр `[agent.delegation] max_depth` в config

**✅ Реализовано (Sprint 1):**

- PR #24 (`5c742e6`) — `DelegationConfig.max_depth` (default 1) +
  `compute_denied_tools` (`SUBAGENT_DENIED_TOOLS` built-in deny list +
  `blocked_tools_extra` / `blocked_tools_override` config). Fail-closed
  recursion guard (`extract_subagent_depth` returns `u8::MAX` on malformed
  input, не 0).
- PR #26 (`a019cd2`) — `DelegationConfig::validate()` at agent-load: rejects
  `max_depth=0`, mutex `extra ⊻ override`, regex `^[a-zA-Z0-9_-]+$` per tool
  name. Fail-fast at boot вместо runtime surprise.
- crud.rs preserves `existing_cfg.agent.delegation.clone()` on PUT (PR #24
  review fix C5 — иначе UI-update сбрасывал в default).
- Files: `crates/hydeclaw-core/src/config/mod.rs`,
  `crates/hydeclaw-core/src/agent/pipeline/{agent_tool,subagent}.rs`.

---

### ✅ P0.4 — Trigram FTS для multilingual search (DONE — Sprint 1, PR #22 + #30)

**Reference:** `D:/GIT/hermes-agent/hermes_state.py:128-136`

**Проблема:** HydeClaw использует стандартный FTS — для русского/CJK словарный токенизатор работает плохо.

**Решение Hermes:**
```sql
CREATE VIRTUAL TABLE messages_fts_trigram USING fts5(
    content, tokenize='trigram'
);
```
- Trigram tokenizer бьёт текст на 3-байтовые последовательности — релевантно для морфологически богатых языков
- Используется параллельно со стандартным FTS (выбор по `lang` сообщения)

**Перенос в HydeClaw:**
- Postgres имеет `pg_trgm` extension (уже доступно)
- Новая миграция: GIN-индекс `CREATE INDEX ON memory_chunks USING gin (content gin_trgm_ops)`
- В `memory.rs::hybrid_search`: для русского-CJK добавить trigram similarity к скору ранжирования
- Опционально: `lang` поле в `memory_chunks` для роутинга на правильный токенизатор

**✅ Реализовано (Sprint 1):**

- PR #22 — миграция `035_pg_trgm_index.sql`: `CREATE EXTENSION pg_trgm` +
  `idx_memory_chunks_content_trgm` (GIN, `gin_trgm_ops`).
- PR #22 — `search_trigram` в `crates/hydeclaw-db/src/memory_queries.rs`:
  similarity-search через `%` оператор + `set_limit(threshold)`. PR #25
  fix-up (`95be96b`) переключил на `SET LOCAL` для error-path GUC isolation.
- PR #22 — 3-way RRF combiner в `crates/hydeclaw-core/src/memory/store.rs`:
  weights `W_SEM=0.6, W_FTS=0.25, W_TRGM=0.15`, `RRF_K=60`. 8-state shortcut
  match для случаев когда ≤1 ветка дала результаты.
- PR #30 (`390e0c7`) — integration tests через `memory_test_facade`
  (`#[doc(hidden)] pub mod`): KeywordEmbedder + 6 sqlx тестов
  (multi-layer fusion, determinism под ties, typo recovery).
- Verified on Pi: 500 chunks в DB, trigram similarity 0.131 для "Windows"
  query, GIN index используется.

---

### ✅ P0.5 — Reasoning Tokens Tracking (DONE — Sprint 1, PR #23 + #28 + #29 + #31)

**Reference:** `D:/GIT/hermes-agent/hermes_state.py:36-72`, `D:/GIT/hermes-agent/environments/agent_loop.py:186-200`

**Проблема:** Extended-thinking модели (DeepSeek-R1, Claude 3.7 thinking, Qwen QwQ) генерируют отдельные reasoning_tokens — HydeClaw их не трекает в `usage_log`.

**Решение Hermes — отдельная колонка + per-turn массив:**
```sql
ALTER TABLE sessions ADD COLUMN reasoning_tokens INTEGER DEFAULT 0;
```
```python
@dataclass
class AgentResult:
    reasoning_per_turn: List[Optional[str]]  # текст reasoning по каждому turn
    ...
```

**Перенос в HydeClaw:**
- Миграция: `ALTER TABLE usage_log ADD COLUMN reasoning_tokens INTEGER`
- В providers (`providers_anthropic.rs`, `providers_openai.rs`, `providers_google.rs`): извлекать `usage.reasoning_tokens` (Anthropic) / `reasoning_content` (OpenAI o1) / `thoughtsTokenCount` (Gemini)
- В UI: отображать как отдельную секцию в Usage event (rounded breakdown: input/output/cache/reasoning)
- Billing: учитывать в стоимости отдельно (reasoning tokens обычно дороже)

**✅ Реализовано (Sprint 1):**

- PR #23 — миграция `036_usage_log_extended_tokens.sql`: добавлены
  `cache_read_tokens`, `cache_creation_tokens`, `reasoning_tokens` в
  `usage_log`. `TokenUsage` в `hydeclaw-types` расширен с новыми полями
  (`Option<u32>` — `None` если провайдер не отдаёт).
- PR #23 + #28 — извлечение во всех 5 провайдерах: `providers_openai.rs`
  (`reasoning_tokens` + `cached_tokens`), `providers_anthropic.rs`
  (cache_read/creation), `providers_google.rs` (`thoughtsTokenCount`),
  плюс пути для других совместимых провайдеров.
- PR #27 (`5bfe0b7`) — `StreamingUsage` struct заменил 5-tuple в OpenAI
  стриме (предотвращает `(cache_read, cache_creation)` swap-баги).
- PR #28 (`35a0621` + fix-up `6741c37`) — Anthropic streaming `chat_stream`
  парсит `message_start` (initial usage) + `message_delta` (cumulative
  output + cache + input при server-side tools). Fix-up L1 — учёт
  `cache_*_input_tokens` в `message_delta` (был отброс), L2 — drop bare
  `message_delta` без `message_start` (защита от corruption usage_log).
- PR #29 (`be8f8e2` + fix-up `aaa96b6`) — UI vitest 21 + 14 + 1 todo: SSE
  parser, stream-processor write, ContextBar render с tooltip breakdown
  (input / cache write × 1.25 / cache read × 0.1 / output / reasoning),
  bar color thresholds (>80% yellow, >95% red), boundary tests.
- PR #31 (`0498af7`) — `record_usage` рефакторинг: 10 позиционных параметров
  → `&TokenUsage`. Compiler теперь ловит swap-баги между cache_read/
  cache_creation/reasoning (раньше все 3 были `Option<u32>`).
- Verified on Pi: chat → SSE `usage{16700/19}` → запись в `usage_log` с
  NULL для ollama (нет extended fields — корректно).

**✅ Latent bug закрыт (PR #33, `6c2547d`):** `chat.rs` тегирует каждый `usage`
SSE event полем `agentName: current_responding_agent`. `stream-processor.ts`
роутит запись на `AgentState` целевого агента (не session-owner). 17 тестов
в `ui/src/__tests__/usage-event-flow.test.ts` — все зелёные, включая
multi-agent isolation test (нет `it.todo()`).

---

## 🟡 P1 — Средний приоритет (улучшения существующего)

### ✅ P1.1 — Compression Chains (parent_session_id) (DONE — Sprint 3, 2026-05-03)

**Reference:** `D:/GIT/hermes-agent/hermes_state.py:50` — поле `parent_session_id` в `sessions`.

**Идея:** После сжатия контекста создаётся новая сессия B, в `parent_session_id=A`, `end_reason='compression'` у A. Цепочка: A → B → C — позволяет восстановить полную историю при необходимости.

**✅ Реализовано:**

- Миграции 041 (`parent_session_id`, `end_reason`) + 042 (`ON DELETE SET NULL`).
- `CompressorState.pending_split: bool` — выставляется при эффективном сжатии.
- `maybe_split_session()` в `bootstrap.rs` — lazy split при следующем turn: создаёт child-сессию, копирует seed (system + summary-as-assistant + tail), маркирует parent `end_reason='compression'`.
- `GET /api/sessions/{id}/chain` — рекурсивный CTE (depth ≤ 20), возвращает цепочку root-first.
- UI: `ParentBadge` в списке сессий + `CompactChainBanner` над чатом (collapsed/expanded, localStorage).
- Покрытие: 5 Rust integration tests + 7 Vitest UI tests.

---

### P1.2 — Cross-Platform Session Mirroring

**Reference:** `D:/GIT/hermes-agent/gateway/mirror.py`

**Идея:** Когда сообщение отправляется в одну платформу из CLI/cron, дублируется в transcript целевой сессии как `mirror=true` запись. Агент **видит контекст** другой платформы при ответе. Cross-platform conversation continuity.

**Перенос:** В `messages` таблицу — новое поле `is_mirror BOOLEAN DEFAULT false`. В `agent/pipeline/bootstrap.rs` — игнорировать mirror-сообщения для агентского контекста, но показывать в UI.

---

### ⚠️ P1.3 — Cron Multi-Target Delivery Routing (PARTIAL)

**Reference:** `D:/GIT/hermes-agent/cron/scheduler.py:74-100`, `D:/GIT/hermes-agent/gateway/delivery.py`

**Идея:** Парсинг targets вида `telegram:12345:thread_id`, `discord:#general`, `origin` (обратно в исходный чат), `local` (на диск). Truncation guard при >4000 символов: full на диск, в чат — short version + link.

**В HydeClaw:** в `scheduled_jobs` уже есть `delivery_channels JSONB` — расширить парсингом target-string в enum:
```rust
enum DeliveryTarget {
    Origin,                                  // в исходный канал
    Local(PathBuf),                         // на диск
    Channel { channel_id: Uuid, thread: Option<i64> },
    HomeChannel(ChannelType),               // env-var configured default
}
```

**⚠️ Частично реализовано:**

- В `scheduled_jobs` есть поле `announce_to JSONB`
  (`{ channel, chat_id, channel_id? }`) — базовый один target.
- `DeliveryTarget` enum и парсинг `telegram:id:thread` строк — не реализованы.
- Не хватает: multi-target список, `origin`/`local` варианты, truncation guard.

---

### P1.4 — MCP Serve Mode (HydeClaw как MCP-сервер)

**Reference:** `D:/GIT/hermes-agent/mcp_serve.py`

**Идея:** Hermes экспортирует себя как MCP-сервер с 9 инструментами:
- `conversations_list`, `conversation_get`, `messages_read`, `messages_send`
- `attachments_fetch`, `events_poll`, `events_wait`
- `permissions_list_open`, `permissions_respond`

Это позволяет Claude Desktop/любому MCP-клиенту читать/писать в сессии Hermes.

**В HydeClaw:** новая команда `hydeclaw mcp serve` — отдельный binary или подкоманда `hydeclaw-core`. Wrapper над существующим `/api/sessions/*`, `/api/messages/*`, `/api/approvals/*` endpoints. Аутентификация через тот же `HYDECLAW_AUTH_TOKEN`.

---

### ⚠️ P1.5 — Hook System для extensibility (PARTIAL)

**Reference:** `D:/GIT/hermes-agent/gateway/hooks.py`

**Идея:** User создаёт `~/.hermes/hooks/my-hook/HOOK.yaml + handler.py`. События: `gateway:startup`, `session:start/end`, `agent:start/step/end`, `command:*`. Async fire, errors не блокируют pipeline.

**В HydeClaw:** trait `Hook` в Rust, динамическая загрузка через WASM (для безопасности) или Lua (`mlua` crate). Альтернатива — webhook-based hooks: `[hooks]` секция в TOML с URLs, вызывается POST'ом на event. Проще портируется, без runtime для пользовательского кода.

**⚠️ Частично реализовано:**

- `crates/hydeclaw-core/src/agent/hooks.rs` — `HookEvent` enum
  (`BeforeMessage`, `AfterResponse`, `BeforeToolCall`, `AfterToolResult`,
  `OnError`), `HookAction` (`Continue` / `Block(String)`), `HookRegistry`
  с `register()` / `fire()`, built-in `logging_hook()` / `block_tools_hook()`.
- TOML-конфиг `[hooks]` для пользовательских webhook-хуков — не реализован.

---

### ✅ P1.6 — Skill Self-Improvement Loop (DONE)

**Reference:** `D:/GIT/hermes-agent/tools/skill_manager_tool.py`, `D:/GIT/hermes-agent/agent/curator.py`

**Идея — НЕ ML, чистый prompt-driven loop:**
- Сложная задача (5+ tool calls) → агент предлагает `skill_manage create` — сохранить рецепт
- Скилл устарел → агент сам вызывает `skill_manage patch` (find-and-replace) или `edit` (rewrite)
- **Curator** на inactivity-trigger переводит неиспользуемые скиллы в архив (30/90 дней)
- Persistent state в `.curator_state` JSON: `last_run_at`, `paused`, `run_count`

**В HydeClaw:** уже есть `workspace/skills/`. Добавить:
- Новый тул `skill_manage` (action: create/patch/edit/archive)
- В `memory-worker`: периодическая задача (cron-like) — `skills_curator`, отслеживает `last_used_at` (новое поле в БД), архивирует stale
- pgvector embeddings скиллов для семантического поиска и консолидации похожих

**✅ Реализовано:**

- `skills/mod.rs` — `SkillFrontmatter.last_used_at: Option<String>` +
  функция `update_last_used_at()`.
- `gateway/handlers/curator_decisions.rs` — curator-логика для скиллов.
- Scheduled skill curator упоминается в `config/mod.rs`.

---

### P1.7 — Voice Memo Pipeline Unification

**Reference:** `D:/GIT/hermes-agent/gateway/platforms/base.py:641-670` (`cache_audio_from_bytes()`)

**Идея:** Audio из всех платформ падает в `AUDIO_CACHE_DIR`, STT-тул читает оттуда. Поддержка OGG/Opus/MP3/WAV/M4A/FLAC. Telegram-специфика: `sendVoice` vs `sendAudio` правильно различает.

**В HydeClaw:** в фазе [260430-oyx-04](commits) добавлен `/api/media/transcribe` — это endpoint per-request. У Hermes — fallback-cache: если STT падает, audio файл остаётся доступным. Перенос: `workspace/audio_cache/` с TTL 24h, индексируется в `messages` через `attachment_id`.

---

## 🟢 P2 — Низкий приоритет / future research

### P2.1 — ACP (Agent Communication Protocol) для IDE integration

**Reference:** `D:/GIT/hermes-agent/acp_adapter/`

**Идея:** Это **стандартный** протокол Anthropic для IDE (VS Code, Zed, JetBrains), не доморощенный. Hermes реализует:
- `acp_adapter/server.py:74` — thread pool для параллельных агентов
- `acp_adapter/session.py` — SessionManager с history, model state, cancel event
- `acp_adapter/events.py` — bridge `tool_progress_callback` → ACP `session_update`
- `acp_adapter/permissions.py` — `approval_callback` → ACP `request_permission` RPC

**Когда заниматься:** если планируется HydeClaw VS Code extension. Сейчас preempt не имеет смысла — ACP стандарт молодой, может ещё измениться.

---

### P2.2 — Multi-Backend Terminal Abstraction

**Reference:** `D:/GIT/hermes-agent/tools/environments/` — 6 бэкендов: `local | docker | ssh | daytona | singularity | modal`

**Архитектура:**
- Unified `BaseEnvironment` trait
- **Session snapshot**: env vars + functions + aliases capture once at init, re-source перед каждой командой
- **CWD persistence**: для remote — in-band stdout markers, для local — temp file
- **Activity callback** для gateway heartbeat (typing indicator)
- **FileSyncManager**: синхронизация `~/.hermes` для Modal (snapshot persistence) и SSH (file uploads)

**Особо ценные backends для HydeClaw:**
- **SSH** — `code_exec` на удалённых машинах через ControlMaster persistence
- **Modal** — serverless code execution с hibernation (idle = near-zero cost)

**В HydeClaw:** сейчас только Docker sandbox. Trait `ExecutionBackend` в Rust + 2 реализации (Docker, SSH) — серьёзная фича для enterprise/research use cases.

---

### P2.3 — Memory Provider Plugin Pattern (Honcho dialectic)

**Reference:** `D:/GIT/hermes-agent/agent/memory_manager.py`, `D:/GIT/hermes-agent/plugins/memory/honcho/__init__.py` (1329 строк)

**Архитектура:**
- Trait-like protocol: `on_prefetch`, `on_sync`, `on_delegation`
- **Single external provider rule** — только ОДИН не-builtin провайдер разрешён (предотвращает tool schema bloat)
- **Honcho dialectic**: multi-pass LLM reasoning в фоне, до 3 проходов с cold/warm prompts, early-exit при достаточном signal
- **Cadence config**: `contextCadence`, `dialecticCadence`, `injectionFrequency` — экономия токенов
- **Empty-streak backoff**: если provider молчит → exponential backoff, max 8×
- **Memory write hook**: при добавлении в MEMORY.md → автоматически зеркалится в Honcho conclusion

**В HydeClaw:** trait `MemoryProvider` в Rust, default = pgvector. Async hook chain — уже частично есть (см. `Modularity Roadmap` в memory). Добавление dialectic prefetch требует осторожности в HTTP-driven gateway (TTL-таймауты).

---

### P2.4 — Toolset Distributions (probabilistic для batch experiments)

**Reference:** `D:/GIT/hermes-agent/toolset_distributions.py:44-54`

```python
"image_gen": {
    "toolsets": {"image_gen": 90, "vision": 90, "web": 55, "terminal": 45, "moa": 10}
}
```

**Идея:** Probabilistic toolset selection — для A/B экспериментов и batch trajectory generation для RL. Не критично для production, интересно для research-режима.

**В HydeClaw:** опциональная фича в `[agent.experiment]` секции. Полезно если будет RL-фасад (см. P2.5).

---

### P2.5 — Atropos RL Environments + Trajectory Compression

**Reference:** `D:/GIT/hermes-agent/environments/`, `D:/GIT/hermes-agent/rl_cli.py`, `D:/GIT/hermes-agent/tinker-atropos/`

**Идея:** Полный RL-pipeline для обучения tool-calling моделей:
- `HermesAgentBaseEnv` (наследует от `atroposlib.BaseEnv`)
- `TerminalTestEnv`, `HermesSweEnv`, `TerminalBench2EvalEnv`
- 11 tool-call парсеров: Hermes, Mistral, Llama3, Qwen, DeepSeek, Kimi, GLM, …
- Phase 1: OpenAI-compatible server (eval, SFT data)
- Phase 2: VLLM ManagedServer с logprobs (GRPO/PPO)

**Когда заниматься:** только если HydeClaw планирует исследовательский трек. Сейчас — overkill.

---

### ⚠️ P2.6 — Prompt Injection Scanner для context files (PARTIAL)

**Reference:** `D:/GIT/hermes-agent/agent/prompt_builder.py:36-73`

**Идея:** Regex-сканер при загрузке `AGENTS.md/CLAUDE.md/SOUL.md` ищет:
- "ignore previous instructions" patterns
- Невидимый Unicode (zero-width characters, RTL overrides)
- `curl ... | sh` patterns для экспорта секретов

Блокирует загрузку при детекте, логирует.

**В HydeClaw:** workspace files читаются как есть. Добавить аналогичный сканер в `workspace.rs::read_file()` — особенно полезно для shared workspace, где один агент может подсадить инъекцию другому.

**⚠️ Частично реализовано:**

- `tools/content_security.rs` — `detect_prompt_injection()` с regex-паттернами
  ("ignore previous instructions", "you are now", XML-теги, опасные команды),
  `wrap_external_content()`. Детект логирует, не блокирует.
- Zero-width chars (`​`, RTL override) — не проверяются.
- Интеграция в `workspace.rs::read_file()` — не сделана (только для tool-output).

---

### P2.7 — Batch Processing с Checkpointing

**Reference:** `D:/GIT/hermes-agent/batch_runner.py` (515 строк), `D:/GIT/hermes-agent/mini_swe_runner.py`

**Идея:** Параллельная обработка JSONL датасета с checkpoint resume — устойчивость к перебоям в долгих задачах.

**В HydeClaw:** только при появлении use case (batch evaluation, dataset generation). Сейчас не нужно.

---

## 📊 Сравнительная матрица: HydeClaw vs Hermes

| Фича | HydeClaw 2026-05-02 | Hermes |
|---|---|---|
| Trajectory compression (protect+summarize) | ❌ примитивная | ✅ продвинутая |
| DM pairing security | ✅ pairing_codes + /api/access | ✅ TOTP-style codes |
| Session mirroring cross-platform | ❌ | ✅ |
| WhatsApp/Signal adapters | ❌ | ✅ |
| MCP serve (как сервер) | ❌ только client | ✅ обе роли |
| ACP (IDE protocol) | ❌ | ✅ |
| Hook system | ✅ WebhookConfig + fire_webhooks (260502-uij) | ✅ |
| Reasoning tokens tracking | ✅ Sprint 1 (#23+#28) | ✅ |
| Skill self-improvement (patch tool) | ✅ curator_decisions + last_used_at | ✅ |
| Curator (skill archival) | ✅ curator_decisions.rs | ✅ |
| Multi-backend terminal (SSH/Modal) | ❌ только Docker | ✅ 6 backends |
| Trigram FTS для CJK/RU | ✅ Sprint 1 (#22+#30) | ✅ |
| Cron multi-target delivery | ✅ normalize_announce_to + multi-target loop (260502-uio) | ✅ |
| Prompt injection scanner | ✅ pub fn + zero-width + workspace.rs (260502-u9v) | ✅ |
| Subagent blocked tools | ✅ Sprint 1 (#24+#26) | ✅ |
| Voice memo cache pipeline | ⚠️ per-request (нет audio_cache) | ✅ unified cache |
| Compression chains (parent_session_id) | ❌ только message-level branching | ✅ session-level |
| Atropos RL pipeline | ❌ | ✅ |
| Memory provider trait/plugins | ❌ монолит | ✅ |

---

## Рекомендуемая последовательность

**✅ Sprint 1 (быстрые wins) — DONE 2026-05-01:**

1. ✅ P0.5 — Reasoning tokens — PR #23, #28, #29, #31
2. ✅ P0.3 — Subagent blocked tools (defensive) — PR #24, #26
3. ✅ P0.4 — pg_trgm индекс — PR #22, #30

Bonus PRs: №25 (3 review-fix-ups), №27 (StreamingUsage struct refactor),
№32 (clippy strict-warnings cleanup).

**✅ Вне Sprint 1 — DONE:**

1. ✅ P0.2 — DM pairing security (pairing_codes + /api/access handlers)
2. ✅ P1.6 — Skill self-improvement (last_used_at + curator_decisions.rs)

**✅ 2026-05-02 — DONE:**

1. ✅ P1.3 — Cron multi-target (normalize_announce_to, 260502-uio)
2. ✅ P1.5 — Hook webhooks (WebhookConfig + fire_webhooks, 260502-uij)
3. ✅ P2.6 — Prompt injection scanner pub + zero-width + workspace.rs (260502-u9v)

**✅ Sprint 2 (значимый user-facing impact) — DONE 2026-05-03:**

1. ✅ P0.1 — Trajectory compression (Compressor + compress_messages + Hermes-style summary)

**✅ Sprint 3 (архитектурные) — DONE 2026-05-03:**

1. ✅ P1.1 — Compression chains (parent_session_id + bootstrap split + chain API + UI)

**Backlog (по запросу):**

- P1.2 — Session mirroring
- P1.4 — MCP serve mode
- P1.7 — Voice memo cache pipeline
- P2.* — только при появлении конкретных use cases

---

## Ссылки на reference-реализации

Все пути относительно `D:/GIT/hermes-agent`. Memory-snapshot этого проекта сохранён в `reference_hermes_agent.md` (auto-memory).

| Тема | Файл Hermes |
|------|-------------|
| Trajectory compression | `trajectory_compressor.py`, `agent/context_compressor.py` |
| DM pairing | `gateway/pairing.py` |
| Session mirroring | `gateway/mirror.py` |
| Cron delivery | `cron/scheduler.py`, `gateway/delivery.py` |
| FTS5 schema + trigram | `hermes_state.py` |
| MCP serve | `mcp_serve.py` |
| Hook system | `gateway/hooks.py` |
| Skill self-improvement | `tools/skill_manager_tool.py`, `agent/curator.py` |
| Subagent isolation | `tools/delegate_tool.py` |
| Prompt injection scanner | `agent/prompt_builder.py:36-73` |
| Memory provider plugins | `agent/memory_manager.py`, `plugins/memory/honcho/` |
| Multi-backend terminal | `tools/environments/{local,docker,ssh,daytona,singularity,modal}.py` |
| ACP adapter | `acp_adapter/{server,session,events,permissions}.py` |
| RL environments | `environments/`, `rl_cli.py` |
| Voice memo cache | `gateway/platforms/base.py:641-670` |

---

## История

- **2026-04-30** — Документ создан после анализа Hermes Agent (4 параллельных Explore-агента).
- Заменяет: `2026-03-30-openclaw-insights-plan.md` (удалён, его hardening-часть выполнена и осталась как `2026-03-30-openclaw-insights-hardening.md`).
- **2026-05-02** — Статусы обновлены по факту кода: P0.2 и P1.6 закрыты вне Sprint 1; P1.3, P1.5, P2.6 помечены как частично реализованные.
