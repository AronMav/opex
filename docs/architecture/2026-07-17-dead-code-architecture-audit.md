# Полный аудит архитектуры: мёртвый и неработающий код (2026-07-17)

Метод: 6 параллельных статических аудитов (API-поверхность, Rust-ядро, UI, toolgate+channels,
БД, тулы/скиллы/инфраструктура). Каждая находка имеет file:line-evidence и уровень уверенности.
Cargo/сборка не запускались (Windows-машина не авторитетна); всё — статический анализ.

Итог одной строкой: **сломанных вызовов (404/рантайм-ошибок между компонентами) — ноль**;
основная масса проблем — мёртвая API-поверхность (~18% роутов без потребителей),
фантомные имена в защитных механизмах и «замороженные» фичи, у которых потерялась одна из сторон
(producer без consumer'а и наоборот).

---

## A. СЛОМАННОЕ — реальный эффект в проде (чинить в первую очередь)

### A1. Фантомное имя `process_start` в трёх защитных механизмах — security/корректность
Реальный тул называется `process` (tool_defs.rs:1079). Три места проверяют несуществующее имя:
- `crates/opex-core/src/agent/pipeline/behaviour.rs:60` — `NON_IDEMPOTENT_TOOLS` содержит
  `process_start` → interrupted-verify guard **никогда не блокирует** повторный
  `process(action="start")` после крэша;
- `crates/opex-core/src/gateway/handlers/agents/crud.rs:487` — авто-deny-list non-base агентов:
  запись no-op (спасает только base-only гейт на самом туле);
- `crates/opex-core/src/gateway/handlers/monitoring/doctor.rs:223` — `dangerous_tools`
  аудирует фантом, реальный `process` не проверяется.
Также stale-имя в `config/skills/agent-management.md:66` и `docs/ARCHITECTURE.md:505`
(там же мёртвые `memory_get`/`memory_delete`). CONFIDENCE: high.

### A2. `memory`-тул: enum действий разъехался с обработчиком
- `pipeline/tool_defs.rs:326` объявляет `compress` — обработчик (`tool_handlers/memory.rs:13-59`)
  его НЕ реализует → вызов = «unknown memory action».
- Обратно: обработчик поддерживает `reindex`, которого нет в enum → модель о нём не знает
  (при этом scaffold base/MEMORY.md его рекламирует). CONFIDENCE: high.

### A3. Активные скиллы поверх draft-тулов
`workspace/skills/smart-home.md`, `email-management.md`, `calendar-management.md` — все
`state: active`, но все их `tools_required` (ha_*, email_*, calendar_*) в `status: draft` →
не загружаются в LLM-контекст (yaml_tools.rs:1742). Скилл триггерится и инструктирует звать
тулы, которых нет в схеме. CONFIDENCE: high.

### A4. Скилл provider-management инструктирует мёртвый API
`config/skills/provider-management.md:59-75,167` — «PUT /api/provider-active {capability: imagegen}»;
после m084 (profiles) всё, кроме `embedding`, возвращает 400 (providers.rs:605-609).
Базовый агент, следуя скиллу, гарантированно упрётся в ошибку. Также
`workspace/skills/agent-audit.md:127` трактует provider-active по-старому,
`config/skills/toolgate-router.md:32` ссылается на упразднённый ручной TOOLS.md. CONFIDENCE: high.

### A5. `pending_messages`: очередь «гарантированной доставки» без producer'а
Consumer есть (`channel_ws/handshake.rs::replay_pending_messages` + cleanup в scheduler),
но первичного INSERT нет — единственный вызов `save_pending` находится внутри самого replay
(re-save при провале). Путь «движок сохраняет done/error при отключённом адаптере» исчез при
редизайне channel-WS. Таблица всегда пуста, механизм мёртв. Решение: восстановить producer
или снести таблицу+replay. CONFIDENCE: high.

### A6. Abort-учёт usage не подключён (m025-фича без вызова)
`usage_log.status` + partial-индекс `idx_usage_log_status_aborted`: `insert_aborted_row` и
константы `STATUS_ABORTED*` (`opex-db/src/usage.rs:71`) вызываются ТОЛЬКО из
`tests/integration_aborted_usage.rs`. Прод не пишет и не читает status. Похоже, обвязка
потерялась при рефакторинге стрим-слоя. CONFIDENCE: high.

### A7. Rename агента теряет пер-агентные данные в agent_name-таблицах
`agents/crud.rs` покрывает 21 agent_id-таблицу + messages + agent_channels + overrides + icon +
secrets-scope, но НЕ таблицы с колонкой `agent_name`: **`handler_config`** (пер-агентные валвы
обработчиков теряются — самое чувствительное), `tool_quality`, `handler_jobs`,
`pending_skill_repairs`. Дополняет уже известный delete-гэп
(docs/architecture/2026-07-17-agent-deletion-completeness-plan.md). CONFIDENCE: high.

### A8. `browser.yaml` (status: verified) указывает на docker-hostname
`workspace/tools/browser.yaml:3` — `http://browser-renderer:9020/automation`; core — нативный
хост-процесс, имя резолвится только внутри docker-сети → NXDOMAIN. Системный `browser_action`
работает через localhost:9020 — yaml-тул сломанный дубликат. CONFIDENCE: medium-high
(если на сервере нет ручного /etc/hosts).

### A9. `POST /api/files/{id}/run` — потерянный UI-вызов или ротные комменты
Комменты ядра (`tool_handlers/file_handler.rs:61-64`, `handler_registry.rs:159`,
`commands/dispatch.rs:14`) утверждают «sync handlers run inline via the composer's
/api/files/{id}/run path», но композер его НЕ вызывает (grep по ui — пусто). Либо мёртвый роут
и стейл-комменты, либо при рефакторинге композера потерян вызов. Рядом:
`GET /api/files/{id}/actions` тоже без потребителей (в UI остался только осиротевший тип
`FileActionButton`, types/api.ts:506-518). CONFIDENCE: high (факт отсутствия вызова).

### A10. UI-дрифт `guard_dropped`
`finalize.rs:129` возвращает `"guard_dropped"` (самый частый вид падений), но
`ui/src/types/api.ts:444` union и `monitor/page.tsx:360` FAILURE_KIND_BADGE его не знают →
бейдж рендерится как «other». Косметика, но по самому горячему виду фейлов. CONFIDENCE: high.

### A11. MCP-entries без контейнеров + мёртвый whitelist
`workspace/mcp/summarize.yaml` и `github.yaml` — `enabled: true`, но в docker-compose нет ни
сервисов, ни build-контекстов → on-demand старт через bollard обречён. Оба в whitelist рестарта
`services.rs:463,467`. CONFIDENCE: high (по репо).

### A12. Скаффолды учат новых base-агентов жить на «Pi»
`scaffold/base/MEMORY.md:13-14` («Key paths on Pi», `opex-core-aarch64`),
`scaffold/base/SOUL.md:8` («on the Pi») — Pi выведен из экосистемы. Плюс
`scaffold/base/MEMORY.md:76` ссылается на несуществующий скилл `verification`. CONFIDENCE: high.

### A13. Graceful shutdown классифицируется как UserCancelled
`cancellable_stream.rs:54` `set_shutdown_drain_reason` + `CancelReason::ShutdownDrain`
(«no runtime caller today») — shutdown-дренаж их не зовёт → при graceful shutdown стрим
помечается `UserCancelled` вместо `ShutdownDrain`. Замороженный контракт. CONFIDENCE:
high (мёртв), medium (что это дефект).

### A14. Мелкие продовые гэпы
- **email-канал нельзя создать из UI**: `channels/page.tsx:59` CHANNEL_TYPES без `"email"`,
  хотя core и адаптер поддерживают. CONFIDENCE: medium (возможно намеренно).
- **approval-allowlist write-only**: UI умеет только POST «Always allow»; GET-список и
  DELETE-роуты мертвы → посмотреть/удалить запись из UI невозможно.
- **unshareSession** (`ui/src/lib/api.ts:283`) — 0 вызовов: «отшэрить» сессию из UI нельзя.
- **toolgate registry-тесты ходят в сеть**: `registry.py:28` импорт `_aload_config_from_api`
  мёртв, тесты monkeypatch'ат его вхолостую и «зеленеют» из-за connection refused реального
  `_refresh`. CONFIDENCE: high (импорт), medium (вакуумность тестов).
- **minimax TTS отсутствует в config/media-drivers.yaml** — драйвер есть в registry.py:118,
  но из UI-каталога провайдера не создать.
- **`SinkError::Full`** матчится в execute.rs:277, но ни один sink его не конструирует —
  backpressure-задел не доведён.
- **`lsp`-тул виден всем агентам при выключенной фиче** (tool_defs.rs:181-211 добавляется
  безусловно, в отличие от гейтящегося browser_action) — загрязняет контекст, каждый вызов = ошибка.
- **Дрейф SYSTEM_TOOL_NAMES** (`pipeline/dispatch.rs:123-129`): `apply_patch`, `lsp`, `todo`,
  `clarify`, `code_orchestrate` не в pass-through списке → у агента с allow-политикой молча
  исчезают из схемы. CONFIDENCE: medium.

---

## B. МЁРТВАЯ API-ПОВЕРХНОСТЬ (48 из ~272 метод-роутов, BROKEN_CALL = 0)

Полный список с evidence — в отчёте аудитора; сводка по кластерам:

**Кластер 1 — «API-полнота» (задокументировано в docs/API.md, UI так и не подключён):**
- per-agent skills: GET/PUT/DELETE `/api/agents/{name}/skills[/{skill}]` (4 роута)
- per-agent yaml-tools: GET/POST×2 `/api/agents/{name}/yaml-tools*` (3 роута)
- memory legacy: GET/POST `/api/memory`, GET `/api/memory/export`, GET/PUT `fts-language`,
  DELETE/PATCH `/api/memory/{id}`, GET `/api/memory/tasks` (8 роутов; documents-роуты живые)
- config: GET `/api/config/export`, POST `/api/config/import`
- skills-детали: GET `versions/{vid}`, POST `snapshot`
- curator: POST `preview`, GET `runs/{id}`
- monitoring: GET `/api/usage/sessions`, GET `/api/audit/tools`,
  GET `/api/sessions/{id}/failures`, GET/PUT `/api/watchdog/config`
- cron: GET `/api/cron/runs` (глобальная история)
- providers: GET `{id}/resolve`, PATCH `{id}` (cli_options)
- services: GET `/api/services` (UI строит список из /api/status)
- agents: GET `{name}/hooks`, GET `{name}/context-breakdown` (T17 — бэк есть, UI читает из SSE),
  DELETE `{name}/icon`, GET `{name}/channels/{id}/status`

**Кластер 2 — потоки, ушедшие в WS-callbacks/меню:**
- POST `/api/agents/{name}/plan/day/{date}/approve|dismiss` (реальный поток — Telegram-callback
  `dpm:` через channel_ws/inline.rs:411 с прямым вызовом функций)
- GET `/api/files/{id}/actions`, POST `/api/files/{id}/run` (см. A9)
- POST/GET `/api/infra/decisions` (создание — in-core из infra-event; скилл прямо запрещает POST)

**Кластер 3 — legacy после рефакторингов:**
- GET `/api/oauth/providers` (сам помечен «backward compat», UI хардкодит список)
- google_auth device-flow стек: 5 роутов `/api/auth/google/*` — единственный потребитель
  smoke-тест существования роутов; UI ходит через generic oauth accounts+bindings
- GET/POST `/api/approvals/allowlist*` (см. A14)

**Не трогать (by design):** `/api/health/dashboard` (внешний мониторинг),
POST `/api/memory/reindex` (операторская ручка, задокументирована в CLAUDE.md),
все HMAC/internal/webhook/OpenAI-compat поверхности — потребители подтверждены.

---

## C. МЁРТВЫЙ КОД ПО СЛОЯМ (чистка)

### C1. Rust (детальный список — в отчёте аудитора; 9 крейтов, не 4)
Мёртвые функции (high): `create_isolated_session_with_user`, `claim_session_running`,
`get_last_user_message` (вытеснены `_with_mode`/`_with_id`-вариантами; opex-db/sessions.rs),
`token_for_session` (shares.rs:48), `load_mcp_prompt` (mcp/mod.rs:346),
`login_code_flow` (~64 строки, gemini oauth/flow.rs:442), `stream_shapes` (sink.rs:64),
`cassette_transport::with_mode`.

TEST_ONLY pub (прод не зовёт): `insert_pool_with_cap` (session_agent_pool.rs:232 —
**прод-eviction заинлайнен отдельно, тестируется копия**), `active_request_count`/
`unregister_request` (agent_state.rs), `first_matcher_matches` (hooks.rs:323),
`poll_device_flow` (device.rs:35), `sanitize_native_name`, `resolve_config_path_in`,
`EngineEventSender::send`/`inner`.

Мёртвые enum-варианты: `StreamEvent::AgentSwitch` (эмиттера нет),
`ProcessingPhase::CallingTool`/`Composing` (ноль конструирований),
`SinkError::Full`/`Fatal`, `WsEvent::AuditEvent` (UI подписан спекулятивно),
`ChannelOutbound::Reload` (мёртв с обеих сторон wire),
`HookEvent::AfterResponse`/`OnError`, `PartialState::Thinking`,
`CatalogSource::OpenRouter`/`LiteLlm`, `Caps::{attachment,reasoning,tool_call}`.

Мёртвые конфиг-ключи: `VideoConfig.{scene_threshold,frame_ceiling,job_timeout_secs,
url_allowlist,note_max_frames,vault_name}` (6 ключей, остатки Phase 6),
`ToolConfig.protocol/.api_key_env`, `McpConfig.protocol`; `[mcp]`-секция — echo-only;
`AgentSettings.{provider,...}` ×6 — читаются только одноразовой profile-миграцией
(помечены к удалению «через релиз»); `CAPABILITY_COMPACTION`.

Крейты/фичи: `opex-migrate-checksums` — orphan bin (не подключён никуда);
фича `otel` у opex-memory-worker + otel_init.rs (~95 строк) — не включается ни одним
деплой/CI-путём (у opex-core фича живая).

Резерв под известную дыру: `wipe_agent_memory` (store.rs:568) лежит `#[allow(dead_code)]`
ровно под agent-deletion-completeness план — задействовать при реализации или снести.

Шум: ~170 `#[allow(dead_code)]`, из них заметная часть — стейл-маркеры на ЖИВОМ коде
(lsp/*, dispatcher/lookup.rs, db/agent_plans.rs, clusters/auth_services.rs и др.) —
маскируют будущий реальный dead code, стоит снять.

### C2. UI (гигиена высокая: 3 мёртвых файла из 445, tsc чистый)
- `components/ui/scroll-area.tsx` (59), `components/ui/progress.tsx` (37) — 0 импортов;
- `components/workspace/markdown-editor.tsx` (100) — test-only, прод использует obsidian-editor;
- 9 React Query хуков в queries.ts живы только в тест-моках (`useProviderModels` замокан
  в 18 тест-файлах вхолостую): useTools, useProviderModels, useCuratorConfig, useUpdateAgent,
  useRestartService, useRebuildService, useHandlerAllowlist, useCreateHandler, useUpdateHandler;
- мёртвые экспорты: selectActiveSessionId, useSelectedBranches, clearLastSessionId,
  MESSAGES_HISTORY_LIMIT, apiGetBlob, inviteAgent (test-only);
- типы: FileActionButton/FileActionsResponse, AgentToolConfig, 16 из 17 back-compat алиасов ws.ts;
- ~32 мёртвых i18n-ключа ×2 локали (кластер context_window_* — фича удалена);
- 5 дефолтных SVG create-next-app в public/.
- 8 роутов-«страниц» без ссылок — это redirect-стабы (legacy-URL), не удалять без решения.

### C3. Toolgate + channels
- DEAD_ENDPOINT: `POST /summarize-video` (video.py:81 — работа идёт in-process),
  `POST /tts` (tts.py:69 — все ходят через /v1/audio/speech), `GET /handlers/{id}` (debug);
- DEAD_CODE: `UTILITY_SERVICES` (registry.py:152, вдобавок ссылается на несуществующий /fetch),
  `aload_config()` (config.py:79, легаси для тестов), `get_secret()` (workspace_helpers.py:30),
  `decodeBase64Param()` (channels common.ts:171), `loadedChannels()` (formatting.ts:55);
- DEAD_DEP в channels/package.json: `irc-framework` (irc.ts на raw net-сокетах),
  `matrix-bot-sdk` (matrix.ts на fetch), `@opentelemetry/api` (medium);
- провайдеры и builtin-обработчики — полный паритет с реестром и FSE_DEFAULT_ALLOWLIST.

### C4. БД (BROKEN_QUERY = 0; итоговая схема: 48 live + 1 dead + 3 deprecated)
- `pending_messages` — см. A5;
- deprecated подтверждены чистыми: `file_scenarios`, `file_scenario_outcomes`, `video_jobs`
  (0 обращений из кода) + их 5 индексов;
- мёртвые индексы: `idx_messages_role`, `idx_messages_tool_call`, `idx_stream_running`
  (medium); дубликаты: `idx_sessions_agent`/`idx_sessions_user` (m022 сам обещал cleanup),
  `idx_session_shares_token` (дублирует UNIQUE), `idx_pairing_codes_agent` (дублирует префикс PK);
- `messages.edited_at` — читается везде, не пишется никогда (всегда NULL);
- comment-drift: `infra_decisions` пишет статус `'triaging'`, не описанный в m080.

### C5. Doc-rot (вводит в заблуждение агентов и разработчиков)
- CLAUDE.md §Graceful Shutdown упоминает «Graph worker» — граф дропнут m018;
- CLAUDE.md пример `searxng_search.yaml` — файла нет; остатки searxng в INTERNAL_BLOCKLIST;
- `service_registry.rs:1-4` doc-comment врёт про расположение файлов;
- `handler_registry.rs:217` ссылается на несуществующий `/api/handlers/enqueue`;
- кластер комментов про «composer's /api/files/{id}/run path» (см. A9);
- `config/services/browser-renderer.yaml` url не читается кодом тула (env/хардкод);
- docs/ARCHITECTURE.md:505 — `process_start`, `memory_get`, `memory_delete`;
- `make test` реально гоняет `--features gemini-cloudcode` (CLAUDE.md описывает plain).

---

## D. Сводная статистика

| Слой | Сломано | Мёртво | BROKEN_CALL |
|---|---|---|---|
| Gateway API | 0 | 48/272 роутов (18%) | 0 |
| Rust-ядро | 3 (A1, A2, A13) | ~10 fn, ~15 enum-вариантов, ~15 конфиг-ключей, 1 крейт | — |
| UI | 1 (A10) | 3 файла, 9 хуков, ~30 экспортов/типов, 32 i18n-ключа | 0 (tsc чист) |
| Toolgate/Channels | 1 (тесты registry) | 3 эндпоинта, 6 функций, 3 deps, Reload | 0 |
| БД | 3 (A5, A6, A7) | 1 таблица, 1 колонка, ~9 индексов | 0 |
| Тулы/скиллы/инфра | 6 (A1-A4, A8, A11, A12) | 2 MCP-entries, 1 compose-кандидат | 0 |

Паттерны: (а) «CRUD-хвосты» — API строился на полноту, UI подключал выборочно;
(б) при рефакторингах (channel-WS, стрим-слой, FSE→handlers, profiles, Pi→server) регулярно
теряется ОДНА сторона контракта — producer, вызов из UI или инструкция скилла;
(в) `#[allow(dead_code)]` со стейл-комментами маскирует различие «резерв» vs «труп».
