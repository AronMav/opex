# Capability-инструменты из активных провайдеров (авто-инструмент + failover)

**Дата:** 2026-06-24
**Статус:** Design (одобрено в brainstorming, ожидает spec review)
**Связано с:** [2026-06-20-websearch-provider-capability-design.md](2026-06-20-websearch-provider-capability-design.md) (вводит multi-active + priority; явно отложило per-capability fallback в future work — эта спека его закрывает), [2026-06-24-provider-priority-drag-reorder-design.md](2026-06-24-provider-priority-drag-reorder-design.md) (UI приоритетов)

## Цель

Убрать «двойную настройку» media-возможностей. Сейчас, чтобы возможность
заработала, нужно настроить **и** провайдера в реестре (`provider_active`),
**и** иметь парный YAML-инструмент в `workspace/tools/`, который этого
провайдера вызывает. Эти два слоя не связаны — двойная точка отказа и лишняя
ручная работа.

После изменения: **активной capability в реестре достаточно** — агент
автоматически получает встроенный инструмент. Единственная точка настройки —
реестр провайдеров. Дополнительно: описание инструмента, которое видит LLM,
динамическое (содержит имя текущего провайдера), и при сбое работает каскадный
failover по приоритету со «sticky»-памятью.

## Контекст (как сейчас)

- 6 capability активируются в `provider_active (capability, provider_name, priority)`
  (миграция 053 — multi-active с приоритетом уже есть): `stt, tts, vision,
  imagegen, embedding, websearch`.
- Из них 5 имеют парный YAML-инструмент, дающий агенту доступ:
  `imagegen→generate_image`, `tts→synthesize_speech`, `websearch→search_web`,
  `stt→transcribe_audio`, `vision→analyze_image`. У `embedding` инструмента нет
  (чисто системный).
- Различия API провайдеров **уже инкапсулированы в toolgate**: каждый
  провайдер реализует общий `Protocol` (`ImageGenProvider.generate(...)` и т.п.
  в `toolgate/providers/base.py`); эндпоинт toolgate (`/generate-image`,
  `/v1/audio/speech`, `/v1/search`, `/transcribe-url`, `/describe-url`)
  стабилен и не меняется при смене провайдера. Поэтому инструмент со
  стабильным контрактом безопасен.
- toolgate уже умеет маршрутизировать вызов к конкретному провайдеру по
  заголовку `X-Opex-Provider` (см. `require_provider` в `dependencies.py`).
- Однако: toolgate использует только топ-1 провайдера (`aget_active` → `LIMIT
  1`), а **failover на сбое не реализован нигде** — ни в toolgate, ни в core.

## Решения (из brainstorming)

1. **Zero-config:** активная capability сама даёт агенту инструмент. YAML под
   эти 5 имён не нужен. Нет провайдера → инструмента в списке агента нет.
2. **Без YAML-override / без обратной совместимости.** Жёсткий cutover (в стиле
   hydeclaw→opex). Вариативность API провайдеров живёт в toolgate, в инструменте
   переопределять нечего.
3. **Подход A (core-native):** встроенные capability-инструменты — новый слой в
   core. Исполнение переиспользует движок `YamlToolDef` (он уже умеет
   `channel_action`, binary, `injected_headers`), но описание, каскад и
   sticky-память — новый тонкий слой поверх.
4. **Динамическое описание:** в описание инструмента подставляется имя текущего
   здорового провайдера; пересобирается каждый раунд, поэтому после пометки
   провайдера «битым» следующий вызов показывает нового.
5. **Failover = каскад + sticky-память.** Вызов идёт по приоритету сверху вниз,
   пока один провайдер не отработает (агент не теряет вызов). Упавший
   помечается «битым» на TTL; пока битый — текущим (и в описании) считается
   следующий. По истечении TTL топ снова пробуется.
6. **Per-agent override** (`imagegen_provider`/`tts_provider`) = голова списка
   приоритета, далее обычный каскад.

## Non-goals (YAGNI)

- `embedding` остаётся без agent-facing инструмента (системный путь — память).
- Авто-триггеры STT/Vision на входящее медиа в channel-пайплайне не меняются
  (они идут не через инструмент).
- Произвольные HTTP-инструменты (`github_*`, `get_weather`, …) остаются в YAML
  без изменений.
- Персистентность sticky-памяти между рестартами не нужна (in-memory; при
  рестарте топ снова пробуется).
- Health-check провайдеров (активная проверка) — не делаем; восстановление
  только по TTL.

## Обзор архитектуры

```text
LLM ← список инструментов (пересобирается каждый раунд)
        ▲  capability_tool_defs(state, agent): для каждой capability с ≥1
        │  здоровым провайдером → ToolDefinition, description += "(current
        │  provider: <label>)"
        │
LLM → вызов инструмента (например generate_image)
        ▼
CapabilityDispatch (новый путь в core)
   order = CapabilityRouter.order(capability, agent_override)   // приоритет − битые
   for provider in order:
       result = exec via YamlToolDef-движок, header X-Opex-Provider: <provider>
       if failover-сбой:  CapabilityRouter.mark_broken(capability, provider); continue
       else: return result            // binary/channel_action → media_background
   // все битые → ошибка агенту
        ▼
toolgate /<endpoint>  (require_provider доверяет X-Opex-Provider)
        ▼
provider.generate/search/transcribe/...   (адаптация под конкретный API)
```

## Компоненты

### 1. `CapabilityRouter` (в `AppState`)
Владеет sticky-состоянием и резолвит провайдеров.

- Состояние: in-memory `DashMap<capability, DashMap<provider, broken_until: Instant>>`.
- `order(capability, agent_override) -> Vec<ProviderPick>` — из
  `db::providers::get_active_providers(capability)` по приоритету; провайдеры с
  непросроченным `broken_until` исключаются (или уходят в хвост как
  last-resort); `agent_override`, если задан, ставится головой.
- `current(capability, agent_override) -> Option<Label>` — первый элемент
  `order(...)`, для описания.
- `mark_broken(capability, provider)` — `broken_until = now + TTL`.
- Лениво чистит просроченные записи при чтении.

### 2. Статический реестр спецификаций capability-инструментов
Единый источник в Rust: `capability → CapabilityToolSpec { tool_name,
base_description, toolgate_path, params (JSON Schema), channel_action?,
binary? }`. Переносит в код то, что сейчас в 5 YAML:

| capability | tool_name | path | params | channel_action |
|---|---|---|---|---|
| imagegen | generate_image | /generate-image | prompt, size, quality | send_photo (binary) |
| tts | synthesize_speech | /v1/audio/speech | text(+voice) | send_voice (binary) |
| websearch | search_web | /v1/search | query, max_results | — |
| stt | transcribe_audio | /transcribe-url | url(+lang) | — |
| vision | analyze_image | /api/vision/analyze (core-проксер) | url, prompt | — |

(Точные параметры берутся 1:1 из текущих YAML на этапе реализации.)

**Особый случай vision.** `analyze_image` исполняется не напрямую через
toolgate, а через core-эндпоинт `/api/vision/analyze`, который умеет скачивать
собственные `/uploads/` (их SSRF-гард toolgate блокирует — нет схемы) и затем
проксирует в toolgate `/describe`/`/describe-url`. Для каскада/выбора провайдера
`CapabilityDispatch` передаёт выбранного провайдера этому core-эндпоинту, а
`media.rs::api_vision_analyze` пробрасывает `X-Opex-Provider` дальше в toolgate
(сейчас не пробрасывает — добавить passthrough). Семантика failover та же.

**`search_web`: убрать LLM-параметр `provider`.** Текущий YAML имеет
опциональный `provider`-параметр (LLM сам выбирал провайдера — модель из
websearch-спеки). В новой авто-модели выбор провайдера определяется
`CapabilityRouter` (приоритет + failover + per-agent override), а текущий
провайдер виден в описании. Поэтому LLM-параметр `provider` удаляется: одна
понятная модель выбора вместо двух.

### 3. Фабрика описаний `capability_tool_defs(state, agent)`
- Для каждой capability с непустым `order(...)` строит `ToolDefinition`:
  `description = base_description + " (current provider: <label>)"`. `label` —
  человекочитаемый из `media-drivers.yaml`, иначе `provider_name`.
- Подключается во все точки сборки tool-list, заменяя YAML capability-инструменты:
  `context_builder.rs`, `engine/context_builder.rs`, `dispatcher/lookup.rs`,
  `pipeline/openai_compat.rs`, `pipeline/subagent_runner.rs`.
- НЕ кладётся в per-session кэш YAML-тулзов (`dispatcher/state.rs`) — собирается
  заново каждый раунд (in-memory, дёшево), чтобы смена провайдера отражалась.

### 4. `CapabilityDispatch` (исполнение с каскадом)
- Срабатывает, когда имя вызванного инструмента принадлежит capability-реестру.
- Берёт `order(...)`, идёт сверху вниз; на каждом шаге конструирует `YamlToolDef`
  в рантайме из `CapabilityToolSpec` и исполняет существующим движком, добавляя
  `X-Opex-Provider: <provider>` в `injected_headers`; binary/`channel_action`
  — через существующий `media_background`.
- При failover-сбое: `mark_broken` + следующий провайдер. Успех → возврат. Все
  битые → ошибка агенту.

## Семантика failover

**Считается сбоем (→ failover к следующему):** таймаут, отказ соединения, HTTP
5xx (toolgate отдаёт 502 при падении провайдера), 429, 401/403 (провайдер плохо
сконфигурирован — у следующего ключ может быть валидным).

**НЕ считается сбоем (→ вернуть ошибку агенту сразу):** 400/422/404 — ошибка
запроса/промпта, одинаково упадёт у всех провайдеров; каскад бессмысленен.

**Sticky/восстановление:** упавший провайдер исключён из `order` пока
`now < broken_until`. По истечении TTL автоматически снова участвует (топ
пробуется первым). Состояние in-memory, при рестарте сбрасывается.

## Конфигурация и политика

- `[capability_tools] failover_ttl_secs` в `opex.toml`, дефолт **300**.
- **Tool policy:** встроенные уважают `deny`/`allow` по имени так же, как YAML
  (deny `generate_image` убирает инструмент из списка и запрещает вызов).
- **Per-agent override:** `imagegen_provider`/`tts_provider` = голова списка
  приоритета, далее обычный каскад; описание для агента показывает его override
  (если здоров).

## Миграция (hard cutover, без обратной совместимости)

1. Удалить из репозитория 5 YAML: `generate_image`, `synthesize_speech`,
   `search_web`, `transcribe_audio`, `analyze_image`.
2. Переключить системные обращения по имени на новый путь — прежде всего
   авто-TTS `synthesize_speech` в `engine/run.rs:248` (иначе сломается после
   удаления YAML). Проверить прочие обращения по этим именам.
3. Management-API YAML-тулзов (`verify`/`disable`) и вкладка «YAML tools» в UI:
   эти 5 больше не YAML — показывать как встроенные read-only либо скрыть из
   YAML-списка (никакой «настройки инструмента» в UI).
4. Деплой-заметка: удалить 5 файлов из живого воркспейса `~/opex/workspace/tools/`
   на сервере (встроенные имеют приоритет, но чистим для порядка).

## Тестирование (TDD — тесты вперёд)

**Unit:**
- `CapabilityRouter`: порядок по приоритету; фильтр битых по TTL; восстановление
  после TTL; `agent_override` как голова; `mark_broken`.
- Классификатор сбоев: 5xx/timeout/429/401/403 → failover; 400/422/404 → нет.
- Фабрика описаний: подстановка `label`; смена описания после `mark_broken`.
- Tool policy `deny` убирает capability-инструмент из списка.

**Integration (mock toolgate, wiremock — как в `media_background`):**
- Каскад «топ 502 → следующий 200»: возврат успеха, топ помечен битым.
- Все провайдеры битые → ошибка агенту.
- `X-Opex-Provider` реально уходит в toolgate с нужным значением на каждом шаге.
- binary/`channel_action` путь: `generate_image → send_photo`/`image_ready`.

**Regression:**
- Существующие тесты `media_background` (image_ready / send_photo / voice)
  проходят через новый путь.

## Open questions / future work

- Метрика/лог переключений провайдера (для наблюдаемости) — желательно, но вне
  scope.
- Возможный «last-resort»: пробовать битого провайдера, если ВСЕ битые (сейчас
  — ошибка). Решение по умолчанию: ошибка; пересмотреть при необходимости.
