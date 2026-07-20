# Owner from Approved Users (multi-owner) — Plan

**Дата:** 2026-07-17
**Статус:** proposed
**Триггер:** UX-претензия владельца — поле `owner_id` требует руками вписывать числовой
Telegram-ID, хотя система уже знает этого человека в списке «Авторизованные пользователи».
Предложение: назначать владельца **из апрувнутых юзеров**, с возможностью **нескольких**
владельцев.

## 1. Почему это нужно

Сейчас `owner_id` (`[agent.access] owner_id`, [config/mod.rs:1267](../../crates/opex-core/src/config/mod.rs#L1267))
— один строковый ID, вводимый вручную. Но `owner_id` — это НЕ дубликат апрув-списка, у него
несколько ролей, которых нет у обычного allowed-user:

- **Bypass доступа** — владелец допущен без pairing ([channels/access.rs:141](../../crates/opex-core/src/channels/access.rs#L141)).
- **Одобрение tool-approval в канале** — только владелец жмёт `approve:/reject:`
  ([inline.rs:192](../../crates/opex-core/src/gateway/handlers/channel_ws/inline.rs#L192)).
- **Owner-only команды в канале** ([inline.rs:310/465/583/689](../../crates/opex-core/src/gateway/handlers/channel_ws/inline.rs#L310)).
- **Адрес проактивной доставки** — `owner_id` парсится как Telegram `chat_id` для инициативы,
  дневного плана, результатов целей ([initiative/delivery.rs:12](../../crates/opex-core/src/agent/initiative/delivery.rs#L12))
  и `announce_to` heartbeat ([config/mod.rs:1278](../../crates/opex-core/src/config/mod.rs#L1278)).
- **Гейт инициативы** — при `owner_id.is_none()` инициатива вообще выключена
  ([tick.rs:71](../../crates/opex-core/src/agent/initiative/tick.rs#L71), [day_plan.rs:94](../../crates/opex-core/src/agent/initiative/day_plan.rs#L94)).

Идея фичи: сделать «владелец» **ролью на апрувнутом пользователе** (флаг), а не отдельным
свободным полем. Тогда владельцев может быть несколько, и назначаются они в один клик из
уже знакомого списка.

## 2. Ключевая развилка: право владельца ≠ адрес доставки

`resolve_owner_target` доставляет на ОДИН `chat_id`. При нескольких владельцах надо развести
две вещи:

- **Authority (право)** — «кто владелец»: bypass доступа, одобрение тулов, owner-команды.
  Это множество — может быть несколько владельцев.
- **Delivery (адрес)** — куда слать проактив (инициатива, announce). Нужен один (или явный
  broadcast).

**Решение v1 (рекомендуется):**
- Authority → множество владельцев (config `owner_id` + флаги в апрув-списке).
- Delivery → **primary owner**: `config.owner_id` если задан, иначе самый ранний
  апрувнутый владелец (его `channel_user_id` = Telegram `chat_id` для DM). Семантика
  одной доставки сохраняется, `resolve_owner_target` меняется минимально.
- **v2 (опционально):** broadcast инициативы всем владельцам на канале. Вынесено за скоуп v1.

## 3. Дизайн

### 3.1. Схема

Миграция: добавить в `channel_allowed_users` колонку
`is_owner BOOLEAN NOT NULL DEFAULT false`. Дата-миграция не нужна — `config.owner_id`
продолжает честь как неявный владелец (backward-compat).

### 3.2. AccessGuard — множественный владелец

- Сохранить синхронный `is_owner(user)` как проверку **config-владельца** (bootstrap):
  `config.owner_id == user`.
- Добавить `async fn is_owner(user)` = config-владелец **ИЛИ** строка апрув-списка с
  `is_owner=true` (запрос как у существующего `is_user_allowed`,
  [db/access.rs:6](../../crates/opex-core/src/db/access.rs#L6)).
- `is_allowed` уже async и ходит в БД — добавить owner-ветку туда же.
- Переключить call-sites `guard.is_owner(...)` на async-версию (все они в async-контексте):
  [inline.rs:192/310/465/583/689](../../crates/opex-core/src/gateway/handlers/channel_ws/inline.rs#L192).

Альтернатива (если async в hot-path нежелателен): держать `HashSet<String>` владельцев в
guard, перестраивать при promote/demote и на apгейте агента (guards уже пересобираются в
`access_guards`). Выбор — при реализации; async-запрос проще и консистентен с `is_allowed`.

### 3.3. Delivery — primary owner

`resolve_owner_target(db, agent, owner_id)` → добавить фолбэк: если `owner_id` (config) пуст,
взять `channel_user_id` самого раннего `is_owner=true` из `channel_allowed_users`. Гейты
инициативы (`owner_id.is_none()`) заменить на «есть хотя бы один владелец» (config или флаг).

### 3.4. API

- `GET /api/agents/{name}/access/users` → в DTO каждого юзера добавить `is_owner`.
- `PATCH /api/agents/{name}/access/users/{user_id}` `{ "is_owner": bool }` — promote/demote.
  ([handlers/access.rs](../../crates/opex-core/src/gateway/handlers/access.rs) — рядом с approve).
- Валидация: demote последнего владельца в `restricted` без `config.owner_id` — разрешить,
  но вернуть warning (потеряется in-channel approve + проактив-доставка).
- `config.owner_id` НЕ трогаем на PUT (сохраняется, как и base/soul — существующий паттерн).

### 3.5. UI

- В карточке доступа (скрин из обсуждения) на строке авторизованного пользователя —
  тумблер/звезда «Владелец». Клик → PATCH.
- Свободное поле `owner_id` понизить до «Дополнительно» (bootstrap для случая, когда ещё
  никто не апрувнут) или скрыть за раскрывашкой. Не удалять — нужно для первичного захода,
  пока список пуст.
- Показывать бейдж «Владелец» на строках с `is_owner=true`.

## 4. Backward-compat

- Существующие агенты с `config.owner_id` работают без изменений — он остаётся неявным
  владельцем и primary-адресом доставки.
- Новая колонка `is_owner` дефолтит в false — существующие апрувы не затрагиваются.
- Ни один API-контракт не ломается (только добавляется поле `is_owner` и новый PATCH).

## 5. Порядок работ

1. Миграция `is_owner` в `channel_allowed_users` + `db/access.rs`: promote/demote/list с флагом.
2. AccessGuard: async `is_owner` (config ∪ флаги) + переключение call-sites.
3. `resolve_owner_target` + гейты инициативы: primary-owner фолбэк.
4. API: PATCH promote/demote + `is_owner` в DTO списка.
5. UI: тумблер «Владелец» на строке юзера, понижение поля owner_id, бейдж.
6. Тесты (§6).

## 6. Тесты

- `is_owner` истинен и для config-владельца, и для флага в апрув-списке; несколько владельцев.
- promote/demote меняют право доступа и одобрения тулов (не-владелец не резолвит approval).
- `resolve_owner_target`: primary = config.owner_id, при пустом — ранний флаг-владелец;
  инициатива не выключается, если владелец задан только флагом.
- backward-compat: агент со старым config.owner_id и без строк `is_owner` ведёт себя как раньше.
- demote последнего владельца → warning, но не 500.

## 7. Скоуп / отложено

- **v2:** broadcast проактива всем владельцам (сейчас — только primary).
- Мульти-канальность владельца (владелец на нескольких каналах одновременно) — вне скоупа.
