# Kimi Code как первоклассный провайдер OPEX

**Дата:** 2026-07-16
**Статус:** дизайн утверждён, готов к плану реализации
**Подход:** Anthropic-совместимый пресет поверх существующего `AnthropicProvider` + Bearer-авторизация

## Цель

Добавить Kimi Code (Moonshot AI, тариф kimi.com/code) как **первоклассный пресет** в списке
провайдеров OPEX — с корректным base_url, форматом и авторизацией — чтобы агент мог выбрать его
из выпадашки без ручной настройки base_url/типа.

## Мотивация и контекст

Kimi Code продаётся против **Anthropic-совместимого эндпоинта** `https://api.moonshot.ai/anthropic`,
на котором крутятся модели `kimi-k3` (флагман, 1M-контекст), `kimi-k2.7-code`, `kimi-k2.6`.
Официальная дока конфигурирует его через Claude Code:

```json
{
  "env": {
    "ANTHROPIC_BASE_URL": "https://api.moonshot.ai/anthropic",
    "ANTHROPIC_AUTH_TOKEN": "YOUR_MOONSHOT_API_KEY",
    "ANTHROPIC_MODEL": "kimi-k3"
  }
}
```

`AnthropicProvider` в OPEX уже строит `POST {base_url}/v1/messages` и полностью совместим по телу
запроса (endpoint принимает `model, messages, system, tools, tool_choice, max_tokens, temperature,
stream`). Единственный барьер — авторизация.

### Ключевой факт: только Bearer

Документация Kimi и все реальные интеграции используют `Authorization: Bearer <moonshot_api_key>`.
`x-api-key` (нативный заголовок Anthropic, который OPEX шлёт сейчас) **не документирован и не
поддерживается**. Авторитетный первоисточник — GitHub-issue MoonshotAI/Kimi-K2 #129:

> «Authorization: Bearer <moonshot_api_key> is accepted (claude-code-router's anthropic transformer
> uses `UseBearer: true` against this base URL).»

То есть распространённый интегратор claude-code-router специально переключает транспорт в Bearer.
Значит, поддержка Bearer в `AnthropicProvider` — **обязательное** условие, а не опциональное.

### Что вне scope

- **OAuth device-flow** (`auth.kimi.com`) — механизм подписки kimi.com/code, аналог
  `gemini-cloudcode` OAuth. Отдельная крупная фича. Берём документированный путь **API-ключ + Bearer**.
- **Полный новый `LlmProvider`-impl** — не нужен: wire-формат Kimi Code = чистый Anthropic Messages API.
- **OpenAI-совместимый путь** (`api.moonshot.ai/v1`) — отклонён: близнец существующего `moonshot`-пресета
  и есть риск, что coding-подписка гейтит ключ на `/anthropic`-шлюз.

## Архитектура изменений

Все правки — в слое провайдеров `crates/opex-core/src/agent/providers/`. UI и БД-схема не трогаются.

### 1. Новый пресет в `PROVIDER_TYPES` (`registry.rs`)

Добавить элемент в статический массив `PROVIDER_TYPES`:

```rust
ProviderTypeMeta {
    id: "kimi",
    name: "Kimi Code (Moonshot)",
    chat_path: "",                                  // как anthropic — URL строит impl из base_url
    default_base_url: "https://api.moonshot.ai/anthropic",
    default_secret_name: "MOONSHOT_API_KEY",        // тот же ключ, что и у пресета moonshot
    requires_api_key: true,
    supports_model_listing: false,                  // у /anthropic-шлюза нет /v1/models
    models_provider: None,
    default_models: &["kimi-k3", "kimi-k2.7-code", "kimi-k2.6"],
}
```

Обоснование полей:

- `chat_path: ""` — пустой путь означает «impl сам строит URL» (та же семантика, что у `anthropic`);
  `AnthropicProvider::chat` делает `format!("{base_url}/v1/messages")`, давая `.../anthropic/v1/messages`.
- `supports_model_listing: false` — на `/anthropic`-шлюзе нет OpenAI `/v1/models`; полагаемся на
  `default_models` + рантайм-хинт `context_limit_hint`.
- `default_secret_name` переиспользует `MOONSHOT_API_KEY` — один аккаунт Moonshot покрывает оба пресета.

`id` в `PROVIDER_TYPES` = значение `provider_type` в строке БД → именно по нему диспетчеризует
`build_provider`.

### 2. Роутинг в `factory::build_provider` (`factory.rs`)

В `match row.provider_type.as_str()` расширить anthropic-плечо:

```rust
"anthropic" | "kimi" => {
    let provider = AnthropicProvider::new_from_row(
        row, secrets, *timeouts, cancel, opts, overrides,
    )?;
    Ok(Box::new(provider))
}
```

Один сайт. `resolve_provider_from_row` не трогаем — его `_`-плечо делегирует в `build_provider`,
который и выполняет диспетч.

### 3. Bearer-авторизация в `AnthropicProvider` (`anthropic/mod.rs`)

Добавить поле в структуру:

```rust
bearer_auth: bool,
```

В `new_from_row` выставлять:

```rust
bearer_auth: row.provider_type != "anthropic",
```

Логика: нативный Anthropic (`provider_type == "anthropic"`) → `x-api-key`; любой сторонний
anthropic-совместимый вендор (сейчас — `kimi`, в будущем — другие) → `Authorization: Bearer`.
Правило future-proof и не требует новых полей на каждый вендор.

Вынести формирование заголовков в приватный хелпер, устраняя дублирование:

```rust
/// Заголовки авторизации + версии для Anthropic-совместимого запроса.
/// `x-api-key` для нативного Anthropic, `Authorization: Bearer` для сторонних
/// anthropic-совместимых вендоров (Kimi Code и т.п.).
fn auth_headers(&self, api_key: Option<&str>) -> Vec<(String, String)> {
    let mut h = vec![("anthropic-version".to_string(), "2023-06-01".to_string())];
    if let Some(key) = api_key.filter(|k| !k.is_empty()) {
        let (name, value) = if self.bearer_auth {
            ("authorization".to_string(), format!("Bearer {key}"))
        } else {
            ("x-api-key".to_string(), key.to_string())
        };
        h.push((name, value));
    }
    h
}
```

Заменить хелпером три захардкоженных сайта `x-api-key`:

- `chat` (сейчас `anthropic/mod.rs:183-187`)
- `chat_stream` (сейчас `anthropic/mod.rs:250-254`)
- `context_limit_hint` (сейчас `anthropic/mod.rs:428-431`) — здесь заголовки ставятся через
  `reqwest::RequestBuilder`, а не `Vec`; адаптировать под тот же выбор `authorization`/`x-api-key`.
  Сам вызов `/v1/models/{model}` на `/anthropic`-шлюзе, скорее всего, вернёт ошибку — это штатно:
  метод фейлится мягко (`.ok()?` → `None`) и хинт контекста просто не подхватывается.

### 4. prompt_cache — оставить default-off

Anthropic `cache_control`-блоки и заголовок `anthropic-beta` против шлюза Moonshot не проверены.
Не форсим кэш: агентский дефолт `prompt_cache = false` и `ProviderOptions.prompt_cache = false`
уже дают выключенное состояние. Никаких спец-действий не требуется — просто не включаем для пресета.

## Поток данных (happy path)

1. Пользователь в UI выбирает тип провайдера **Kimi Code (Moonshot)** (появляется автоматически из
   `/api/providers/types`), вводит Moonshot API-ключ, выбирает модель (напр. `kimi-k3`).
2. Строка `providers` пишется с `provider_type = "kimi"`, `base_url = https://api.moonshot.ai/anthropic`,
   ключ — в вольте под скоупом UUID строки.
3. Агент через профиль (`text`-слот) резолвит провайдер → `resolve_provider_for_agent` →
   `build_provider` → плечо `"kimi"` → `AnthropicProvider` с `bearer_auth = true`.
4. Запрос: `POST https://api.moonshot.ai/anthropic/v1/messages` с заголовками
   `anthropic-version: 2023-06-01` + `Authorization: Bearer <key>`.

## Обработка ошибок

- Нет ключа → `auth_headers` не добавляет авторизацию → шлюз вернёт 401 → штатный error-путь провайдера.
- `context_limit_hint` на несуществующем `/v1/models` → мягкий `None`, без падения.
- Невалидный тип модели → шлюз вернёт ошибку в теле → штатный разбор ответа Anthropic.

## Тестирование

Unit-тесты (не требуют сети/БД, идут в bin-таргете opex-core):

1. `resolve_chat_url("kimi", "https://api.moonshot.ai/anthropic")` →
   `"https://api.moonshot.ai/anthropic"` (пустой `chat_path` возвращает base как есть); а связка с
   `AnthropicProvider` даёт `.../anthropic/v1/messages` — покрыть проверкой построения URL в impl.
2. `auth_headers` при `bearer_auth = true` содержит `("authorization", "Bearer k")` и НЕ содержит
   `x-api-key`; при `bearer_auth = false` — наоборот.
3. `PROVIDER_TYPES` содержит запись `id == "kimi"` с ожидаемыми base_url и `default_secret_name`.
4. `build_provider` для строки с `provider_type = "kimi"` возвращает провайдер с `name() == "anthropic"`
   (роутинг в anthropic-плечо), а не уходит в `OpenAiCompatibleProvider`.

Ручная проверка на сервере (реальным ключом): один чат-запрос через пресет Kimi Code, убедиться в
200 и корректном стриме tool-calls.

## Смежные сайты, проверенные и намеренно не тронутые

Ревью прошёлся по всем местам, где код спец-кейсит `provider_type`. Ниже — что проверено и почему
правок не требует:

- **`gateway/handlers/agents/lifecycle.rs:105`** (сборка провайдера компакции) — матчит только
  CLI-типы (`claude-cli | gemini-cli | codex-cli`), всё остальное уходит в `build_provider`. `"kimi"`
  идёт по тому же пути, что и в основном резолве. Правок нет.
- **`agent/model_discovery.rs` (`discover_models`, строки ~239 и ~311)** — спец-кейсит
  `"anthropic" | "claude-cli"` для листинга через `fetch_anthropic_models`. `"kimi"` **намеренно**
  оставлен в дефолтной `other`-ветке (OpenAI-совместимый листинг). Это безопасно, потому что у пресета
  `supports_model_listing: false`: UI листинг не предлагает, выпадашка моделей питается из
  `default_models`. **Важно для будущего:** если когда-нибудь включат `supports_model_listing: true`,
  листинг поедет по неверной ветке — тогда нужно добавить `"kimi"` в anthropic-плечи `discover_models`
  И научить `fetch_anthropic_models` Bearer-режиму (сейчас он шлёт `x-api-key`). В рамках этого MVP
  листинг выключен — трогать `model_discovery` не нужно.
- **`gateway/handlers/providers.rs:679`** (`"driver": provider_type` для toolgate) — касается только
  медиа-провайдеров (tts/stt/embedding). Kimi — текстовый, в медиа-слоты не назначается. Не релевантно.
- **`opex-catalog`** — нет метаданных моделей `kimi-*` (контекст/цены). UI покажет обобщённые бейджи.
  Опциональный follow-up, вне scope MVP.

**Требование к конфигу (не код):** при создании подключения в UI тип провайдера (`type`) должен быть
`llm`/`text` — иначе `resolve_provider_for_agent` отбракует строку (`factory.rs:192`). Общее правило
для всех текстовых провайдеров, не специфично для Kimi.

## Границы и что НЕ делаем

- Не трогаем UI (пресет появляется динамически).
- Не трогаем БД-миграции.
- Не добавляем OpenAI-совместимый `.ai`-пресет (YAGNI).
- Не реализуем OAuth device-flow подписки (отдельная фича).
- Метаданные моделей `kimi-k3` в `opex-catalog` (контекст/цены) — опциональный follow-up, вне scope MVP.

## Затрагиваемые файлы

- `crates/opex-core/src/agent/providers/registry.rs` — +1 запись в `PROVIDER_TYPES`, +тест.
- `crates/opex-core/src/agent/providers/factory.rs` — +`"kimi"` в anthropic-плечо, +тест.
- `crates/opex-core/src/agent/providers/anthropic/mod.rs` — поле `bearer_auth`, хелпер `auth_headers`,
  замена 3 сайтов, +тесты.
