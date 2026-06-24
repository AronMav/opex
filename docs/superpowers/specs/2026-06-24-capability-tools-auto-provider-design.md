# Capability-инструменты из активных провайдеров (авто-инструмент + failover)

**Дата:** 2026-06-24
**Статус:** Design v3 (одобрено в brainstorming + два раунда адверсариального ревью; блокеры закрыты; ожидает финального spec review)
**Связано с:** [2026-06-20-websearch-provider-capability-design.md](2026-06-20-websearch-provider-capability-design.md) (ввёл multi-active + priority; отложил per-capability fallback — эта спека его закрывает), [2026-06-24-provider-priority-drag-reorder-design.md](2026-06-24-provider-priority-drag-reorder-design.md) (UI приоритетов)

## Цель

Убрать «двойную настройку» media-возможностей. Сейчас, чтобы возможность
заработала, нужно настроить **и** провайдера в реестре (`provider_active`),
**и** иметь парный YAML-инструмент. Эти слои не связаны — двойная точка отказа
и лишняя ручная работа.

После изменения: **активной capability в реестре достаточно** — агент
автоматически получает встроенный инструмент. Единственная точка настройки —
реестр провайдеров. Дополнительно: динамическое описание (имя текущего
провайдера), каскадный failover по приоритету с per-session sticky-памятью, и
та же логика выбора провайдера для авто-обработки входящего медиа (FSE).

### Граница унификации (что честно покрывается)

`CapabilityRouter` становится единым выбором провайдера для **agent-facing
путей** capability stt/tts/vision/imagegen/websearch:

- LLM-инструменты (sync + binary/channel_action), openai_compat, субагенты —
  через единый choke-point `execute_tool_calls_partitioned → execute_tool_call_inner`;
- авто-TTS на исходящий ответ (`maybe_auto_tts`);
- авто-STT/Vision на входящее медиа (FSE);
- vision-проксер `api_vision_analyze` (получает доступ к router — см. §6).

**Вне унификации (осознанно):** `embedding` — нет agent-facing инструмента,
резолвится toolgate-ом напрямую (память/worker); живёт по старой модели
(top-1, без failover/sticky). Это Non-goal, а не дыра.

## Контекст (как сейчас)

- 6 capability в `provider_active (capability, provider_name, priority)`
  (миграция 053, multi-active + priority). `db::providers::get_active_providers(capability)`
  → `Vec<(name, priority)>` ([db/providers.rs:164](../../crates/opex-core/src/db/providers.rs)).
- 5 capability имеют парный YAML-инструмент: `imagegen→generate_image`,
  `tts→synthesize_speech`, `websearch→search_web`, `stt→transcribe_audio`,
  `vision→analyze_image`.
- Различия API провайдеров инкапсулированы в toolgate (общий `Protocol`,
  стабильный эндпоинт). Маршрутизация к конкретному провайдеру — заголовком
  `X-Opex-Provider`.
- **Проблемы текущего кода (подтверждены ревью):**
  - `require_provider` при неизвестном `X-Opex-Provider` **молча** падает на
    топ-1 ([toolgate/dependencies.py:34-37](../../toolgate/dependencies.py)).
  - failover нигде нет (top-1).
  - `search.py` и core-проксер `api_vision_analyze` схлопывают всё в **502**,
    теряя реальный upstream-статус ([routers/search.py:38](../../toolgate/routers/search.py),
    [media.rs:379](../../crates/opex-core/src/gateway/handlers/media.rs)).
  - `provider_header_for` читает только per-agent override, не `provider_active`
    ([media_background.rs:201](../../crates/opex-core/src/agent/pipeline/media_background.rs)).
  - авто-обработка медиа идёт через FSE (`run_transcribe`/`run_describe`,
    [file_scenario/dispatch.rs](../../crates/opex-core/src/agent/file_scenario/dispatch.rs)),
    multipart, без provider-заголовка.

## Решения (brainstorming + 2 раунда ревью)

1. **Zero-config:** активная capability сама даёт агенту инструмент. Нет
   провайдера → нет инструмента.
2. **Без YAML-override / без обратной совместимости.** Hard cutover.
3. **Подход A (core-native):** встроенные capability-инструменты + тонкий слой
   (описание/каскад/sticky) поверх существующего HTTP-исполнения.
4. **Единый `CapabilityRouter`** для всех agent-facing путей (см. «Граница
   унификации»).
5. **Динамическое описание:** `(current provider: <label>)`; **стабильно
   байт-в-байт при неизменном current** в рамках сессии (prompt-cache).
6. **Failover = каскад + per-session sticky + last-resort.**
7. **Sticky scope = per-session.** Ключ — `(session, capability, provider)`.
   Снимает кросс-загрязнение между агентами и стабилизирует prompt-cache
   активного диалога; цена — failover «переучивается» в каждой сессии
   (приемлемо).
8. **Sticky-семантика по типу сбоя:** 5xx / timeout / connection / 429 →
   `mark_broken` (в session-scope). **401/403 и `503 unknown_provider` → НЕ
   mark_broken** (специфично для ключа/конфига/рассинхрона), но failover к
   следующему.
9. **Per-agent override** (`imagegen_provider`/`tts_provider`) = голова списка;
   валидируется против active-списка capability на стороне core (если не активен
   — игнор + warning, обычный каскад); идёт по strict-пути.
10. **Toolgate strict-режим:** core всегда шлёт `X-Opex-Provider` + strict-флаг;
    неизвестный/чужой-capability провайдер → **503 `unknown_provider`** (не тихий
    fallback). Core трактует этот 503 как failover-сбой (Решение 8).
11. **Структурный статус:** capability-путь получает `(status, body)`
    структурно (не парсинг строки `anyhow`). Toolgate `search.py` и
    `api_vision_analyze` пробрасывают реальный upstream-статус.
12. **Терминальное правило:** если `order()` исчерпан без успеха (включая
    all-401/403) → структурная ошибка последнего провайдера (напр.
    `no working <capability> provider`), без зацикливания.

## Non-goals (YAGNI)

- `embedding` — без agent-facing инструмента (память/worker), отдельный контур.
- Health-check провайдеров — нет; восстановление только по TTL.
- Персистентность sticky между рестартами — нет (in-memory).
- FSE для документов (`/extract-text-url`) — не media-capability, не трогаем.

## Обзор архитектуры

```text
ВСЕ agent-facing пути выбора media-провайдера → CapabilityRouter (per-session)
  ├─ LLM sync tool (search_web/transcribe_audio)      ┐
  ├─ LLM binary/channel tool (generate_image/tts)     │ choke-point:
  ├─ openai_compat tools                               │ execute_tool_calls_partitioned
  ├─ subagent tools                                    ┘ → CapabilityDispatch
  ├─ авто-TTS исходящего ответа (maybe_auto_tts)       → CapabilityDispatch
  ├─ FSE входящего медиа (run_transcribe/run_describe) → router в DispatchInput
  └─ vision-проксер (api_vision_analyze)               → router через state

CapabilityDispatch / router-потребитель:
  order = router.order(session, capability, agent_override)   // - битые(session)
  for provider in order:            // + last-resort если все битые
      exec(..., X-Opex-Provider: <provider>, strict)
      (status, body) структурно
      match classify(status):
        5xx/timeout/429        → mark_broken(session,…); next
        401/403/503-unknown    → next (без mark_broken)
        400/422/404            → стоп, ошибка агенту
        2xx                    → return
  order исчерпан → терминальная ошибка (Решение 12)
        ▼
toolgate require_provider(strict): неизвестный/чужой provider → 503 unknown_provider;
        passthrough реального upstream-статуса
```

## Компоненты

### 1. `CapabilityRouter` (в `AppState` и `AgentState`)
Per-session sticky + резолв провайдеров. Доступен из `AppState`-кластеров (сборка
описаний) и `AgentState` (media_background/FSE читают `ctx.state`). Прецедент
in-memory DashMap — `session_pools` ([clusters/agent_core.rs:24](../../crates/opex-core/src/gateway/clusters/agent_core.rs)).

- Состояние: `DashMap<(SessionId, capability), DashMap<provider, broken_until: Instant>>`
  (или эквивалент с session в ключе).
- `order(session, capability, agent_override) -> Vec<ProviderPick>`:
  1. `get_active_providers(capability)` по приоритету;
  2. убрать провайдеры с непросроченным `broken_until` (в этой сессии);
  3. если пусто → **last-resort**: полный active-список, сортировка по
     «давности сбоя»: провайдеры без записи в sticky → как `epoch` (пробуем
     первыми), затем по `broken_until` возрастанию;
  4. если `agent_override` активен и не битый — голова списка.
- `current(session, capability, agent_override)` — первый из `order`, для описания.
- `mark_broken(session, capability, provider)` — `broken_until = now + TTL`;
  ТОЛЬКО для 5xx/timeout/connection/429.
- Лениво чистит просроченные и завершённые сессии.

### 2. Статический реестр спецификаций
`capability → CapabilityToolSpec`. Параметры **1:1 из текущих YAML** (точные
имена — LLM-контракт):

| capability | tool | toolgate path | params (точные имена) | channel_action | binary |
|---|---|---|---|---|---|
| imagegen | generate_image | /generate-image | prompt, size, quality | send_photo | да |
| tts | synthesize_speech | /v1/audio/speech | text | send_voice | да |
| websearch | search_web | /v1/search | query, max_results | — | нет |
| stt | transcribe_audio | /transcribe-url | audio_url, language | — | нет |
| vision | analyze_image | /api/vision/analyze (core-проксер) | image_url, question, language | — | нет |

Заметки реализации (из ревью):
- `synthesize_speech` без параметра `voice` (голос из провайдера/агента); перенести
  `channel_action: send_voice` + body_template.
- `search_web`: тело через `body_template` + `response_transform: $.results`;
  **LLM-параметр `provider` удаляется** (выбор — через router).
- `transcribe_audio`: `audio_url`/`language`, `response_transform: $.text`.
- `analyze_image`: `image_url`/`question`/`language`, `response_transform: $.description`.
- vision имеет ДВА upstream-пути одной capability: инструмент → core-проксер →
  toolgate `/describe-url`; FSE → toolgate `/describe` (multipart). Оба обязаны
  давать сопоставимый структурный `(status, body)` для общей session-sticky.

### 3. Фабрика описаний `capability_tool_defs(state, agent, session)`
- Для каждой capability с непустым `order(...)` → `ToolDefinition`,
  `description = base + " (current provider: <label>)"`. `label` из
  `media-drivers.yaml`, иначе `provider_name`. Текст стабилен при неизменном
  current в сессии.
- Точки подключения (ревью уточнило — НЕ «5 одинаковых»):
  - LLM-список: [context_builder.rs:551](../../crates/opex-core/src/agent/context_builder.rs),
    [pipeline/openai_compat.rs:27](../../crates/opex-core/src/agent/pipeline/openai_compat.rs),
    [pipeline/subagent_runner.rs:173](../../crates/opex-core/src/agent/pipeline/subagent_runner.rs);
  - tool_use-диспетчер: [dispatcher/lookup.rs:66](../../crates/opex-core/src/agent/dispatcher/lookup.rs);
  - name-set visibility/dispatch: `available_tool_names()`
    [engine/context_builder.rs:403](../../crates/opex-core/src/agent/engine/context_builder.rs).
- НЕ кэшировать в YAML-кэше ([engine/context_builder.rs:366](../../crates/opex-core/src/agent/engine/context_builder.rs))
  и НЕ в `describe_cache` ([dispatcher/state.rs:14](../../crates/opex-core/src/agent/dispatcher/state.rs)).

### 4. `CapabilityDispatch`
- Перехват имён из реестра перед YAML-веткой в
  [engine_dispatch.rs:168](../../crates/opex-core/src/agent/engine_dispatch.rs) (и в subagent-пути).
- Каскад по `order(session, …)`; на каждом шаге — `X-Opex-Provider` + strict в
  `injected_headers` (движок кладёт их в запрос,
  [yaml_tools.rs:824](../../crates/opex-core/src/tools/yaml_tools.rs)); получает
  структурный `(status, body)`.
- **Binary/channel_action:** failover ВНУТРИ `execute_binary`-цикла, т.к.
  доставка fire-and-forget ([media_background.rs:346](../../crates/opex-core/src/agent/pipeline/media_background.rs)).
  Терминальный all-broken на channel-пути НЕ доставляется LLM (агент уже получил
  «dispatched») — только в канал/лог (см. §Семантика).

### 5. Интеграция FSE (отдельная фаза, не «тонкий слой»)
- Протащить `CapabilityRouter` + `session` через
  [dispatch_seam.rs](../../crates/opex-core/src/agent/file_scenario/dispatch_seam.rs)
  (`dispatch_attachments` → `run_builtin`) в `DispatchInput`
  ([dispatch.rs:15](../../crates/opex-core/src/agent/file_scenario/dispatch.rs)).
  Это смена сигнатур по FSE-цепочке + обновление ~35 тестов
  `dispatch.rs`/`dispatch_seam.rs` — планировать как полноценную фазу.
- `run_transcribe`/`run_describe` каскадят по `order(session,"stt"/"vision")`,
  добавляя `X-Opex-Provider`+strict к multipart-POST; общая session-sticky.

### 6. Vision-контур (замкнуть)
- Сменить инъекцию `api_vision_analyze`
  ([media.rs:295](../../crates/opex-core/src/gateway/handlers/media.rs)):
  `State<ConfigServices>` → state с доступом к `CapabilityRouter` + `session`.
- Хендлер каскадит по `order(session,"vision")`, прокидывает `X-Opex-Provider`
  +strict в toolgate `/describe`/`/describe-url`, **пробрасывает реальный
  upstream-статус** (не схлопывать в 502). Сохраняет обработку `/uploads/`.

### 7. Правки toolgate (Python)
- `require_provider` ([dependencies.py:29](../../toolgate/dependencies.py)):
  strict-флаг от core → неизвестный/чужой-capability `X-Opex-Provider` → **503
  `unknown_provider`** (не fallback).
- `aget_instance` ([registry.py:233](../../toolgate/registry.py)) /
  `require_provider`: проверять принадлежность провайдера к запрошенной capability.
- Passthrough реального upstream-статуса: `search.py` и core-проксер
  `api_vision_analyze` (сейчас всё в 502).

## Семантика failover

**Классификация (структурный статус):**
- 5xx / timeout / connection / 429 → failover + `mark_broken(session)`.
- 401 / 403 / **503 `unknown_provider`** → failover **без** `mark_broken`
  (конфиг/рассинхрон, не «здоровье»).
- 400 / 422 / 404 → стоп, ошибка агенту.
- 2xx → возврат.

**Last-resort:** `order()` пуст после фильтра → полный список (без записи sticky
= epoch первыми, далее по `broken_until` возрастанию).

**Терминал:** `order()` исчерпан без успеха (вкл. all-401/403) → структурная
ошибка последнего провайдера, без зацикливания и повторного last-resort.

**Binary/channel_action:** каскад внутри `execute_binary`; терминальный
all-broken НЕ доставляется LLM (fire-and-forget) — только канал/лог.

**Восстановление:** по TTL (`broken_until`), per-session, in-memory.

## Конфигурация и политика

- `[capability_tools] failover_ttl_secs` в `opex.toml`, дефолт **300**.
- Tool policy `deny`/`allow` по имени — уважается.
- Per-agent override — Решение 9.
- **Subagent-политика (РЕШЕНО):** добавить `generate_image, synthesize_speech,
  analyze_image, transcribe_audio, search_web` в `SUBAGENT_DENIED_TOOLS`
  ([subagent.rs:17](../../crates/opex-core/src/agent/pipeline/subagent.rs)) —
  сохраняет security-инвариант `integration_fse_security.rs` без правки тестов.
  (При необходимости выдать субагенту конкретную capability — пересмотреть точечно.)

## Миграция (hard cutover)

1. Удалить 5 YAML: `generate_image`, `synthesize_speech`, `search_web`,
   `transcribe_audio`, `analyze_image`.
2. Системные обращения по имени → `CapabilityDispatch`: `maybe_auto_tts`
   ([engine/run.rs:248](../../crates/opex-core/src/agent/engine/run.rs)),
   `handle_tool_test` ([pipeline/handlers.rs:547](../../crates/opex-core/src/agent/pipeline/handlers.rs)).
3. **`has_tool`/`has_search`** ([engine/mod.rs:267](../../crates/opex-core/src/agent/engine/mod.rs))
   — рывайрить на capability-реестр (иначе блок «Web Search» исчезнет из
   промпта; потребители `context_builder.rs:265`, `subagent_runner.rs:65`,
   `openai_compat.rs:69`).
4. **`augment_search_web_description`** ([context_builder.rs:668](../../crates/opex-core/src/agent/context_builder.rs))
   — это НЕ дубль, а более богатая семантика (список всех провайдеров+приоритеты).
   **Намеренно заменяется** на single-current-provider, т.к. LLM-параметр
   `provider` удалён и список более не actionable. Тесты `augments_search_web_*`
   (context_builder.rs:1122-1158) **переписать под новый формат**, не адаптировать.
5. **Skill-audit:** `audit_all_skills_required_tools_exist`
   ([skills/mod.rs:766](../../crates/opex-core/src/skills/mod.rs)) — добавить
   capability-имена в «known»; обновить `media_processing_is_video_only` (skills/mod.rs:762).
6. **UI:** добавить источник для capability-инструментов (merge в `/api/yaml-tools`
   или отдельная read-only секция) — иначе они невидимы (`tools/page.tsx`).
7. Промпты/скиллы про `search_web(provider=...)` — обновить ([workspace.rs:495](../../crates/opex-core/src/agent/workspace.rs),
   `workspace/skills/web-search.md`, `daily-briefing.md`).
8. Деплой-заметка: удалить 5 файлов из `~/opex/workspace/tools/` на сервере.
9. Комментарии: `middleware.rs:216`, `parallel.rs:566/1085`, `channel_actions.rs:112`.

## Тестирование (TDD)

**Unit:**
- `CapabilityRouter` (per-session): порядок по приоритету; фильтр битых по
  session+TTL; восстановление; last-resort (tie-break: без записи = первыми);
  override-голова; override вне active / битый → каскад; `mark_broken` только
  5xx/429/timeout; **изоляция между сессиями** (битый в S1 не влияет на S2).
- Классификатор: 5xx/timeout/429 → failover+mark_broken; 401/403/503-unknown →
  failover без mark_broken; 400/422/404 → стоп; **all-401 → терминальная ошибка**.
- Фабрика описаний: подстановка `label`; стабильность при неизменном current;
  смена после `mark_broken`.
- Tool policy `deny`; subagent denylist содержит 5 имён.

**Integration (mock toolgate, wiremock):**
- Каскад «топ 5xx → следующий 200»; `X-Opex-Provider` корректен на каждом шаге.
- strict: неизвестный/чужой-capability провайдер → 503 unknown_provider →
  failover без mark_broken.
- Все битые → last-resort.
- binary/channel_action: `generate_image → send_photo`/`image_ready`, failover
  внутри `execute_binary`.
- FSE-каскад: авто-`describe`/`transcribe` каскадит, делит session-sticky с
  инструментом; изоляция между сессиями.
- vision-проксер: каскад + passthrough статуса + `/uploads/` сохранён.

**Regression:**
- `media_background` (image_ready/send_photo/voice) через новый путь.
- FSE-тесты (`describe_503_provider_inactive_is_failed` и пр.).
- `integration_fse_security.rs` — зелёный благодаря subagent denylist.

## Open questions / future work

- Метрика/лог переключений провайдера и недоставленного channel-all-broken.
- embedding-failover (если появится потребность) — отдельная задача.
