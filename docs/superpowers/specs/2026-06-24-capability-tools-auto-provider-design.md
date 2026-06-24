# Capability-инструменты из активных провайдеров

**Дата:** 2026-06-24
**Статус:** Design v4 (одобрено в brainstorming + 2 раунда ревью + калибровка по Open WebUI; разбито на фазы; Фаза A готова к writing-plans)
**Связано с:** [2026-06-20-websearch-provider-capability-design.md](2026-06-20-websearch-provider-capability-design.md), [2026-06-24-provider-priority-drag-reorder-design.md](2026-06-24-provider-priority-drag-reorder-design.md)

## Цель

Убрать «двойную настройку» media-возможностей. Сейчас, чтобы возможность
заработала, нужно настроить **и** провайдера в реестре (`provider_active`), **и**
иметь парный YAML-инструмент. Эти слои не связаны — двойная точка отказа и
лишняя ручная работа.

После изменения: **активной capability в реестре достаточно** — агент
автоматически получает встроенный инструмент. Единственная точка настройки —
реестр провайдеров.

## Поэтапность (РЕШЕНО)

Разбито на две независимые поставки. Калибровка по Open WebUI (крупнейший
аналог) показала: даже он обходится **single-engine switch** без failover между
media-провайдерами — значит каскад/sticky избыточны для первой итерации и
выносятся в отдельную фазу.

- **Фаза A — Zero-config авто-инструменты (эта спека, делаем сейчас).**
  Активная capability → встроенный инструмент; описание содержит имя
  **топ-приоритетного** активного провайдера. Без failover, без sticky, без
  правок toolgate, без рефактора FSE/vision. Решает исходную боль.
- **Фаза B — Failover + sticky + полная унификация (отдельная спека/план,
  потом).** Каскад по приоритету, per-session sticky, классификатор сбоев,
  toolgate strict-режим + passthrough статуса, vision-проксер через router,
  интеграция FSE. Делается, когда появится реальный кейс с несколькими
  провайдерами на одну capability. Детали — в конце документа (раздел «Фаза B»).

---

# ФАЗА A — Zero-config авто-инструменты

## Контекст

- 6 capability в `provider_active (capability, provider_name, priority)` (м053,
  multi-active + priority). `db::providers::get_active_providers(capability)` →
  `Vec<(name, priority)>` по приоритету ([db/providers.rs:164](../../crates/opex-core/src/db/providers.rs));
  топ-1 — `get_provider_active` (LIMIT 1).
- 5 capability имеют парный YAML-инструмент: `imagegen→generate_image`,
  `tts→synthesize_speech`, `websearch→search_web`, `stt→transcribe_audio`,
  `vision→analyze_image`. `embedding` инструмента не имеет (системный).
- Различия API провайдеров инкапсулированы в toolgate (общий `Protocol`,
  стабильный эндпоинт). Смена провайдера контракт инструмента не меняет.

## Решения (Фаза A)

1. **Zero-config:** активная capability сама даёт инструмент. Нет провайдера →
   инструмента нет.
2. **Без YAML-override / без обратной совместимости.** Hard cutover.
3. **Описание = топ-провайдер.** `description += " (provider: <label>)"`, где
   label — топ-приоритетный активный провайдер capability (из
   `get_active_providers`). Стабильно, пока конфиг не меняется → prompt-cache
   не страдает.
4. **Исполнение переиспользует существующий HTTP-движок** (`YamlToolDef`,
   сконструированный в рантайме из спецификации) — он уже умеет
   `channel_action`, binary, `injected_headers`. **Без каскада** — один
   провайдер (топ-1 / per-agent override), как сейчас.
5. **Граница:** `embedding` вне (нет agent-facing инструмента). Авто-STT/Vision
   через FSE и vision-проксер `api_vision_analyze` в Фазе A **не меняются**
   (остаются на топ-1) — их унификация в Фазе B.

## Компоненты (Фаза A)

### 1. Статический реестр `CapabilityToolSpec`
Единый источник в Rust: `capability → { tool_name, base_description,
toolgate_path, params, channel_action?, binary? }`. Параметры **1:1 из текущих
YAML** (точные имена — LLM-контракт):

| capability | tool | path | params (точные) | channel_action | binary |
|---|---|---|---|---|---|
| imagegen | generate_image | /generate-image | prompt, size, quality | send_photo | да |
| tts | synthesize_speech | /v1/audio/speech | text | send_voice | да |
| websearch | search_web | /v1/search | query, max_results | — | нет |
| stt | transcribe_audio | /transcribe-url | audio_url, language | — | нет |
| vision | analyze_image | /api/vision/analyze (core-проксер) | image_url, question, language | — | нет |

Заметки:
- `synthesize_speech`: без параметра `voice`; перенести `channel_action: send_voice` + body_template.
- `search_web`: тело через `body_template` + `response_transform: $.results`;
  **LLM-параметр `provider` удаляется** (один провайдер = топ/override).
- `transcribe_audio`: `audio_url`/`language`, `response_transform: $.text`.
- `analyze_image`: `image_url`/`question`/`language`, `response_transform: $.description`.

### 2. Фабрика описаний `capability_tool_defs(state, agent)`
- Для каждой capability с ≥1 активным провайдером → `ToolDefinition`,
  `description = base + " (provider: <топ-label>)"` (label из
  `media-drivers.yaml`, иначе `provider_name`).
- Точки подключения (ревью уточнило — НЕ «5 одинаковых»):
  - LLM-список: [context_builder.rs:551](../../crates/opex-core/src/agent/context_builder.rs),
    [pipeline/openai_compat.rs:27](../../crates/opex-core/src/agent/pipeline/openai_compat.rs),
    [pipeline/subagent_runner.rs:173](../../crates/opex-core/src/agent/pipeline/subagent_runner.rs);
  - tool_use-диспетчер: [dispatcher/lookup.rs:66](../../crates/opex-core/src/agent/dispatcher/lookup.rs);
  - name-set visibility/dispatch: `available_tool_names()` [engine/context_builder.rs:403](../../crates/opex-core/src/agent/engine/context_builder.rs).
- НЕ кэшировать в YAML-кэше ([engine/context_builder.rs:366](../../crates/opex-core/src/agent/engine/context_builder.rs))
  и в `describe_cache` ([dispatcher/state.rs:14](../../crates/opex-core/src/agent/dispatcher/state.rs)).

### 3. `CapabilityDispatch`
- Перехват имён из реестра перед YAML-веткой в [engine_dispatch.rs:168](../../crates/opex-core/src/agent/engine_dispatch.rs) (и в subagent-пути).
- Исполнение через существующий движок (без каскада). Провайдер — per-agent
  override (`imagegen_provider`/`tts_provider`) если задан, иначе топ-1; как
  сегодня, через `X-Opex-Provider` в `injected_headers`
  ([yaml_tools.rs:824](../../crates/opex-core/src/tools/yaml_tools.rs)).
- Binary/channel_action — через существующий `media_background`
  ([media_background.rs](../../crates/opex-core/src/agent/pipeline/media_background.rs)),
  как сейчас YAML channel-action инструменты.

## Миграция (Фаза A, hard cutover)

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
   — НЕ дубль, а более богатая семантика (список всех провайдеров+приоритеты).
   **Намеренно заменяется** на single-provider, т.к. LLM-параметр `provider`
   удалён. Тесты `augments_search_web_*` (context_builder.rs:1122-1158)
   переписать под новый формат.
5. **Skill-audit:** `audit_all_skills_required_tools_exist`
   ([skills/mod.rs:766](../../crates/opex-core/src/skills/mod.rs)) — добавить
   capability-имена в «known»; обновить `media_processing_is_video_only` (skills/mod.rs:762).
6. **UI:** добавить источник для capability-инструментов (merge в `/api/yaml-tools`
   или отдельная read-only секция) — иначе невидимы (`tools/page.tsx`).
7. Промпты/скиллы про `search_web(provider=...)` — обновить ([workspace.rs:495](../../crates/opex-core/src/agent/workspace.rs),
   `workspace/skills/web-search.md`, `daily-briefing.md`).
8. **Subagent denylist:** добавить 5 имён в `SUBAGENT_DENIED_TOOLS`
   ([subagent.rs:17](../../crates/opex-core/src/agent/pipeline/subagent.rs)) —
   сохраняет инвариант `integration_fse_security.rs` без правки тестов.
9. Деплой-заметка: удалить 5 файлов из `~/opex/workspace/tools/` на сервере.
10. Комментарии: `middleware.rs:216`, `parallel.rs:566/1085`, `channel_actions.rs:112`.

## Конфигурация и политика (Фаза A)

- Tool policy `deny`/`allow` по имени — уважается.
- Per-agent override (`imagegen_provider`/`tts_provider`) — как сегодня (один
  провайдер; описание показывает его, если задан).

## Тестирование (Фаза A, TDD)

**Unit:**
- Фабрика описаний: подстановка топ-label; стабильность при неизменном конфиге;
  отсутствие инструмента, если нет активного провайдера.
- `CapabilityDispatch`: перехват перед YAML; per-agent override → `X-Opex-Provider`.
- Tool policy `deny` убирает инструмент; subagent denylist содержит 5 имён.

**Integration (mock toolgate, wiremock):**
- generate_image → send_photo / image_ready (binary/channel_action путь).
- synthesize_speech → send_voice.
- search_web/transcribe_audio — sync-путь через capability-инструмент.

**Regression:**
- `media_background` (image_ready/send_photo/voice) через новый путь.
- Авто-TTS (`maybe_auto_tts`) работает после удаления YAML.

---

# ФАЗА B — Failover, sticky, полная унификация (отложено)

Делается отдельной спекой/планом, когда появится реальная потребность в
нескольких провайдерах на одну capability. Зафиксировано здесь, чтобы не терять
проработку из ревью.

- **`CapabilityRouter` с per-session sticky-памятью** (`DashMap<(session,
  capability), {provider → broken_until}>`); `order()`/`current()`/`mark_broken()`.
- **Каскад по приоритету** сверху вниз, пока один не отработает; **last-resort**
  если все битые (сортировка по давности сбоя, без записи = первыми).
- **Классификатор сбоев (структурный статус):** 5xx/timeout/connection/429 →
  failover + `mark_broken`; 401/403/`503 unknown_provider` → failover без
  `mark_broken`; 400/422/404 → стоп; терминал при исчерпании `order()`.
- **Toolgate strict-режим** для `X-Opex-Provider` (неизвестный/чужой-capability
  → 503, не тихий fallback) + проверка capability в `aget_instance`
  ([dependencies.py:29](../../toolgate/dependencies.py), [registry.py:233](../../toolgate/registry.py)).
- **Passthrough реального upstream-статуса:** `search.py` и core-проксер
  `api_vision_analyze` (сейчас всё в 502).
- **Структурный `(status, body)`** из движка вместо парсинга строки `anyhow`.
- **Vision-контур:** сменить инъекцию `api_vision_analyze`
  ([media.rs:295](../../crates/opex-core/src/gateway/handlers/media.rs))
  `State<ConfigServices>` → state с router; каскад + passthrough; сохранить `/uploads/`.
- **Интеграция FSE (отдельная фаза):** протащить router+session через
  [dispatch_seam.rs](../../crates/opex-core/src/agent/file_scenario/dispatch_seam.rs)
  → `DispatchInput`; `run_transcribe`/`run_describe` каскадят, делят
  session-sticky; ~35 тестов FSE обновить.
- **Динамическое описание** меняется при `mark_broken`; стабильно при
  неизменном current в сессии (prompt-cache).
- Конфиг `[capability_tools] failover_ttl_secs` (дефолт 300).

## Open questions / future (Фаза B)

- Метрика/лог переключений провайдера; недоставленный channel-all-broken.
- Идея из Open WebUI: если chat-модель multimodal — слать картинку inline и
  пропускать `describe` (экономия vision-вызова) — отдельная задача.
- embedding-failover — отдельная задача, если понадобится.
