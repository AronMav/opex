# Спека №1: Сущность «Профиль» — провайдеры, модели и резервы вне агента

**Дата:** 2026-07-15
**Статус:** дизайн согласован (брейншторм 2026-07-14/15)
**Связанные документы:** [2026-07-15-voice-ux-redesign-design.md](2026-07-15-voice-ux-redesign-design.md) (спека №2, строится поверх этой)

## 1. Проблема

Настройки провайдеров размазаны по трём местам с разной семантикой:

1. **Агент** (`AgentSettings`, [config/mod.rs:929-959](../../../crates/opex-core/src/config/mod.rs)): `provider`+`model` (legacy), `provider_connection` (именованная LLM-запись), `fallback_provider` (один, только text), `tts_provider`, `imagegen_provider`. STT/vision/search у агента не настраиваются вообще.
2. **Глобальный маппинг** `provider_active` (capability → провайдер + priority): tts, stt, vision, imagegen, websearch, compaction, embedding. Секция «Активные провайдеры» на странице Провайдеры.
3. **Записи провайдеров** (`providers` table): голос TTS зашит в `options.voice` записи — разные голоса требуют дублирования записей.

Следствия: web-озвучка игнорирует пер-агентный `tts_provider` (ходит через глобальный active), резервирование есть только у text (и только один резерв), появление нового типа настройки требует нового поля в агенте, «активные» конфликтуют с пер-агентными override'ами.

## 2. Решение

Новая сущность **Профиль**: именованный набор провайдеров, моделей, голоса и резервных цепочек для всех агентских capability. Агент ссылается на профиль одним полем. Секция «Активные провайдеры» упраздняется (кроме embedding). Профили можно копировать.

```text
Агент ──profile──▶ Профиль ──slots──▶ записи providers (по именам)
                     │
                     ├─ text:      [{provider, model?}, {provider, model?}, …]   ← primary + резервы
                     ├─ compaction:[{provider, model?}, …]                        ← пусто → цепочка text
                     ├─ stt:       [{provider}, …]
                     ├─ tts:       [{provider, voice?}, …]                        ← voice пуст → options.voice записи
                     ├─ vision:    [{provider, model?}, …]
                     ├─ imagegen:  [{provider}, …]
                     └─ websearch: [{provider}, …]                                ← приоритетная цепочка поиска
```

**Не входит в профиль:** embedding — общий векторный индекс памяти, смена провайдера ломает размерность всей базы. Остаётся в `provider_active` как сейчас (решение пользователя: «эмбеддинг оставляем как есть, он общий»).

**Пустой слот = возможность выключена** для агентов этого профиля: не регистрируется capability-tool, `/api/tts/synthesize` отвечает 409, UI прячет соответствующие функции. Никаких неявных дефолтов (решение пользователя).

## 3. Модель данных

### 3.1 Таблица `profiles` (миграция m084)

```sql
CREATE TABLE profiles (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name TEXT NOT NULL UNIQUE,
    slots JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
```

`slots` — объект `{capability: [SlotEntry, …]}`. `SlotEntry`:

```jsonc
{ "provider": "minimax-t2a",   // имя записи providers (NOT NULL)
  "model": "MiniMax-Text-01",  // опционально: text/compaction/vision
  "voice": "Russian_Champ" }   // опционально: только tts
```

Порядок элементов массива = приоритет: `[0]` — основной, дальше резервы. Валидация при записи: известные capability-ключи (`text|compaction|stt|tts|vision|imagegen|websearch`), непустые имена провайдеров, существование записей providers соответствующей категории (text-слот принимает категории `text`/`llm`).

Rust-типы в `db/profiles.rs`: `ProfileRow`, `SlotEntry { provider: String, model: Option<String>, voice: Option<String> }`, `Slots = HashMap<String, Vec<SlotEntry>>`.

### 3.2 Агент

В `AgentSettings` добавляется:

```toml
[agent]
profile = "Default"   # имя профиля; отсутствует → "Default"
```

Поля `provider`, `model`, `provider_connection`, `fallback_provider`, `tts_provider`, `imagegen_provider` **удаляются из активного использования**:

- в структуре остаются на один релиз как `#[serde(default)] #[deprecated]` (толерантный парс старых TOML до прогона миграции), engine их не читает;
- PUT /api/agents их больше не принимает и не отдаёт (см. §5);
- startup-миграция (§6) физически переписывает TOML-файлы без этих полей.

`temperature` и `max_tokens` остаются у агента — это параметры сэмплинга, не выбор провайдера.

## 4. Резолюция

Единый модуль `agent/profile_resolver.rs`:

```rust
pub async fn resolve_chain(db, profile_name, capability) -> Vec<SlotEntry>;  // [] = выключено
pub async fn resolve_profile_for_agent(db, agent: &AgentSettings) -> ProfileRow; // agent.profile → "Default" fallback + warn
```

Если профиль, указанный у агента, не найден (удалён/опечатка) — используется `Default` c `tracing::warn!`; если нет и его — все слоты пустые (агент без LLM получает существующий `UnconfiguredProvider`-sentinel).

### 4.1 text (LLM-цикл)

- `resolve_provider_for_agent` ([providers/factory.rs:178](../../../crates/opex-core/src/agent/providers/factory.rs)) меняет источник: вместо `agent.provider_connection`/`agent.model` — `profile.slots.text[0]` (`provider` → строка providers, `model` → override поверх `default_model`).
- `FallbackPolicy` ([pipeline/behaviour.rs:109](../../../crates/opex-core/src/agent/pipeline/behaviour.rs)) обобщается с одного `fallback_provider` на **цепочку** `text[1..]`: при срабатывании порога (существующая семантика `is_failover_worthy` / `consecutive_failure_threshold`, включая своп на первой transport/Unknown-ошибке) движок переключается на следующий элемент цепочки; исчерпание цепочки = текущее поведение «fallback не задан». Свитч по-прежнему per-run: следующий run начинает с primary.

### 4.2 compaction

`lifecycle.rs:69-71` сейчас берёт `provider_active[compaction]`. Меняется на `profile.slots.compaction[0]` (с моделью), пустой слот → компакция идёт основным text-провайдером профиля (текущее поведение «falls back to primary» сохраняется).

### 4.3 Медиа (stt / tts / vision / imagegen)

Core всегда шлёт toolgate **явный** `X-Opex-Provider: <chain[i].provider>` (механизм уже существует: [dependencies.py:16-42](../../../toolgate/dependencies.py), [media_background.rs:208-226](../../../crates/opex-core/src/agent/pipeline/media_background.rs)). Новое:

- **Цепочка**: при сетевой ошибке / HTTP ≥ 500 / 503-degraded от toolgate core повторяет запрос со следующим элементом слота. 4xx (кроме 429) не ретраится — это ошибка запроса, не провайдера.
- **Голос TTS**: `chain[i].voice` инжектится в тело запроса `/v1/audio/speech` (поле `voice` OpenAI-compat) — и в `synthesize_speech` body_template ([capability_tools.rs:46-48](../../../crates/opex-core/src/agent/capability_tools.rs)), и в `/api/tts/synthesize`. Пустой `voice` → поле не передаётся, работает `options.voice` записи провайдера (аналогия «модель агента → иначе модель провайдера», решение пользователя).
- Точки инжекции header'а остаются прежними (`engine_dispatch` для YAML/capability-тулов, `media_background`, `channel_actions`), меняется источник значения: профиль вместо `agent.tts_provider`/`agent.imagegen_provider`.
- В `dependencies.py::require_provider` fallback-ветка «override неизвестен → aget_active» сохраняется на переходный период, но нормальный путь теперь всегда с header'ом.

### 4.4 websearch

- Toolgate `/v1/search` принимает новый заголовок `X-Opex-Providers: name1,name2,…` (упорядоченный список). Если задан — испробовать по порядку (существующая цепочко-логика поиска переиспользуется, меняется источник списка); если нет — legacy-путь `aget_active("websearch")` ([search.py:24](../../../toolgate/routers/search.py)) для вызовов без агентского контекста (ctx.search у file-handlers).
- **Изменение реестра toolgate:** `aget_active(capability)` при отсутствии active-строки (после миграции их нет ни для чего, кроме embedding) падает на «включённые записи соответствующей категории, отсортированные по имени». Это сохраняет работоспособность ctx.tts/ctx.stt/ctx.search у file-handlers без provider_active-строк; агентские пути этот фоллбек не используют (у них всегда явный header).
- Core инжектит `X-Opex-Providers` из `profile.slots.websearch` при вызове `search_web`.

### 4.5 Гейтинг capability-тулов

`capability_tool_defs` / `find_capability_tool` ([capability_tools.rs:111-135](../../../crates/opex-core/src/agent/capability_tools.rs)) получают параметр «слоты профиля агента» вместо запроса к `provider_active`: тул регистрируется, если соответствующий слот непуст; в описание подставляется `chain[0].provider`. Callers (`tool_defs.rs`, `engine_dispatch`) уже работают в контексте агента — профиль доступен.

### 4.6 Hot-reload

`PUT/DELETE /api/profiles/{id}` после записи в БД пересобирает engines всех агентов, ссылающихся на профиль, — тем же механизмом, каким PUT /api/agents применяет изменения агента (существующий reload-путь lifecycle). Config-watcher не участвует (профили живут в БД).

## 5. API

Новый handler `gateway/handlers/profiles.rs` (sub-router pattern):

| Метод | Путь | Действие |
| --- | --- | --- |
| GET | `/api/profiles` | список: id, name, slots, `agents: [имена]` (кто ссылается), timestamps |
| POST | `/api/profiles` | создать `{name, slots}` |
| GET | `/api/profiles/{id}` | одна запись |
| PUT | `/api/profiles/{id}` | обновить name/slots (+ hot-reload агентов) |
| POST | `/api/profiles/{id}/copy` | копия с именем `{name} (copy)`/`{name} (copy N)` |
| DELETE | `/api/profiles/{id}` | 409, если на профиль ссылается хотя бы один агент; `Default` неудаляем |

Изменения существующих API:

- **Agents** (`agents/{dto,dto_structs,schema,crud}.rs`): в DTO/schema добавляется `profile: String`; удаляются `provider`, `model`, `provider_connection`, `fallback_provider`, `tts_provider`, `imagegen_provider` (breaking — единственный потребитель UI в репо; фиксируется в `api.generated.ts`). `AgentInfo`/summary дополняется `capabilities: {text, stt, tts, vision, imagegen, websearch: bool}` — вычислено из слотов профиля, UI гейтит функции по нему.
- **provider-active** (`providers.rs:549-599`): PUT принимает только `embedding` (остальные capability → 400 с подсказкой «managed by profiles»); GET отдаёт только embedding-строки. Экспорт конфига для toolgate (`providers.rs:48`, `/api/providers` snapshot) продолжает отдавать embedding-active; медиа-active toolgate больше не нужен (явные header'ы), но snapshot сохраняет поле для ctx.*-fallback'а (см. Out of scope).
- **Удаление провайдера** (`providers.rs:414-430`): к существующей чистке `provider_active` добавляется проверка «имя используется в слотах профилей» → 409 со списком профилей (запрет вместо тихой чистки: слот с дырой = сломанная цепочка).

## 6. Миграция

Однократный startup-seed в core (код, не SQL — нужен доступ к TOML), по образцу first-run bootstrap'а памяти; идемпотентность через `system_flags['profiles_migrated']`:

1. Если флаг стоит — пропустить.
2. Создать профиль `Default` из `provider_active`: для каждой capability из {tts, stt, vision, imagegen, websearch, compaction} все строки по priority → слот-цепочка; text-слот — из настроек первого base-агента (или первого агента вообще).
3. Для каждого агента: если его (`provider_connection`|`model`|`fallback_provider`|`tts_provider`|`imagegen_provider`) дают конфигурацию, отличную от `Default`, — создать профиль `{AgentName}` (text: `[{provider_connection, model}, {fallback_provider}]`; tts/imagegen-override'ы поверх Default-слотов) и назначить его; иначе назначить `Default`.
4. Переписать TOML агентов: добавить `profile = "…"`, удалить шесть legacy-полей (через `toml_edit`, сохраняя остальное форматирование — паттерн `config/mod.rs:2264+`).
5. Удалить из `provider_active` все строки, кроме `capability='embedding'`.
6. Поставить флаг. Ошибки шагов — fail-loud в лог, флаг не ставится (повтор на следующем старте).

Setup wizard: шаг провайдера дополнительно создаёт/наполняет `Default` (text-слот из созданной записи), первый агент получает `profile = "Default"` вместо `provider_connection`.

## 7. UI

- **Новая вкладка «Профили»** в группе СИСТЕМА, **над «Провайдеры»** ([app-sidebar.tsx:77](../../../ui/src/components/app-sidebar.tsx) — вставка перед `nav.providers`; роут `/profiles/`).
- **Страница списка**: карточки (имя, сводка слотов «text: ollama/kimi-k2.6 +1 резерв · tts: minimax (Russian_Champ) · …», бейджи агентов), кнопки создать / копировать / удалить (disabled с тултипом, если используется).
- **Редактор профиля**: секция на каждый слот — селект провайдера (записи соответствующей категории), поле модели с каталогом (переиспользуется механика диалога агента) для text/compaction/vision, селект голоса для tts (`GET /api/tts/voices?provider=`), «+ резервный провайдер» → упорядоченный список со стрелками вверх/вниз и удалением.
- **Страница Провайдеры**: секция «Активные провайдеры» заменяется компактным блоком «Embedding» (единственная оставшаяся capability); CRUD записей не меняется.
- **Диалог агента**: блок provider/model/fallback/TTS/imagegen заменяется селектом «Профиль» + ссылка «Открыть профили». `useCommands`-style hook `useProfiles` (React Query, ключи `["profiles"]`, `["profiles", id]`).
- Локализация ru/en для всех новых строк.

## 8. Инварианты и краевые случаи

- `Default` всегда существует после миграции/wizard'а; неудаляем и непереименовываем (кнопки disabled, API 409/400).
- Профиль удалён вручную из БД, агент ссылается → резолвер падает на `Default` + warn (не паника).
- Провайдер, на который ссылается слот, нельзя удалить (409, §5) — но `enabled=false` записи допустимы: резолюция пропускает выключенные записи цепочки (как «резерв на паузе»), пустая эффективная цепочка = слот выключен.
- Copy даёт глубокую копию slots; имя уникализируется суффиксом.
- Конкурентные PUT профиля: last-write-wins (как у остальных CRUD в проекте).
- Embedding не трогается ни миграцией (кроме сохранения его строк), ни UI-удалением секции.

## 9. Тесты

- **db/profiles.rs**: CRUD, copy-нейминг, валидация слотов (неизвестная capability, несуществующий провайдер, категория-mismatch), 409 на delete-in-use (`#[sqlx::test]`).
- **profile_resolver**: agent→profile→chain; отсутствующий профиль → Default + warn; пустые слоты; enabled=false пропускается.
- **factory/behaviour**: text-цепочка — primary из слота, свитч на резерв №1, затем №2, исчерпание; compaction из слота / fallback на text.
- **media retry**: 503 от toolgate → следующий провайдер цепочки; 400 — без ретрая.
- **capability_tools**: тул регистрируется/пропадает по слоту профиля (переписать существующие `#[sqlx::test]` с `set_provider_active_list` на слоты).
- **Миграция**: seed из provider_active + агентских полей, идемпотентность, TOML переписан без legacy-полей.
- **toolgate**: `X-Opex-Providers` порядок/фоллбек (pytest); voice в теле `/v1/audio/speech` уважается.
- **UI (vitest)**: редактор слотов (добавление резерва, порядок, голос), копирование, селектор профиля в диалоге агента, гейтинг по `capabilities`.

## 10. Вне объёма (зафиксировано)

- `ctx.tts`/`ctx.stt`/`ctx.search` внутри toolgate file-handlers остаются на внутренней резолюции реестра (highest-priority enabled запись категории) — у handler-jobs нет обязательного агентского контекста. Перевод ctx.* на профили — отдельная задача.
- Пер-агентные override'ы поверх профиля (гибрид) — сознательно нет: модель «агент → профиль» чистая, копирование профилей закрывает кастомизацию.
- Апстрим-фикс `(Empty response:` и голосовой UX — спека №2.
