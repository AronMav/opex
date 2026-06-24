# Capability-инструменты из активных провайдеров (авто-инструмент + failover)

**Дата:** 2026-06-24
**Статус:** Design v2 (одобрено в brainstorming + учтено адверсариальное ревью; ожидает финального spec review)
**Связано с:** [2026-06-20-websearch-provider-capability-design.md](2026-06-20-websearch-provider-capability-design.md) (ввёл multi-active + priority; явно отложил per-capability fallback в future work — эта спека его закрывает), [2026-06-24-provider-priority-drag-reorder-design.md](2026-06-24-provider-priority-drag-reorder-design.md) (UI приоритетов)

## Цель

Убрать «двойную настройку» media-возможностей. Сейчас, чтобы возможность
заработала, нужно настроить **и** провайдера в реестре (`provider_active`),
**и** иметь парный YAML-инструмент в `workspace/tools/`. Эти слои не связаны —
двойная точка отказа и лишняя ручная работа.

После изменения: **активной capability в реестре достаточно** — агент
автоматически получает встроенный инструмент. Единственная точка настройки —
реестр провайдеров. Дополнительно: описание инструмента, которое видит LLM,
динамическое (содержит имя текущего провайдера); при сбое — каскадный failover
по приоритету со «sticky»-памятью; та же логика выбора провайдера применяется и
к авто-обработке входящего медиа (FSE).

## Контекст (как сейчас)

- 6 capability активируются в `provider_active (capability, provider_name,
  priority)` (миграция 053 — multi-active с приоритетом уже есть): `stt, tts,
  vision, imagegen, embedding, websearch`. Есть `db::providers::get_active_providers(capability)`
  → `Vec<(name, priority)>` по приоритету ([db/providers.rs:164](../../crates/opex-core/src/db/providers.rs)).
- 5 capability имеют парный YAML-инструмент: `imagegen→generate_image`,
  `tts→synthesize_speech`, `websearch→search_web`, `stt→transcribe_audio`,
  `vision→analyze_image`. `embedding` инструмента не имеет (системный).
- Различия API провайдеров инкапсулированы в toolgate: общий `Protocol` на
  capability (`toolgate/providers/base.py`), стабильный эндпоинт. Смена
  провайдера контракт инструмента не меняет.
- toolgate умеет маршрутизировать к конкретному провайдеру по заголовку
  `X-Opex-Provider` ([toolgate/dependencies.py:29](../../toolgate/dependencies.py)).
- **Однако:** failover на сбое не реализован нигде (toolgate берёт топ-1,
  `aget_active`), а `require_provider` при **неизвестном** `X-Opex-Provider`
  **молча** падает на топ-1 (а не отдаёт ошибку).
- **Авто-обработка входящего медиа идёт не через инструменты, а через FSE**
  ([file_scenario/dispatch.rs](../../crates/opex-core/src/agent/file_scenario/dispatch.rs)):
  `run_transcribe` → toolgate `/transcribe`, `run_describe` → `/describe`
  (multipart, без `X-Opex-Provider`, без failover, топ-1).

## Решения (из brainstorming + ревью)

1. **Zero-config:** активная capability сама даёт агенту инструмент. YAML под
   эти 5 имён не нужен. Нет провайдера → инструмента в списке нет.
2. **Без YAML-override / без обратной совместимости.** Жёсткий cutover.
3. **Подход A (core-native):** встроенные capability-инструменты — новый слой в
   core поверх существующего HTTP-исполнения; динамическое описание, каскад и
   sticky-память — новый тонкий слой.
4. **Единый `CapabilityRouter`** — общий слой выбора провайдера для capability,
   используется И инструментами, И FSE (авто-STT/Vision). Одна sticky-память,
   одна модель приоритета/failover.
5. **Динамическое описание:** имя текущего здорового провайдера в описании
   инструмента; **стабильно байт-в-байт при неизменном провайдере** (чтобы не
   ломать prompt-cache); меняется только при реальной смене провайдера.
6. **Failover = каскад + sticky-память + last-resort.** Каскад по приоритету
   сверху вниз, пока один не отработает. Если **все** битые — last-resort:
   пробуем по порядку «наименее свежий сбой первым» (не отказываем на весь TTL).
7. **Sticky-семантика по типу сбоя:** 5xx / timeout / connection / 429 →
   глобальный `mark_broken` (это про здоровье провайдера). **401/403 → НЕ
   глобальный sticky** (специфично для ключа/агента) — failover на месте без
   глобальной пометки, чтобы сбой ключа агента A не ломал override агента B.
8. **Per-agent override** (`imagegen_provider`/`tts_provider`) = голова списка
   приоритета; проходит ту же broken-фильтрацию; **валидируется против
   active-списка capability** — если override не активен или битый, падаем в
   обычный каскад (лог-warning), а не шлём заведомо неизвестного провайдера.
9. **Toolgate правится** (HIGH-1/HIGH-2): strict-режим для `X-Opex-Provider` от
   core + проверка принадлежности провайдера к capability + passthrough
   реального upstream-статуса там, где сейчас всё схлопывается в 502.
10. **Структурный статус:** capability-путь получает `(status, body)`
    структурно, а не парсит код из строки `anyhow::Error`.

## Non-goals (YAGNI)

- `embedding` остаётся без agent-facing инструмента (системный путь — память).
- Health-check провайдеров (активная проверка) — не делаем; восстановление
  только по TTL.
- Персистентность sticky-памяти между рестартами не нужна (in-memory).
- FSE для документов (`/extract-text-url`) — не media-provider capability, не
  трогаем (embedding/documents вне scope).

## Обзор архитектуры

```text
┌─ LLM-инструменты ──────────────────────────────────────────────┐
│ LLM ← capability_tool_defs(state, agent): ToolDefinition с      │
│        description += "(current provider: <label>)"             │
│ LLM → вызов generate_image/synthesize_speech/search_web/...     │
│        ▼ CapabilityDispatch                                     │
├─ FSE (авто-медиа) ─────────────────────────────────────────────┤
│ входящее audio/image → run_transcribe / run_describe           │
│        ▼ (тот же CapabilityRouter)                             │
└────────────────────────────────────────────────────────────────┘
        │ order = CapabilityRouter.order(capability, agent_override)
        │ for provider in order:   // + last-resort если все битые
        │     exec(..., header X-Opex-Provider: <provider>, strict)
        │     (status, body) структурно
        │     if failover-сбой: mark_broken(тип-зависимо); continue
        │     else: return
        ▼
toolgate /<endpoint>  require_provider(strict): неизвестный provider → 503,
        провайдер чужой capability → ошибка; иначе тот самый provider
        ▼  passthrough реального upstream-статуса (не 502-схлопывание)
provider.generate/search/transcribe/describe/...
```

## Компоненты

### 1. `CapabilityRouter` (в `AppState` И `AgentState`)
Владеет sticky-состоянием, резолвит провайдеров. Должен быть доступен и из
`AppState`-кластеров (для сборки описаний/инструментов), и из `AgentState`
(media_background/FSE читают `ctx.state`, не `AppState` напрямую — см.
[agent_state.rs](../../crates/opex-core/src/agent/agent_state.rs)). Прецедент
in-memory DashMap-поля — `session_pools` в [clusters/agent_core.rs:24](../../crates/opex-core/src/gateway/clusters/agent_core.rs).

- Состояние: in-memory `DashMap<capability, DashMap<provider, broken_until: Instant>>`.
- `order(capability, agent_override) -> Vec<ProviderPick>`:
  1. из `get_active_providers(capability)` по приоритету;
  2. убрать провайдеры с непросроченным `broken_until`;
  3. если результат пуст → **last-resort**: вернуть полный active-список,
     отсортированный по `broken_until` возрастанию (наименее свежий сбой первым);
  4. если `agent_override` задан И присутствует в active-списке И не битый —
     поставить головой.
- `current(capability, agent_override) -> Option<Label>` — первый элемент
  `order(...)`, для описания.
- `mark_broken(capability, provider)` — `broken_until = now + TTL`; вызывается
  ТОЛЬКО для 5xx/timeout/connection/429.
- Лениво чистит просроченные записи при чтении.

### 2. Статический реестр спецификаций
Единый источник в Rust: `capability → CapabilityToolSpec`. Параметры берутся
**1:1 из текущих YAML** (имена точные — это LLM-контракт):

| capability | tool | toolgate path | params (точные имена) | channel_action | binary |
|---|---|---|---|---|---|
| imagegen | generate_image | /generate-image | prompt, size, quality | send_photo | да |
| tts | synthesize_speech | /v1/audio/speech | text | send_voice | да |
| websearch | search_web | /v1/search | query, max_results | — | нет |
| stt | transcribe_audio | /transcribe-url | audio_url, language | — | нет |
| vision | analyze_image | /api/vision/analyze (core-проксер) | image_url, question, language | — | нет |

Заметки (из ревью — не упустить при реализации):
- `synthesize_speech` НЕ имеет параметра `voice` (голос — из провайдера/агента);
  переносим `channel_action: send_voice` + body_template.
- `search_web` собирает тело через `body_template` с `{{#if}}` и
  `response_transform: $.results` — воспроизвести в спеке, не только параметры.
  **LLM-параметр `provider` удаляется** (выбор — через `CapabilityRouter`).
- `transcribe_audio`: `audio_url`/`language`, `response_transform: $.text`.
- `analyze_image`: `image_url`/`question`/`language`, `response_transform: $.description`.

### 3. Фабрика описаний `capability_tool_defs(state, agent)`
- Для каждой capability с непустым `order(...)` строит `ToolDefinition`:
  `description = base_description + " (current provider: <label>)"`. `label` из
  `media-drivers.yaml`, иначе `provider_name`. Текст **стабилен при неизменном
  провайдере**.
- Подключается в реальные точки сборки списка инструментов (ревью уточнило —
  их НЕ «5 одинаковых»):
  - LLM-видимый список: [context_builder.rs:551](../../crates/opex-core/src/agent/context_builder.rs),
    [pipeline/openai_compat.rs:27](../../crates/opex-core/src/agent/pipeline/openai_compat.rs),
    [pipeline/subagent_runner.rs:173](../../crates/opex-core/src/agent/pipeline/subagent_runner.rs);
  - tool_use-диспетчер (describe/search): [dispatcher/lookup.rs:66](../../crates/opex-core/src/agent/dispatcher/lookup.rs);
  - **name-set для visibility/dispatch:** `available_tool_names()` в
    [engine/context_builder.rs:403](../../crates/opex-core/src/agent/engine/context_builder.rs)
    — добавить capability-имена, иначе dispatch-проверка существования разойдётся.
- НЕ кэшировать в YAML-кэше ([engine/context_builder.rs:366](../../crates/opex-core/src/agent/engine/context_builder.rs)
  `load_yaml_tools_cached`, TTL 30s) и НЕ класть в `describe_cache`
  ([dispatcher/state.rs:14](../../crates/opex-core/src/agent/dispatcher/state.rs)) —
  иначе «current provider» залипнет.

### 4. `CapabilityDispatch` (исполнение с каскадом)
- Перехватывает имена из capability-реестра **раньше** YAML-ветки в
  [engine_dispatch.rs:168](../../crates/opex-core/src/agent/engine_dispatch.rs)
  (и в subagent-пути).
- Берёт `order(...)`, идёт сверху вниз; на каждом шаге исполняет через
  существующий HTTP-движок, добавляя `X-Opex-Provider` + strict-заголовок в
  `injected_headers` (движок их кладёт в запрос —
  [yaml_tools.rs:824](../../crates/opex-core/src/tools/yaml_tools.rs)); получает
  **структурный** `(status, body)`.
- Классификация (см. ниже) → `mark_broken` (тип-зависимо) + следующий, либо
  возврат, либо стоп-ошибка.
- **Бинарные/channel_action (generate_image, synthesize_speech):** failover
  встраивается ВНУТРЬ цикла повторов `execute_binary`, т.к. channel-доставка
  fire-and-forget ([media_background.rs:346](../../crates/opex-core/src/agent/pipeline/media_background.rs)
  `spawn()` возвращает «dispatched» до генерации). LLM-facing «все
  битые→last-resort/ошибка» применима к синхронному пути (UI
  `execute_inline_for_ui`, а также search/transcribe/vision).

### 5. Интеграция FSE (авто-STT/Vision)
- `DispatchInput` ([file_scenario/dispatch.rs:15](../../crates/opex-core/src/agent/file_scenario/dispatch.rs))
  расширяется доступом к `CapabilityRouter` (через `AgentState`/seam в
  [dispatch_seam.rs](../../crates/opex-core/src/agent/file_scenario/dispatch_seam.rs)).
- `run_transcribe`/`run_describe` каскадят по `order("stt")`/`order("vision")`,
  на каждом шаге добавляя `X-Opex-Provider` (+strict) к multipart-POST в
  `/transcribe`/`/describe`; failover-сбой → `mark_broken` + следующий; общая
  sticky-память с инструментами.
- Динамическое описание FSE не касается (нет LLM-инструмента) — выигрыш только
  failover + согласованность выбора провайдера.

### 6. Правки toolgate (Python)
- `require_provider` ([dependencies.py:29](../../toolgate/dependencies.py)): при
  заголовке strict-режима от core неизвестный `X-Opex-Provider` → **503
  `unknown_provider`** (не fallback на топ-1). Per-agent пользовательский
  override без strict сохраняет текущий мягкий fallback.
- `aget_instance` ([registry.py:233](../../toolgate/registry.py)) /
  `require_provider`: проверять, что провайдер принадлежит запрошенной
  capability (нельзя дёрнуть чужой инстанс).
- Passthrough реального upstream-статуса: `search.py`
  ([routers/search.py:35](../../toolgate/routers/search.py)) и core-проксер
  `api_vision_analyze` ([media.rs:367](../../crates/opex-core/src/gateway/handlers/media.rs))
  сейчас схлопывают всё в 502 — пробрасывать реальный статус (как уже делают
  tts/stt/imagegen/vision-direct), иначе классификатор failover слеп.

## Семантика failover

**Классификация (по структурному статусу):**
- failover к следующему + глобальный `mark_broken`: timeout, отказ соединения,
  5xx (после passthrough — реальный 5xx провайдера), 429.
- failover к следующему, но **без** глобального `mark_broken`: 401/403
  (специфично для ключа/конфига — не «здоровье» провайдера).
- стоп, ошибка агенту (без каскада): 400/422/404 — ошибка запроса/промпта,
  одинакова у всех.

**Last-resort:** если после фильтра `order()` пуст — пробуем полный список,
отсортированный по `broken_until` возрастанию.

**Восстановление:** по TTL (`broken_until`). Состояние in-memory.

## Конфигурация и политика

- `[capability_tools] failover_ttl_secs` в `opex.toml`, дефолт **300**.
- Tool policy `deny`/`allow` по имени — уважается (deny `generate_image` убирает
  из списка и запрещает вызов).
- Per-agent override — см. Решение 8.
- **Subagent-политика:** определить явно, видны ли capability-инструменты
  субагентам. Сейчас `SUBAGENT_DENIED_TOOLS` не содержит этих имён, а
  `integration_fse_security.rs` ассертит, что субагент НЕ имеет `analyze_image`
  — согласовать (либо добавить в denylist, либо обновить security-тесты).

## Миграция (hard cutover, без обратной совместимости)

1. Удалить из репо 5 YAML: `generate_image`, `synthesize_speech`, `search_web`,
   `transcribe_audio`, `analyze_image`.
2. **Системные обращения по имени** перевести на `CapabilityDispatch`:
   авто-TTS `maybe_auto_tts` ([engine/run.rs:248](../../crates/opex-core/src/agent/engine/run.rs)),
   `handle_tool_test` ([pipeline/handlers.rs:547](../../crates/opex-core/src/agent/pipeline/handlers.rs)).
3. **`has_tool`/`has_search`** ([engine/mod.rs:267](../../crates/opex-core/src/agent/engine/mod.rs))
   — сейчас проверяет файл на диске; рывайрить на capability-реестр, иначе блок
   «Web Search» исчезнет из системного промпта (потребители
   `context_builder.rs:265`, `subagent_runner.rs:65`, `openai_compat.rs:69`).
4. **`augment_search_web_description`** ([context_builder.rs:668](../../crates/opex-core/src/agent/context_builder.rs))
   — уже существующая функция динамического описания search_web; удалить/слить с
   `capability_tool_defs` (дубль) + обновить тесты `augments_search_web_*`
   (context_builder.rs:1122-1158).
5. **Skill-audit:** `audit_all_skills_required_tools_exist`
   ([skills/mod.rs:766](../../crates/opex-core/src/skills/mod.rs)) упадёт —
   добавить capability-имена в «known» (через `all_system_tool_names()` или
   отдельный список); обновить `media_processing_is_video_only` (skills/mod.rs:762).
6. **UI:** capability-инструменты сейчас невидимы (`GET /api/yaml-tools` и
   вкладка `tools/page.tsx` читают только файлы). Добавить источник (merge
   capability-spec в ответ `/api/yaml-tools` или отдельная read-only секция).
7. **Промпты/скиллы про `search_web(provider=...)`** — обновить
   ([workspace.rs:495](../../crates/opex-core/src/agent/workspace.rs),
   `workspace/skills/web-search.md`, `daily-briefing.md`): параметр `provider`
   удалён, выбор — через per-agent конфиг/router.
8. Деплой-заметка: удалить 5 файлов из живого воркспейса
   `~/opex/workspace/tools/` на сервере.
9. Обновить комментарии-упоминания: `middleware.rs:216`, `parallel.rs:566/1085`,
   `channel_actions.rs:112`.

## Тестирование (TDD — тесты вперёд)

**Unit:**
- `CapabilityRouter`: порядок по приоритету; фильтр битых по TTL; восстановление;
  **last-resort при всех битых** (сортировка по `broken_until`); override-голова;
  override вне active / битый override → обычный каскад; `mark_broken` только для
  5xx/429/timeout.
- Классификатор сбоев: 5xx/timeout/429 → failover+mark_broken; 401/403 →
  failover без mark_broken; 400/422/404 → стоп.
- Фабрика описаний: подстановка `label`; **стабильность текста при неизменном
  провайдере** (prompt-cache); смена после `mark_broken`.
- Tool policy `deny` убирает capability-инструмент.

**Integration (mock toolgate, wiremock):**
- Каскад «топ 5xx → следующий 200»: успех, топ помечен битым; `X-Opex-Provider`
  с нужным значением на каждом шаге.
- **strict-режим toolgate:** неизвестный `X-Opex-Provider` → 503 (а не тихий
  топ-1); провайдер чужой capability → ошибка.
- Все битые → last-resort пробует наименее свежий.
- binary/channel_action путь: `generate_image → send_photo`/`image_ready`,
  failover внутри `execute_binary`.
- **FSE-каскад:** авто-`describe`/`transcribe` каскадит и делит sticky-память с
  инструментом той же capability.

**Regression:**
- Тесты `media_background` (image_ready/send_photo/voice) через новый путь.
- FSE-тесты `dispatch.rs`/`dispatch_seam.rs` (включая
  `describe_503_provider_inactive_is_failed`) — поведение при 503 сохранено.
- `integration_fse_security.rs` (subagent без `analyze_image`) — согласовать с
  subagent-политикой.

## Open questions / future work

- Метрика/лог переключений провайдера (наблюдаемость) — желательно, вне scope.
- Возможный per-agent скоуп sticky-памяти (сейчас глобальный per-capability с
  исключением 401/403) — пересмотреть, если появятся agent-specific 5xx.
