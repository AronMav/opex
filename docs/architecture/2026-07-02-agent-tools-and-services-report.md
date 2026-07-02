# Отчёт: методы применения инструментов и сервисов агентами

Дата: 2026-07-02. Область: бэкенд (opex-core, toolgate, channels). Отчёт составлен по результатам
исследования кода; все пути указаны относительно корня репозитория.

---

## 1. Сводка

Агент видит единый интерфейс — массив `tools` в запросе к LLM — но за ним стоит **пять классов
исполнителей** с разными путями вызова:

1. **Системные инструменты** — in-process Rust-обработчики (workspace, memory, agent, cron, git и т.д.).
2. **YAML-инструменты** — декларативные HTTP-интеграции из `workspace/tools/*.yaml`; агент может
   создавать их сам в рантайме (`tool_create` → `tool_test` → `tool_verify`).
3. **MCP-инструменты** — внешние MCP-серверы в Docker-контейнерах (запуск on-demand через bollard).
4. **Сервисы через toolgate** — Python-шлюз медиавозможностей (STT/TTS/Vision/ImageGen/Embeddings/
   WebSearch/file handlers) с мультипровайдерным реестром.
5. **Channel actions** — побочные эффекты доставки (send_voice/send_photo в Telegram/Discord и т.д.),
   срабатывающие после успешного выполнения инструмента.

Ключевой архитектурный приём — **партиция core/extension**: десять «ядровых» инструментов
(`workspace_read/write/edit/list`, `code_exec`, `memory`, `agent`, `skill_use`, `web_fetch`, `tool_use`)
всегда предзагружены в контекст LLM, а остальные (YAML, MCP, условные системные) подключаются лениво
через мета-инструмент `tool_use` (действия `search` / `describe` / `call`). Это экономит токены контекста
и позволяет держать каталог инструментов практически неограниченным.

---

## 2. Таксономия: пути вызова

| Класс | Путь вызова | Исполнитель | Auth | Кэш | Побочные эффекты |
|---|---|---|---|---|---|
| Системные (~30 шт.) | LLM → `SystemToolRegistry::dispatch()` (HashMap trait-объектов) | `agent/tool_handlers/*.rs` + `agent/pipeline/handlers.rs` | не нужен (in-process) | — | файлы workspace, БД, сообщения |
| YAML | LLM → rewrite `tool_use(call)` → `tools/yaml_tools.rs` → HTTP | reqwest (обычный или SSRF-клиент) | bearer_env / basic_env / api_key / oauth_refresh / oauth_provider / custom; секреты из vault | 30s TTL, ключ = tool+method+endpoint+params | `channel_action` (медиа в канал) |
| MCP | LLM → `McpRegistry` → Docker RPC `tools/call` | контейнер MCP-сервера | своя у сервера | discovery на диске + в памяти | зависят от сервера |
| code_exec | LLM → `CodeExecHandler` → `containers/sandbox.rs` | персистентный Docker-контейнер агента | git-токены из OAuth-биндингов как env | — | файлы в /workspace |
| Субагенты | LLM → `agent` tool → `SessionAgentPool` | tokio-задача с собственным LLM-диалогом | — | состояние пула в памяти | вложенные tool-вызовы |
| Toolgate-сервисы | ядро/handlers → HTTP loopback :9011 | `toolgate/providers/*.py` (9 STT, 8 Vision, 8 TTS, 5 ImageGen, 2 Embedding, 3 WebSearch) | ключи провайдеров из реестра; `X-Opex-Provider` — override per-agent | реестр конфига 30s + ETag | доставка бинарников в каналы |
| browser_action | LLM → handler → HTTP :9020 (browser-renderer) | Playwright-рендерер | политика blocked_domains | — | скриншот/HTML |
| Channel actions | результат инструмента → `ChannelActionRouter` → WS → `channels/src/bridge.ts` | TS-драйверы (telegram/discord/slack/matrix/email/irc/whatsapp) | токены каналов из vault | — | сообщения/реакции/пины |

---

## 3. Жизненный цикл вызова инструмента (end-to-end)

1. **Сборка видимости** (`agent/pipeline/tool_defs.rs`, ~1300 строк). `build_internal_tool_definitions()`
   собирает системные инструменты по контексту агента: `is_base`, группы (`git`, `tool_management`,
   `skill_editing`, `session_tools`), наличие sandbox, URL browser-renderer («disabled» — инструмент
   не показывается). Затем добавляются YAML-инструменты (по статусу `verified`) и MCP.
2. **Фильтрация политикой** (`filter_tools_by_policy`): **deny проверяется первым** — даже ядровые
   системные инструменты можно запретить. Далее allowlist-семантика (`allow_all` /
   `deny_all_others` / явный `allow`). Для субагентов дополнительно применяется
   `runtime_subagent_denylist()` = константа `SUBAGENT_DENIED_TOOLS` ∪ `blocked_tools_extra`;
   ослабить встроенный запрет субагент не может.
3. **Семантический отбор** (`context_builder.rs::select_top_k_tools_semantic`) — опциональное
   ранжирование инструментов эмбеддингами относительно запроса пользователя (top-K, кэш
   `tool_embed_cache`).
4. **Вызов LLM** — отфильтрованный список уходит в `tools`; LLM возвращает `tool_calls`.
5. **Rewrite диспетчера**: вызовы `tool_use(action="call", name=X)` переписываются в прямой вызов X
   с повторной проверкой deny-списков; отклонённые вызовы получают синтетический результат-ошибку
   без выполнения.
6. **Approvals** (`agent/approval_manager.rs`): если инструмент в `require_for` /
   `require_for_categories` — выполнение блокируется; создаётся запись в БД, событие уходит в SSE,
   UI-уведомления и каналы; движок ждёт `tokio::oneshot` из `ApprovalWaitersMap` с таймаутом.
   Пользователь может **изменить аргументы** при одобрении — инструмент выполнится с новыми.
   `auto_approve_for_channels` пропускает согласование для автоматических каналов (cron и т.п.).
7. **Партиция parallel/sequential** (`agent/pipeline/parallel.rs`, ~1430 строк). Параллельно-безопасные
   системные инструменты (read-only: `web_fetch`, `memory`, `workspace_read`, `workspace_list` и др.)
   и YAML-инструменты с `parallel: true` без `channel_action` выполняются через `join_all`;
   остальные — последовательно. Таймаут: 120с по умолчанию, 600с для инструмента `agent`
   (`safety_timeout_secs`).
8. **Семантический кэш**: для кэшируемых инструментов (поиск, web_fetch, browser_render) — проверка
   по эмбеддингу с порогом сходства 0.95 до выполнения, сохранение после (TTL 3600с).
9. **Диспатч** (`engine_dispatch.rs::execute_tool_call`): SystemToolRegistry → YAML →
   ToolRegistry (внешние) → MCP. Вокруг — аудит (tool, session, params, duration, status),
   метрики и хуки.
10. **Hooks** (`agent/hooks.rs`): `BeforeToolCall` (sync-обработчики + async-webhooks в decision-режиме,
    могут заблокировать вызов или изменить аргументы), `AfterToolResult` (могут трансформировать
    результат — подмена помечается provenance-тегом вебхука).
11. **Loop detection** (`agent/tool_loop.rs`): двухфазный `LoopDetector` — `check_limits()` до
    выполнения (hash имени+аргументов; порог одинаковых вызовов, по умолчанию 10) и
    `record_execution()` после (3 последовательные ошибки одного инструмента → break; лимит итераций
    50). После рестарта детектор прогревается из `session_timeline` (только для режимов
    ResumeRunning/ExplicitResume).
12. **Качество инструментов** (`db/tool_quality.rs`): каждый результат пишется в таблицу
    `tool_quality` (последние 20 вызовов, `penalty_score`); `PenaltyCache` обновляется раз в 30с
    и используется для депрriorизации деградировавших инструментов; `/api/doctor` показывает их.
13. **Персист и инъекция в контекст**: `tool_start`/`tool_end` в `session_timeline`, результат — в
    `messages` (детачнутый tokio::spawn — переживает разрыв клиента). Результат усечён под бюджет
    контекста модели (`truncate_tool_result`), маркеры `__file__:` / `__rich_card__:` извлекаются
    (файлы попадают в таблицу `uploads` с HMAC-подписанными URL). Результаты возвращаются
    **в исходном порядке вызовов** (`assemble_ordered`) и уходят следующей итерации LLM.
14. **Финализация**: `end_turn` → finalize; `turn_limit`/loop-break → опциональный
    `ForcedFinalCallPolicy` (финальный вызов LLM без инструментов, для cron-путей).

---

## 4. Инвентарь системных инструментов

Регистрация: `agent/tool_registry.rs` (`SystemToolRegistry::build()`, trait `SystemToolHandler`,
HashMap по имени — без гигантского match). Обработчики: `agent/tool_handlers/*.rs`, логика — в
`agent/pipeline/handlers.rs` и соседних модулях pipeline.

| Категория | Инструменты | Примечания |
|---|---|---|
| Workspace | `workspace_write/read/list/edit/delete/rename`, `apply_patch`, `lsp` | запись с чекпойнтом и LSP-диагностикой; защита read-only путей (`workspace.rs::is_read_only`) |
| Память | `memory` (search/index/reindex/get/delete/update) | pgvector, гибридный поиск |
| Код | `code_exec` | Docker-sandbox, см. §7 |
| Делегирование | `agent` (ask/status/kill), `agents_list` | см. §8 |
| Коммуникации | `message`, `cron`, `git`, `process`, `browser_action` | cron: полный CRUD только для base-агентов |
| Управление инструментами | `tool_create/list/test/verify/disable/discover` | группа `tool_management`; discover — интроспекция OpenAPI/GraphQL |
| Диспетчер | `tool_use` (search/describe/call) | семантический поиск по каталогу (top-5) |
| Skills | `skill` (create/update/list), `skill_use` | загрузка скилла в системный промпт |
| Сессии | `session` (list/history/search/context/send/export) | группа `session_tools` |
| Прочее | `secret_set`, `web_fetch`, `todo`, `clarify`, `rich_card`, `canvas` | clarify — интерактивный запрос уточнения у пользователя |

---

## 5. YAML-инструменты: самодельные интеграции

`tools/yaml_tools.rs` (~3160 строк) — самый развитый механизм расширения. Возможности схемы:

- **Запрос**: method/endpoint/headers, параметры с `location: query|path|body|header`, значениями
  по умолчанию (в т.ч. `default_from_env` со скоупом агента), enum и примерами; `body_template`
  с mustache-подстановкой (JSON-экранирование), GraphQL-режим (query + variables).
- **Auth** (8 типов): `bearer_env`, `basic_env`, `api_key_header`, `api_key_query`, `custom`
  (шаблоны заголовков с `${ENV}`), `oauth_refresh` (обмен refresh-токена), `oauth_provider`
  (токен из OAuthManager по биндингу агента), `none`. Разрешение секретов: vault
  (`(name, scope)` → `(name, "")` → env). Секреты редактируются из сообщений об ошибках.
- **Ответ**: `response_transform` (JSONPath) и `response_pipeline` (jsonpath → pick_fields →
  sort_by → limit); бинарные ответы до 50 МБ.
- **Надёжность**: retry (429/5xx, backoff), кэш (TTL, настраиваемый набор ключевых параметров,
  LRU-эвикция), пагинация (offset/cursor/page, жёсткие капы страниц и общего объёма).
- **Безопасность**: выбор HTTP-клиента по endpoint — внутренние admin-сервисы
  (`tools::ssrf::is_internal_endpoint`: toolgate, browser-renderer, ядро) идут обычным клиентом,
  все внешние — через `ssrf_http_client()` с DNS-резолвером, блокирующим приватные диапазоны
  (закрывает DNS-rebinding). Имена инструментов валидируются `[a-zA-Z0-9_-]`.
- **Жизненный цикл, управляемый самим агентом**: `tool_discover` (генерация YAML из OpenAPI) →
  `tool_create` (draft) → `tool_test` (прогон с логом auth/запроса/трансформации) → `tool_verify`
  (draft → verified, появляется в каталоге) → `tool_disable`. Параллельно есть полный CRUD
  через HTTP API (`gateway/handlers/yaml_tools.rs`). `required_base: true` скрывает инструмент
  от не-base агентов.
- **`channel_action`**: после успешного ответа бинарный результат (`data_field: _binary` или
  JSONPath-поле) асинхронно отправляется в канал как voice/photo/file — не блокируя возврат
  результата в LLM-цикл.

---

## 6. MCP

`src/mcp/mod.rs`: реестр MCP-серверов с Docker-запуском on-demand (bollard). Discovery
(`tools/list` RPC) кэшируется двухуровнево — в памяти и на диске (`cache_dir/{name}.json`),
чтобы рестарт ядра не требовал подъёма контейнеров. Вызов — RPC `tools/call` в контейнер,
HTTP-клиент со 120с таймаутом. Имена серверов/инструментов валидируются тем же паттерном
`[a-zA-Z0-9_-]` (защита от path traversal в кэш-файлах и именах контейнеров).

---

## 7. code_exec: песочница

`containers/sandbox.rs`: персистентный Docker-контейнер на агента (образ `opex-core-sandbox`),
workspace смонтирован в `/workspace` (rw). Лимиты из `SandboxConfig`: память (мин. 512 МБ),
доля CPU, настраиваемый таймаут; внешний safety-таймаут инструмента — 600с. Захватываются
stdout/stderr/exit_code. В контейнер инжектируются git-креденшалы из OAuth-биндингов агента
(`PROVIDER_GIT_TOKEN`, `GIT_AUTHOR_*`). После выполнения `artifact_hook.rs` диффит SHA-256
снапшоты workspace и сообщает LLM созданные/изменённые файлы. Отдельный одноразовый режим
(джоб-контейнер `network=none`, read-only FS, лимит PID) используется для изолированных прогонов.

---

## 8. Субагенты (инструмент `agent`)

`agent/pipeline/agent_tool.rs` + `agent/session_agent_pool.rs`. Модель — **всегда живые пиры,
привязанные к сессии**: `LiveAgent` держит собственный LLM-контекст в памяти, получает сообщения
через mpsc, статус — AtomicU8 (idle/processing), ожидание результата — `tokio::sync::Notify`
(без поллинга). Пул на сессию (`SessionAgentPool`), глобальный кап 1000 пулов с LRU-эвикцией
и периодической чисткой (idle > 600с).

Ограничения делегирования (`[agent.delegation]` в TOML):
- `max_depth` — по умолчанию **1** (субагенты не могут спаунить дальше); глубина передаётся через
  `_context.subagent_depth`, некорректное значение трактуется fail-closed (u8::MAX — блок).
- `SUBAGENT_DENIED_TOOLS` (константа): `workspace_delete`, `workspace_rename`, `cron`, `secret_set`,
  `process`, `code_exec`, медиа-инструменты, `search_web`, `clarify`. Расширяется через
  `blocked_tools_extra`, ослабить нельзя.
- Любой агент сессии может ask/status/kill любого другого — peer-to-peer, без автоматического
  роутинга ходов.

---

## 9. Сервисы через toolgate

Toolgate (Python/FastAPI, loopback :9011) — медиашлюз с провайдерной абстракцией
(`toolgate/providers/base.py`): 9 STT-, 8 Vision-, 8 TTS-, 5 ImageGen-, 2 Embedding-,
3 WebSearch-провайдера. Активный провайдер на возможность выбирается через реестр
(таблицы `providers`/`provider_active` в ядре), toolgate тянет конфиг с ядра с TTL 30с + ETag
и деградирует мягко (503 `degraded: true`, последний известный конфиг сохраняется).
Per-agent override — заголовок `X-Opex-Provider`.

Пути использования агентами:
- **YAML-инструменты**, указывающие на toolgate-endpoints (`/tts`, `/stt`, `/vision`, `/imagegen`,
  `/search`, `/fetch`) — часто в связке с `channel_action` (синтез речи → голосовое в Telegram).
- **File Handler Hub**: самоописывающиеся Python-обработчики (`toolgate/handlers/builtin/*.py` +
  hot-reload `workspace/file_handlers/*.py`). Ядро дискаверит их условным GET `/handlers` (ETag),
  матчит по mime/размеру (`match_buttons`), запускает POST'ом **байтов** (multipart) — toolgate
  никогда не ходит по loopback-URL сам. Асинхронные джобы — таблица `handler_jobs` + воркер в ядре;
  колбэки прогресса/результата авторизуются per-job HMAC-токеном (защита от IDOR).
- **ctx API для обработчиков**: `stt`, `vision`, `tts`, `imagegen`, `search`, `embed`, `http`,
  `llm` (raw-вызов LLM через ядро), `progress`, `result` — то есть Python-обработчик получает
  весь стек возможностей платформы.
- **Эмбеддинги памяти**: ядро и memory-worker вызывают только toolgate `/v1/embeddings`;
  напрямую к Ollama/OpenAI ядро не ходит.

---

## 10. Skills и Curator

Скиллы — Markdown с YAML-frontmatter (`workspace/skills/*.md`; `config/skills/*.md` — только для
base-агентов): name/description/triggers/tools_required/priority/state/pinned. Совпавшие скиллы
инъецируются в системный промпт; `skill_use` загружает скилл явно (и реактивирует архивный).
Curator (`src/curator/`) ведёт жизненный цикл в три фазы: (1) переходы Active→Stale→Archived по
давности использования без LLM; (2) очередь починки сломанных скиллов агентом; (3) аналитика —
предложения archive/merge/fix. `pinned: true` полностью выводит скилл из-под Curator.

## 11. Cron / scheduler

`src/scheduler/` (tokio-cron-scheduler): агент планирует задания инструментом `cron`
(полный CRUD — только base). Запуск идёт изолированным пайплайном
(`handle_isolated_via_pipeline`, `BehaviourLayers::for_cron` — fallback-провайдер, auto-continue,
восстановление сессии, форсированный финальный вызов). Результат доставляется в `announce_to`:
`local` (файл в `workspace/agents/{agent}/cron_output/`) или канал (обрезка до 4000 символов,
полный текст — в workspace). Сентинел `HEARTBEAT_OK` в ответе подавляет анонс — паттерн
«тихо, если всё хорошо».

## 12. Channel actions

`agent/channel_actions.rs`: `ChannelActionRouter` — мультиканальный диспетчер
(`HashMap<"{type}:{conn_id}", mpsc::Sender>`, ёмкость 64 на канал — backpressure против OOM на
бинарниках). Результат с действием уходит по WS в TypeScript-адаптер (`channels/src/bridge.ts`),
который маппит его на драйвер платформы (send_voice/send_photo/react/pin/edit...). Подтверждение —
oneshot-ответ; таймаут результата сообщения 5 минут, access-check — 10 секунд fail-closed.

---

## 13. Оценка

### Сильные стороны

1. **Партиция core/extension + `tool_use`-диспетчер** — решает главную проблему tool-heavy агентов
   (раздувание контекста определениями инструментов) без потери доступности каталога. Семантический
   поиск инструментов по эмбеддингам — зрелое дополнение.
2. **Deny-first политика на всех уровнях** и неослабляемый субагентский denylist — последовательная
   модель наименьших привилегий (deny бьёт даже ядровые инструменты; fail-closed на глубине
   делегирования).
3. **Единый диспатч с аудитом**: каждый вызов проходит через одну точку (`execute_tool_call`) с
   таймлайном, аудит-логом, метриками, хуками и записью качества — трассируемость полная.
4. **Самообслуживание агентом**: цепочка `tool_discover → create → test → verify` позволяет агенту
   наращивать собственный инструментарий без деплоя, при этом draft-инструменты не видны до
   верификации, а SSRF-защита не обходится.
5. **Обратные связи по качеству**: LoopDetector (с прогревом из таймлайна после краша) +
   `tool_quality`/penalty_score — редкая для таких систем петля деградации/депрriorизации
   ненадёжных инструментов.
6. **Approvals с редактированием аргументов** — human-in-the-loop не «да/нет», а полноценная
   правка вызова; доставка запросов во все поверхности (SSE, UI, каналы).
7. **Провайдерная абстракция toolgate** — 30+ взаимозаменяемых медиапровайдеров за одним
   интерфейсом, per-agent override, мягкая деградация.

### Риски и слабые места

1. **`_session_tool_state` объявлен, но не используется** (`parallel.rs`) — задел под
   per-session состояние инструментов не реализован; describe-кэши глобальные.
2. **Глобальность penalty_score**: качество инструмента считается на процесс, а не на агента/
   контекст — инструмент, ломающийся у одного агента из-за его конфига, будет депрriorизирован
   у всех.
3. **Семантический кэш поиска (порог 0.95, TTL 1ч)** может отдавать устаревшие результаты для
   time-sensitive запросов; инвалидации по времени суток/событию нет.
4. **Хуки-вебхуки выполняются последовательно** в decision-режиме — медленный вебхук задерживает
   каждый вызов инструмента; таймауты есть, но бюджет на цепочку не суммируется.
5. **Прогрев LoopDetector не восстанавливает hash-детекцию** (таймлайн не хранит аргументы) —
   после рестарта детект одинаковых вызовов начинается с нуля; повторяющийся цикл может получить
   ещё до 10 итераций.
6. **Сложность YAML-механизма** (~3160 строк: auth, пагинация, pipeline, GraphQL, кэш, retry) —
   де-факто собственный mini-framework; тестовое покрытие критично, стоит следить за его ростом.
7. **`channel_action` привязывает YAML-инструменты к последовательному исполнению** — любой
   инструмент с медиа-доставкой теряет параллелизм; при интенсивном использовании медиа это
   заметно удлиняет ходы.

### Рекомендации

- Реализовать или удалить `_session_tool_state` (сейчас это мёртвый параметр в горячем пути).
- Рассмотреть скоупинг `penalty_score` по агенту (или паре агент+инструмент).
- Хранить хэш аргументов в `session_timeline.tool_end`, чтобы прогрев LoopDetector восстанавливал
  и детекцию повторов, а не только цепочку ошибок.
- Ограничить суммарный бюджет времени на цепочку decision-вебхуков одного вызова.
- Для семантического кэша поиска — параметр TTL на инструмент (у новостных/курсовых запросов — минуты).
