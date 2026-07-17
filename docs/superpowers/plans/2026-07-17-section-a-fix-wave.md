# Цикл A — fix-wave (раздел «сломанное» аудита 2026-07-17)

> Исполнять батчами по слоям деплоя. Каждый пункт верифицирован субагентом против HEAD (после чистки мёртвого кода, a2662e28). Вердикты: CONFIRMED — чиним; REFUTED/RESOLVED — пропуск; A13/SinkError::Full — пропуск (риск/бессмысленно).

**Гейты:** Rust — `cargo check --all-targets` + `clippy -D warnings` (сервер, `CARGO_BUILD_JOBS=4 nice ionice`); UI — `tsc` + vitest; toolgate — pytest (локально). Деплой по слоям, смоук после каждого.

**Правило безопасности:** config/workspace-файлы на сервере могли разойтись с репо (runtime-editable). Перед правкой серверной копии — сверить с репо; править ОБЕ стороны.

---

## Batch A-1 — Rust core (server-deploy.sh: pull→build→swap→restart)

Все правки собираются одной сборкой → один деплой.

### A1 — `process_start` фантом (PARTIAL: реален 1 гвард)
- `crates/opex-core/src/agent/pipeline/behaviour.rs:60` — в `NON_IDEMPOTENT_TOOLS` заменить `"process_start"` → `"process"` (гвард матчит по имени тула; тул называется `process`, tool_defs.rs:1079). **Это единственная правка с эффектом** (interrupted-guard начнёт блокировать двойной старт процесса).
- `crates/opex-core/src/gateway/handlers/agents/crud.rs:487` и `monitoring/doctor.rs:223` — тот же `"process_start"`→`"process"` для консистентности (косметика, эффекта на безопасность нет).
- `docs/ARCHITECTURE.md:505` — `process_start`→`process`; `memory_get`/`memory_delete` — это действия одного тула `memory`, не отдельные тулы (уточнить формулировку).
- Тест: добавить в `behaviour.rs` тесты кейс на `process` в `is_non_idempotent_tool`.

### A2 — memory enum разъехался с обработчиком (CONFIRMED)
- `crates/opex-core/src/agent/pipeline/tool_defs.rs:326` — enum action: `["search","index","get","delete","update","compress"]` → `["search","index","reindex","get","delete","update"]` (убрать мёртвый `compress`, добавить рабочий `reindex`).
- `tool_defs.rs:320` (description) — `...update/compress` → `...update/reindex`.
- Обработчик (`tool_handlers/memory.rs`) НЕ трогать — уже поддерживает нужный набор.

### A6 — abort-usage учёт не подключён (CONFIRMED)
- `crates/opex-core/src/agent/pipeline/finalize.rs` — в арках `FinalizeOutcome::Failed` (~513) и `Interrupted` (~618) добавить fire-and-forget `insert_aborted_row(..., STATUS_ABORTED)` по образцу `spawn_record_failure` (finalize.rs:501-509) и `record_usage`-spawn (execute.rs:592-610). `FinalizeContext` уже несёт session_id/llm_provider/llm_model/agent_name + partial (для оценки out-токенов `partial.len()/4`).
- Гард двойного учёта: abort-пути имеют `response.usage==None` → `record_usage` там не срабатывает, пересечения нет; сделать явным.
- STATUS_ABORTED_FAILOVER (execute.rs:~504, перед `adopt_fallback`) — вторично; можно во втором проходе.
- Индекс `idx_usage_log_status_aborted` жив (m088 его не дропал).

### A7 — rename теряет `agent_name`-таблицы (CONFIRMED, 4/4)
- `crates/opex-core/src/gateway/handlers/agents/crud.rs` — в rename-транзакцию рядом с `agent_channels` (~889-910) добавить 4 UPDATE:
  ```sql
  UPDATE handler_config        SET agent_name = $1 WHERE agent_name = $2
  UPDATE tool_quality          SET agent_name = $1 WHERE agent_name = $2
  UPDATE handler_jobs          SET agent_name = $1 WHERE agent_name = $2
  UPDATE pending_skill_repairs SET agent_name = $1 WHERE agent_name = $2
  ```
  bind `$1=new_name`, `$2=old name`. handler_config — самое чувствительное (валвы), остальные транзиентны, но аддитивно и дёшево.

### A11-rust — MCP whitelist (CONFIRMED, rust-часть)
- `crates/opex-core/src/gateway/handlers/services.rs:417,421` — убрать `mcp-summarize` и `mcp-github` из `RESTART_ALLOWED` (контейнеров нет → рестарт всё равно падает). (yaml `enabled:false` — в Batch A-4.)

### A12 — скаффолды учат «Pi» (CONFIRMED, include_str→rebuild)
- `crates/opex-core/scaffold/base/MEMORY.md:13` — «Key paths on Pi» → «Key paths on server (x86_64)»; `:14` `opex-core-aarch64` → `opex-core-x86_64`; `:76` ссылка на несуществующий скилл `verification` — убрать/заменить на существующий.
- `crates/opex-core/scaffold/base/SOUL.md:8` — «on the Pi» → «on the host».

### A14-minimax — каталог драйверов (CONFIRMED, include_str→rebuild)
- `config/media-drivers.yaml` секция `tts:` — добавить `- { driver: minimax, label: "MiniMax TTS", requires_key: true }` (формат сверить с соседними записями). Драйвер уже в registry.py:118.

### A14.2 — lsp виден при выключенной фиче (CONFIRMED)
- `crates/opex-core/src/agent/pipeline/tool_defs.rs` — добавить `pub lsp_enabled: bool` в `ToolDefsContext` (69-75); `lsp` (181-211) пушить через `if ctx.lsp_enabled { ... }` (паттерн browser_action, 969-971).
- Прокинуть `cfg.lsp.enabled` во все конструирования `ToolDefsContext` (context_builder.rs и др. — найти grep'ом).

### A14.3 — дрейф SYSTEM_TOOL_NAMES (CONFIRMED)
- `crates/opex-core/src/agent/pipeline/dispatch.rs:123-129` — добавить в `SYSTEM_TOOL_NAMES`: `"apply_patch"`, `"todo"`, `"clarify"`, `"code_orchestrate"`, `"lsp"` (иначе исчезают у агентов с allow/deny_all_others).
- Опц.: усилить тест `system_tool_names_is_subset...` обратной проверкой (каждый core-тул кроме `memory`/`tool_*` ∈ SYSTEM_TOOL_NAMES).

### A9 — doc-rot комменты про удалённый /api/files/{id}/run (RESOLVED→комменты)
- `agent/tool_handlers/file_handler.rs:63-64`, `agent/handler_registry.rs:159`, `agent/commands/dispatch.rs:14`, `gateway/handlers/handlers_admin.rs:4` — `/api/files/{id}/run` → `/api/files/run`; `/api/files/{id}/actions` — убрать/актуализировать. Только комменты, эффекта нет.

**Гейт A-1:** cargo check --all-targets + clippy на сервере зелёные. Смоук: /health 200, NRestarts=0, `process`-тул в схеме base-агента, memory action=reindex работает, rename не теряет handler_config (проверить).

---

## Batch A-2 — UI (deploy-ui.sh)

### A10 — бейдж guard_dropped (PARTIAL→UI)
- `ui/src/types/api.ts:~444` `SessionFailureKind` union — добавить `"guard_dropped"` (самодокумент; `|string` уже спасает от TS-ошибки).
- `ui/src/app/(authenticated)/monitor/page.tsx:~360` `FAILURE_KIND_BADGE` — добавить `guard_dropped` (warning-стиль), иначе цвет «other».

### A14-email — канал в UI (CONFIRMED)
- `ui/src/app/(authenticated)/.../channels/page.tsx:~59` `CHANNEL_TYPES` — добавить `"email"` (адаптер channels/src/drivers/email.ts + ядро уже поддерживают). Проверить, не нужна ли email-специфичная форма конфига.

**Гейт A-2:** tsc + vitest зелёные; prod-build. Смоук: email в списке каналов, guard_dropped бейдж цветной.

---

## Batch A-3 — toolgate (sync .py + restart)

### A14-registry — мёртвый импорт + вакуумные тесты (CONFIRMED)
- `toolgate/registry.py:28` — убрать неиспользуемый импорт `_aload_config_from_api` (реальный путь — `aload()` через httpx напрямую).
- `toolgate/tests/test_registry.py:22,62,235` — перевести monkeypatch с `registry._aload_config_from_api` на `registry.httpx.AsyncClient` (через `_install_fake_httpx`/аналог), иначе тесты остаются вакуумными (зеленеют по connection-refused).

**Гейт A-3:** pytest локально (тесты реально проверяют aload, а не вхолостую). Смоук: toolgate health 200.

---

## Batch A-4 — runtime config/workspace (серверная копия + репо)

Перед правкой серверной копии — сверить с репо (могли разойтись).

### A8 — browser.yaml docker-имя (CONFIRMED)
- `workspace/tools/browser.yaml:3` — `http://browser-renderer:9020/automation` → `http://localhost:9020/automation` (localhost:9020 уже в internal-списке ssrf.rs:85). Синхронизировать серверную копию `~/opex/workspace/tools/browser.yaml`.

### A11-yaml — MCP entries без контейнеров (CONFIRMED)
- `workspace/mcp/summarize.yaml`, `workspace/mcp/github.yaml` — `enabled: true` → `false`. Синхронизировать серверные копии.

### A4 — provider-management скилл учит мёртвый API (CONFIRMED)
- `config/skills/provider-management.md:59-68` — блок «Activate media provider» (PUT /api/provider-active для не-embedding → 400) переписать на управление через Профили; `provider-active` оставить только для `embedding`.
- `workspace/skills/agent-audit.md:127` — уточнить, что GET /api/provider-active показывает только embedding.
- (`toolgate-router.md` — REFUTED, TOOLS.md существует, не трогать.)
- Синхронизировать серверные `~/opex/config/skills/`, `~/opex/workspace/skills/`.

**Гейт A-4:** browser yaml-тул резолвится (localhost), MCP summarize/github не в whitelist и enabled:false, provider-management не советует мёртвый PUT.

---

## Batch A-5 — pending_messages: durable-доставка (ФИЧА, отдельный дизайн)

Решение владельца: **полноценный редизайн** (не «вернуть producer» — после рестарта адаптера теряется маршрутизация). Смоделировать по рабочей `outbound`-очереди (`handshake.rs:236 replay_outbound_queue`): хранить routing (chat_id/peer) + освежать correlation-id при replay. Затрагивает Rust (producer в `channel_ws/session_queue.rs:218-220` + схему таблицы), channels/src/bridge.ts (routing по сохранённому ключу, не по in-memory Map), миграцию. **Требует собственного дизайн-дока (brainstorming) перед реализацией.**

---

## Пропущено (по верификации)
- **A3** REFUTED — `filter_skills_by_available_tools` уже вычёркивает скиллы без доступных тулов. (Опц. косметика: архивировать 3 спящих скилла.)
- **A13** CONFIRMED, но diagnostic-only (метка abort_reason), фикс трогает shutdown hot-path — НЕ трогаем.
- **A14 SinkError::Full** — безвредный задел под backpressure, оборонительное match-плечо. Не трогаем.
- **A14 approval-allowlist GET/DELETE, unshareSession** — RESOLVED чисткой (feature-gap/уже удалено).
