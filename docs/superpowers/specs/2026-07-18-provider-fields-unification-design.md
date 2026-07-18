# Унификация полей выбора провайдера / модели / голоса в UI

**Дата:** 2026-07-18
**Статус:** утверждено (brainstorming, подход A)
**Область:** только `ui/` — бэкенд не затрагивается.

## Проблема

Поля выбора модели (и рядом — провайдера и голоса TTS) реализованы в пяти местах
по-разному:

| Место | Как сейчас |
|---|---|
| Композер чата (`chat/composer/ModelDropdown.tsx`) + пикер «перегенерировать» | Select из агрегатора с бейджами — эталон |
| Профили (`profiles/_parts/ProfileEditor.tsx`) | свободный текстовый Input для text/compaction/vision |
| Роутинг-правила агента (`agents/RoutingRulesEditor.tsx`) | свободный Input; props `discoveredModels`/`fetchModels` прокинуты, но не используются; мёртвый хардкод `FALLBACK_MODELS` с устаревшими моделями |
| Провайдеры, текстовые (`providers/_parts/TextFields.tsx`) | Select появляется только после кнопки «discover», иначе Input |
| Провайдеры, медиа (`providers/_parts/MediaFields.tsx`) | свободный Input |
| Setup-визард (`setup/page.tsx`) | свой Select/Input + хардкод популярных моделей; pre-create discovery **сломан молча** (см. ниже) |

«Агрегатор» — `GET /api/providers/{id}/models`
(`api_unified_provider_models`): живой discovery у провайдера + обогащение из
каталога (`enrich_from_catalog`) + fallback-список для CLI-провайдеров. В UI —
хук `useProviderModelsDetailed` (React Query, ключ `["providers", id, "models"]`).

Попутная находка: setup-визард зовёт
`/api/providers/${providerType}/models?base_url=…`, но роут парсит `{id}` как
`Uuid` → строка типа `"openai"` даёт 400, и код тихо падает в хардкод-список.
Мёртвый вызов подлежит сносу.

## Решение

Семья из трёх переиспользуемых компонентов в
`ui/src/components/provider-fields/`, работающих поверх существующих
эндпоинтов. Новых зависимостей и изменений бэкенда нет.

### 1. `ProviderSelect`

Единый селект провайдера.

- Props: `value` (имя провайдера), `onChange`, `categories: string[]`
  (напр. `["text","llm"]` для text/compaction-слотов), `allowNone?` (пункт «—»
  для роутинга), `disabled?`, размерные пропсы по месту.
- Данные: `useProviders`, фильтр по `categories`.
- Вид везде одинаковый — как сейчас в роутинге: иконка + имя +
  `default_model` серым мелким шрифтом.

### 2. `ModelCombobox`

Единое поле модели: Input со свободным вводом + выпадающий список подсказок.

- Props: `value`, `onChange`, источник — **либо** `providerId: string | null`
  (uuid существующего провайдера), **либо** `staticOptions: string[]`
  (провайдер ещё не создан: setup-визард, create-формы провайдеров),
  `placeholder?`, `disabled?`.
- Механика: `Input` (font-mono) + кастомный absolute-список с aria-ролями
  combobox/listbox (в UI-ките нет cmdk/popover — не добавляем зависимостей).
- При `providerId`: ленивая загрузка через `useProviderModelsDetailed` при
  первом открытии; пункт списка = id модели + `ModelBadges`
  (контекст/капабилити).
- Ввод фильтрует список по вхождению подстроки без учёта регистра; значение
  вне списка валидно (нестандартные id).
- Свободный ввод разрешён всегда.

### 3. `VoiceSelect`

Голос TTS.

- Props: `value`, `onChange`, `providerName`.
- Новый хук `useTtsVoices(providerName)` (React Query, ключ
  `["tts-voices", provider]`) — заменяет ручной `fetchVoices` с seq-guard'ом и
  `voicesByProvider`-стейтом в ProfileEditor.
- Select из списка; при пустом списке или ошибке — деградация в свободный
  Input (сейчас при недоступном toolgate поле голоса нечем заполнить).

## Точки внедрения

### Профили — `profiles/_parts/ProfileEditor.tsx`

- Провайдер → `ProviderSelect` с `categoriesFor(cap)`.
- Модель (text/compaction/vision) → `ModelCombobox` с `providerId`, найденным
  по имени провайдера строки через `useProviders`.
- Голос → `VoiceSelect`; удалить `fetchVoices`, `voicesByProvider`,
  seq/unmount-refs.
- `data-testid` (`profile-slot-*`, `profile-row-*`, `profile-model-*`)
  сохранить.

### Роутинг-правила — `agents/RoutingRulesEditor.tsx`

- Провайдер → `ProviderSelect` с `allowNone`.
- Модель → `ModelCombobox`.
- Снести: `FALLBACK_MODELS`; props `discoveredModels`/`fetchModels` из
  редактора и всей цепочки прокидки (`AgentEditDialog` → `agents/page.tsx`);
  `PROVIDERS` — если проверка использований подтвердит, что больше нигде не
  нужна.

### Провайдеры, текстовые — `providers/_parts/TextFields.tsx`

- `default_model` → `ModelCombobox`: при редактировании — `providerId`
  (кнопка «discover» и хинт «сохраните, чтобы обнаружить модели» не нужны);
  при создании — `staticOptions` из выбранного пресета каталога
  (`CatalogProvider.models` — сейчас берётся только первая модель, остальной
  список пропадает).
- Вычистить `discoveredModels`/`modelsLoading`/`onDiscoverModels` из
  родительской `providers/page.tsx`.

### Провайдеры, медиа — `providers/_parts/MediaFields.tsx`

- `default_model` → `ModelCombobox` (редактирование — `providerId`;
  создание — без опций, фактически free input в едином виде).

### Setup-визард — `setup/page.tsx`

- Поле модели → `ModelCombobox` со `staticOptions` = существующие
  хардкод-подсказки по типу провайдера (`fallbackModels`).
- Снести сломанный pre-create fetch и связанный стейт. Post-create проверку
  ключа через `/api/providers/{created.id}/models` оставить — это валидация,
  не поле.

### Не трогаем

Композер (`ModelDropdown`) и пикер «перегенерировать» — компактные read-only
переключатели уже поверх агрегатора; свободный ввод там не нужен.

## Крайние случаи и поведение

- **Смена провайдера в строке** — поведение за местом использования:
  - профили: модель **очищается** (пустое поле = «default_model провайдера»
    по семантике `useAgentTextModel`; плейсхолдер так и говорит); голос
    очищается, как и сейчас;
  - роутинг: модель заполняется `default_model` нового провайдера (текущее
    поведение, модель там обязательна).
- **Провайдер не выбран** → `ModelCombobox` disabled с плейсхолдером
  «сначала выберите провайдера».
- **Листинг недоступен / discovery упал / список пуст** → combobox работает
  как обычный Input; в раскрытом списке одна строка-подсказка «список
  недоступен, введите id вручную». Без тостов — это штатный случай
  (`supports_model_listing=false`).
- **Загрузка** → строка со спиннером внутри выпадашки; из кэша React
  Query — мгновенно.
- **Сохранённое значение вне списка** → отображается как есть, без пометок.
- **Длинные списки** (сотни моделей у OpenRouter) → `max-h` со скроллом +
  фильтрация.
- **Пустой ввод при фильтрации** → полный список, а не «ничего не найдено».

## Тесты

Vitest + testing-library, запуск строго из `ui/`. Порядок — TDD: сначала
тесты компонентов, потом реализация, потом внедрение по точкам с прогоном
соответствующего теста после каждой.

**Новые** — `provider-fields/__tests__/`:

- `ModelCombobox`: открытие списка; ленивый фетч по `providerId` (мок
  `apiGet`); фильтрация; выбор пункта; свободный ввод вне списка; disabled
  без провайдера; пустой список → подсказка; режим `staticOptions` без
  сетевых вызовов.
- `ProviderSelect`: фильтрация по `categories` (в т.ч. `["text","llm"]`);
  пункт «—» при `allowNone`.
- `VoiceSelect`: список из `useTtsVoices`; деградация в Input при пустом
  списке/ошибке.

**Обновляемые:** `profile-editor.test.tsx`, `agent-form.test.tsx`,
`agent-tabs.test.tsx` (проверить импорты `FALLBACK_MODELS`/`PROVIDERS` перед
сносом), `providers-page.test.tsx`, `provider-form.test.ts`,
`setup-page.test.tsx`.

**Финальная проверка:** `npm run build` + полный `npm test` из `ui/`;
Rust-тесты не нужны (бэкенд не затронут).
