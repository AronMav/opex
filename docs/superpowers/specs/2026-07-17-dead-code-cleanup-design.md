# Дизайн: чистка мёртвого кода и мёртвых роутов (2026-07-17)

Источник находок: `docs/architecture/2026-07-17-dead-code-architecture-audit.md`
(6-слойный статический аудит). Настоящий спек покрывает **разделы B (мёртвые роуты)
и C (мёртвый код)** аудита. Раздел A (починка сломанного поведения) — отдельный
последующий цикл, вне этого спека.

## Цель и границы

**Цель:** безопасно убрать однозначно мёртвый код и мёртвую API-поверхность, уменьшить
площадь сопровождения и убрать вводящий в заблуждение doc-rot/маркеры — не меняя рабочего
поведения системы.

**Ключевые решения (согласованы с владельцем):**
- Обратная совместимость НЕ требуется. Всё, что держится только ради compat, удаляется.
- Удаляем однозначно мёртвое; «замороженный резерв под будущие фичи» оставляем, но чиним
  вводящие в заблуждение комментарии и стейл-`#[allow(dead_code)]`.
- deprecated-таблицы (`file_scenarios`, `file_scenario_outcomes`, `video_jobs`) НЕ дропаем
  (history-preserving) — только подтверждаем, что код к ним не обращается.
- Схемные миграции БД в объёме: **дроп мёртвых индексов + мёртвой колонки `messages.edited_at`**.
- `usage_log.status` + abort-обвязка (`insert_aborted_row`, `STATUS_ABORTED*`) — **ОСТАВИТЬ**
  (резерв под подключение в цикле A). Колонку и индекс `idx_usage_log_status_aborted` НЕ дропаем.
- `AgentSettings` migration-only ключи (`provider`, `model`, `provider_connection`,
  `fallback_provider`, `tts_provider`, `imagegen_provider`) + `db/profile_migration.rs` —
  **СНЕСТИ оба** (прод мигрирован, compat не нужен).
- Фича `otel` у `opex-memory-worker` + `otel_init.rs` — **УДАЛИТЬ** (не собирается нигде;
  у `opex-core` otel остаётся).
- Роуты-UX-дыры (`context-breakdown`, `approvals/allowlist` GET+DELETE, `unshareSession`) —
  **УДАЛИТЬ сейчас** (отказ от недостроенных фич).

**Не входит в объём:** любые пункты раздела A аудита (фантом `process_start`, `memory(compress)`,
скиллы на draft-тулах, `pending_messages` producer, rename-гэп, `guard_dropped` UI-дрифт,
`minimax` в media-drivers.yaml и т.д.). Резервы, оставляемые нетронутыми: `wipe_agent_memory`,
`SinkError::Full`, `set_shutdown_drain_reason`/`CancelReason::ShutdownDrain` (последнее — часть
A13, чинится в цикле A).

## Архитектура выполнения: 5 батчей + сквозной doc-rot

Каждый батч — отдельная серия правок в master → своя верификация → свой деплой. Правки прямо
в master (принято в проекте). Ничего не пушим/деплоим без явного подтверждения владельца.

Порядок продиктован (а) границами деплоя проекта и (б) codegen-зависимостью:
`opex-types` генерит TS-типы для UI и channels, поэтому Rust-батч (с реген) обязан
задеплоиться до UI/channels-батчей.

```
Батч 1 (Rust + opex-types + gen-types)  ──deploy──▶  контрольная точка (doctor/logs)
         │
         ▼
Батч 2 (UI)  ──deploy──▶  точка       Батч 3 (toolgate + channels)  ──deploy──▶  точка
         │                                    │
         └──────────────┬─────────────────────┘
                        ▼
Батч 4 (миграции БД)  ──deploy(remote)──▶  точка
                        ▼
Батч 5 (мёртвые роуты + docs/API.md)  ──deploy──▶  финальный doc-rot проход
```

Между батчами — контрольная точка: `make doctor` + `make logs`. Батч сломался → `git revert`
+ redeploy, остальные не затронуты.

---

## Батч 1 — Rust (opex-core, opex-db, opex-types, прочие крейты)

**Мёртвые функции (удалить):**
- `opex-db/src/sessions.rs`: `create_isolated_session_with_user` (:452),
  `claim_session_running` (:874, вытеснена `_with_mode`), `get_last_user_message` (:1228,
  вытеснена `_with_id`)
- `opex-db/src/shares.rs:48` `token_for_session`
- `opex-core/src/mcp/mod.rs:346` `load_mcp_prompt`
- `opex-core/src/agent/providers/gemini_cloudcode/oauth/flow.rs:442` `login_code_flow` (~64 стр)
- `opex-core/src/agent/pipeline/sink.rs:64` `stream_shapes`
- `opex-core/src/agent/providers/cassette_transport.rs:130` `with_mode`

**TEST_ONLY `pub` (перевести в `#[cfg(test)]` или удалить, если тест не нужен):**
- `session_agent_pool.rs:232` `insert_pool_with_cap` — ВНИМАНИЕ: прод-eviction заинлайнен
  отдельно в `agent_tool::ask_spawn_new`; тест проверяет копию. При переводе в cfg(test)
  зафиксировать дублирование как долг (в цикле A свести к одной реализации).
- `agent_state.rs:182,187` `active_request_count`, `unregister_request`
- `hooks.rs:323` `first_matcher_matches`
- `gemini_cloudcode/oauth/device.rs:35` `poll_device_flow`
- `commands/spec.rs:98` `sanitize_native_name`
- `opex-gateway-util/src/config_path.rs:10` `resolve_config_path_in`
- `engine_event_sender.rs:92,108` `inner`, `send`

**Мёртвые enum-варианты (удалить + подчистить осиротевшие матчеры):**
- `stream_event.rs:108` `StreamEvent::AgentSwitch`
- `engine/stream.rs:21-22` `ProcessingPhase::CallingTool`, `::Composing`
- `pipeline/sink.rs:31,34` `SinkError::Fatal` (удалить). ПРИМ.: `SinkError::Full` — оставить
  (резерв backpressure; матчер в `execute.rs:277` остаётся).
- `hooks.rs:11,14` `HookEvent::AfterResponse`, `::OnError`
- `providers/error.rs:35` `PartialState::Thinking`
- `opex-catalog/src/lib.rs:86,89` `CatalogSource::OpenRouter`, `::LiteLlm`;
  `Caps::{attachment,reasoning,tool_call}` (115-122)
- **opex-types (генерят TS):** `channels.rs:266` `ChannelOutbound::Reload`,
  `ws.rs:135` `WsEvent::AuditEvent`

**Мёртвые конфиг-ключи (удалить):**
- `config/mod.rs:2067-2082` `VideoConfig.{scene_threshold, frame_ceiling, job_timeout_secs,
  url_allowlist, note_max_frames, vault_name}` (6 ключей + их default-фны; оставить
  `digest_provider`/`digest_model`)
- `config/mod.rs:824-825` `ToolConfig.protocol`, `.api_key_env`
- `config/mod.rs:849,878` `McpConfig.protocol`, `McpFileEntry.protocol`
- `db/providers.rs:56` `CAPABILITY_COMPACTION` (legacy provider_active-ключ)
- `config/mod.rs:973-999` `AgentSettings.{provider, model, provider_connection,
  fallback_provider, tts_provider, imagegen_provider}` **+ удалить `db/profile_migration.rs`**
  и снять его вызов из startup (main.rs)

**Крейты/фичи:**
- Удалить orphan-крейт `crates/opex-migrate-checksums/` целиком + убрать из `workspace.members`
- Удалить фичу `otel` у `opex-memory-worker` в его `Cargo.toml` + `otel_init.rs` + otel-ветки
  в его `main.rs` (у opex-core не трогать)

**Зачистка стейл-`#[allow(dead_code)]` на ЖИВОМ коде** (маркер врёт, маскирует будущий
реальный dead code): `agent/dispatcher/lookup.rs:8,102`, `agent/lsp/*` (~10 методов + manager
+ servers), `db/agent_plans.rs:22,42-50`, `gateway/clusters/auth_services.rs:14,25,35,71`,
`uploads.rs:326 mint_codemode_token`. FromRow/диагностические поля (sqlx) не трогать —
это норма.

**Оставить нетронутым (резерв):** `wipe_agent_memory` (store.rs:568 + трейт-метод в
memory_service.rs) — обновить комментарий, что это резерв под план agent-deletion.

**Doc-rot этого батча:** комментарии, ссылающиеся на удаляемое; `service_registry.rs:1-4`
(врёт про расположение файлов); `handler_registry.rs:217` (несуществующий `/api/handlers/enqueue`).

**Верификация:** локально `make check`; на сервере полная сборка + тесты
(`CARGO_BUILD_JOBS=4 nice ionice`, детачед) + `make lint` (clippy `-D warnings`) +
`gen-types` и проверка отсутствия codegen-дрифта (`api.generated.ts`/`sse.generated.ts`/
`ws.generated.ts`/channels `types.generated.ts`). **Деплой:** `make remote-deploy`.

---

## Батч 2 — UI

**Мёртвые файлы (удалить):**
- `ui/src/components/ui/scroll-area.tsx`, `ui/src/components/ui/progress.tsx`
- `ui/src/components/workspace/markdown-editor.tsx` (test-only) + его тест в
  `src/__tests__/pages-smoke.test.tsx` (снять ссылку/мок)

**Мёртвые хуки в `lib/queries.ts` (удалить + вычистить их моки из ~20 тест-файлов в том же
коммите):** `useTools`, `useProviderModels` (замокан в 18 файлах вхолостую),
`useCuratorConfig`, `useUpdateAgent`, `useRestartService`, `useRebuildService`,
`useHandlerAllowlist`, `useCreateHandler`, `useUpdateHandler`.

**Мёртвые экспорты:** `stores/chat-selectors.ts` `selectActiveSessionId` (:45),
`useSelectedBranches` (:102) + `selectSelectedBranches` (:62); `stores/chat-persistence.ts:34`
`clearLastSessionId`; `stores/chat-types.ts:18` `MESSAGES_HISTORY_LIMIT`;
`lib/api.ts` `apiGetBlob` (:202), `unshareSession` (:283, роут тоже удаляется в батче 5),
`inviteAgent` (:318, test-only).

**Мёртвые типы:** `types/api.ts` `FileActionButton`/`FileActionsResponse` (:510-519),
`AgentToolConfig` (:482); `types/ws.ts:13-31` — 16 back-compat алиасов (оставить только `WsLog`).

**Прочее:** ~32 мёртвых i18n-ключа × 2 локали (`en.json`/`ru.json`; кластер
`providers.context_window_*` и др. — плюрали `_one/_few/_many/_other` не трогать);
5 дефолтных SVG в `ui/public/` (`file/globe/next/vercel/window.svg`).

**Не удалять:** 8 redirect-стабов страниц (legacy-URL), `/overflow-check` (CI), `/share`,
кодген `api.generated.ts` (неиспользуемые типы — ожидаемо). `guard_dropped` UI-дрифт — это
раздел A, не трогаем здесь.

**Coordination:** уходит подписка UI на `WsEvent::AuditEvent` (согласовано с батчем 1).

**Верификация:** `npx tsc --noEmit` + `vitest` (только из `ui/`) + `npm run build`.
**Деплой:** `deploy-ui.sh` (union `_next/static`).

---

## Батч 3 — toolgate + channels

**toolgate — мёртвые эндпоинты (удалить + их тесты):**
- `routers/video.py:81` `POST /summarize-video` (работа идёт in-process через `video_helpers`)
- `routers/tts.py:69` `POST /tts` (все потребители на `/v1/audio/speech`)
- `handlers/router.py:89` `GET /handlers/{handler_id}` (debug, без потребителей)

**toolgate — мёртвый код:**
- `registry.py:152-155` `UTILITY_SERVICES` (+ ссылка на несуществующий `/fetch`)
- `registry.py:28` мёртвый импорт `_aload_config_from_api` + починить registry-тесты, которые
  monkeypatch'ат его вхолостую и реально ходят в сеть (`tests/test_registry.py:22,62,235`)
- `config.py:79` `aload_config()` (+ его тест) — легаси
- `workspace_helpers.py:30` `get_secret()` — ПРИМ.: задуман как API для внешних workspace-
  обработчиков; проверить на сервере наличие `workspace/file_handlers/*.py` вне git перед
  удалением (CONFIDENCE medium в аудите).

**channels — мёртвый код:** `src/drivers/common.ts:171` `decodeBase64Param` (+ его тест),
`src/formatting.ts:55` `loadedChannels`.

**channels — мёртвые deps (`package.json`):** `irc-framework` (драйвер на raw net-сокетах),
`matrix-bot-sdk` (драйвер на fetch), `@opentelemetry/api` (проверить, не peer-dep ли otel-
пакетов перед удалением — CONFIDENCE medium).

**Coordination:** снять обработку `ChannelOutbound::Reload` на TS-стороне
(`src/session.ts:210`, `src/bridge.ts:321`) + тест-фикстуру — согласовано с батчем 1.

**Верификация:** `bun test` (channels); pytest (toolgate) после починки сетевых registry-тестов;
сверка TS↔Rust протокола по регенённым типам. **Деплой:** toolgate — scp `.py` + restart;
channels — restart.

---

## Батч 4 — миграции БД

Новая миграция `migrations/mNNN_drop_dead_indexes.sql` (номер — следующий за m087):

**DROP INDEX:** `idx_messages_role`, `idx_messages_tool_call`, `idx_stream_running`,
`idx_sessions_agent`, `idx_sessions_user` (поглощены композитом m022/m072),
`idx_session_shares_token` (дублирует UNIQUE), `idx_pairing_codes_agent` (дублирует префикс PK).

**DROP COLUMN:** `messages.edited_at` (читается в history-SELECT'ах, но НИКОГДА не пишется →
всегда NULL). ВНИМАНИЕ: сначала (в батче 1/деплое перед этим) убрать чтение колонки из
SELECT-списков и DTO-маппинга в `opex-db/src/sessions.rs`, иначе запрос сломается. Поэтому
батч 4 идёт ПОСЛЕ деплоя Rust-батча.

**НЕ дропаем:** `usage_log.status` + `idx_usage_log_status_aborted` (резерв под цикл A);
deprecated-таблицы `file_scenarios`/`file_scenario_outcomes`/`video_jobs` + их индексы
(history-preserving); `pending_messages` (решение по ней — цикл A).

Использовать `IF EXISTS` для идемпотентности. **Верификация:** миграция автозапускается на старте
сервера при ближайшем `remote-deploy`; `make doctor` + `make logs` подтверждают успешный старт.

---

## Батч 5 — мёртвые роуты (раздел B аудита) + docs/API.md

Удалить регистрации роутов + их обработчики (осиротевшие обработчики — тоже Rust dead code,
компилятор поймает ссылки) + обновить `docs/API.md`.

**Удаляем** (полный список — раздел B аудита; кластеры):
- per-agent skills (4 роута), per-agent yaml-tools (3), memory legacy (8: GET/POST `/api/memory`,
  export, fts-language GET/PUT, DELETE/PATCH `{id}`, tasks), config export/import (2),
  skills versions/snapshot (2), curator preview + runs/{id} (2), monitoring
  (`/api/usage/sessions`, `/api/audit/tools`, `/api/sessions/{id}/failures`,
  watchdog/config GET/PUT), cron/runs, providers resolve + PATCH cli_options, `/api/services`,
  agents hooks + **context-breakdown** (UX-дыра, удаляем), icon DELETE, channels/{id}/status,
  plan/day approve+dismiss (поток в Telegram-callback), files/{id}/actions + files/{id}/run,
  infra/decisions POST+GET, oauth/providers (backward compat), google_auth device-flow (5),
  **approvals/allowlist GET + DELETE** (UX-дыры, удаляем).

**Оставляем (живой внешний контракт, НЕ compat-мусор):** `/v1/*` (OpenAI-compat),
HMAC-колбэки (`/api/files/jobs/*`, `/api/uploads/*`, `/api/internal/*`, `/api/sandbox/*`),
вебхуки, OAuth-callback, `/api/csp-report`, `/api/memory/reindex` (оператор),
`/api/health/dashboard` (внешний мониторинг).

**Доп. страховка ПЕРЕД деплоем батча 5:** grep по серверу (`~/opex`, nginx-конфиг,
systemd-юниты, `*.sh`) на обращения к удаляемым путям. Неожиданный потребитель → вынести в
отдельное решение, не сносить вслепую.

**Верификация:** сборка на сервере (осиротевшие обработчики) + `make doctor` + дымовая проверка
UI. **Деплой:** `make remote-deploy`.

---

## Финальный проход — сквозной doc-rot

После всех батчей — один проход по документации:
- `CLAUDE.md` §Graceful Shutdown (упоминание «Graph worker» — граф дропнут m018);
  пример `searxng_search.yaml` (файла нет); заметка про `make test` (реально гоняет
  `--features gemini-cloudcode`).
- `docs/ARCHITECTURE.md:505` — `process_start`, `memory_get`, `memory_delete` (мёртвые имена).
- Остатки `searxng` в `INTERNAL_BLOCKLIST` (ssrf.rs:87-88) — оценить, оставить ли.

## Риски и откат

- **Ложный «0 ссылок» (макросы/serde/роутинг-строки):** истинный гейт — компилятор/тесты на
  сервере, а не статика. Красная сборка ловит до деплоя.
- **Codegen-дрифт opex-types:** батч 1 деплоится до 2/3; `gen-types` в верификации батча 1.
- **`messages.edited_at`:** чтение колонки убирается в Rust-батче (деплой раньше), только потом
  DROP COLUMN в батче 4.
- **Внешний потребитель роута:** grep по серверу перед батчем 5; откат `git revert` + redeploy.
- **Изоляция батчей:** контрольная точка (`make doctor`/`make logs`) между батчами; сломанный
  батч откатывается независимо.

## Критерий готовности

Все 5 батчей задеплоены и прошли контрольные точки; `make doctor` зелёный; сборка на сервере
+ `make lint` + UI-тесты/tsc + channels/toolgate-тесты зелёные; codegen-дрифта нет;
`docs/API.md` синхронизирован с оставшимися роутами; doc-rot вычищен. Раздел A аудита остаётся
как отдельный запланированный цикл.
