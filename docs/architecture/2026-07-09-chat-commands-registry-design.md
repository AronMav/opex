# Единый реестр команд чата + file-handlers-as-commands

**Дата:** 2026-07-09
**Статус:** дизайн одобрен, ревью учтено (F1–F10), готов к плану
**Референсы:** OpenClaw `src/auto-reply/commands-registry.*` (декларативный реестр), Hermes `ui-tui/src/app/slash/commands/*` (интерактивные пикеры)

## Проблема

Две связанные потребности:

1. **File handlers должны запускаться как команды чата**, а не только через меню файла/ссылки. Обработчик (`summarize_video`, `transcribe`, …) — это «опция», доступная и как меню-affordance, и как команда `/summarize_video <url>`.
2. **Команды чата в OPEX разрозненны.** Сейчас это жёстко зашитый `match` в
   [commands.rs](../../crates/opex-core/src/agent/pipeline/commands.rs) на ~14 команд
   (`/status`, `/new`, `/reset`, `/compact`, `/rollback`, `/model`, `/think`, `/voice`,
   `/usage`, `/export`, `/help`, `/memory`, `/goal`, `/subgoal`). Нет реестра,
   автодополнения, нативных меню каналов, аргументов с выбором.

Обе потребности сходятся в одну: **единый декларативный реестр команд**, где file
handlers — один из источников команд (наряду со встроенными), а меню аргументов
переиспользует уже существующую инфраструктуру кнопок (`handler_menu` / Telegram inline).

## Что взято из референсов

**OpenClaw (эталон).** Декларативный `ChatCommandDefinition { key, nativeName,
description, textAliases[], scope: text|native|both, category, args[], argsParsing,
argsMenu, formatArgs }`. Аргументы типизированы, с `choices` (статичные **или
функция-провайдер** — динамические), `captureRemaining`, автодополнением. `argsMenu:
"auto"` → интерактивное меню-выбор кнопками. `nativeName` → регистрация в нативном меню
Telegram/Discord. Валидация реестра (`assertCommandRegistry`: нет дублей, консистентность
scope). Плагины добавляют свои команды.

**Hermes.** TUI-команды с оверлеями/пикерами (модель-picker, session-picker), алиасы,
gateway RPC. Заточен под TUI; для канало-ориентированного OPEX реестр OpenClaw подходит
лучше. Берём идею интерактивных пикеров как частный случай argsMenu.

## Решения (из брейншторминга)

| Развилка | Решение |
| --- | --- |
| Объём 1-й итерации | Фундамент-реестр + миграция builtins + handlers-as-commands. Skills/YAML-тулы как источники — отдельным циклом позже. |
| Поверхности | Web-автодополнение, Telegram native menu, argsMenu-кнопки, Discord slash. |
| Авторинг handler-команд | Гибрид: авто-деривация (имя = id) + опциональный `<command>` в дескрипторе toolgate. |
| Исполнение | В обход LLM, детерминированно (как текущие slash-команды). |
| Архитектура | Реестр в Rust-ядре как единый источник истины; `GET /api/commands` для UI и channels. |

## Архитектура

Ядро получает `CommandRegistry` в `AppState`, агрегирующий `CommandSpec` из источников
через трейт `CommandSource`:

- **`BuiltinCommandSource`** — статичный список встроенных команд, каждая привязана к
  Rust-обработчику (миграция текущего `match`).
- **`HandlerCommandSource`** — деривация `CommandSpec` из `HandlerRegistry` (манифесты
  toolgate `/handlers`): одна команда на обработчик + опциональный `<command>`-оверрайд.

Обязанности реестра: агрегация + валидация (нет дублей имён/алиасов — порт
`assertCommandRegistry`), резолв по имени/алиасу, фильтрация по видимости для агента
(fse-allowlist, `required_base`, tool-deny), сериализация в JSON.

**Единый путь исполнения — ключевая инвариант.** Нативная команда Telegram, `/cmd` из
web-композера и Discord-slash приходят в ядро как обычное `Message` с текстом
`/cmd args` → `bootstrap` → `registry.dispatch`. Никакой поверхностно-специфичной логики
исполнения.

```
                    ┌─────────────────────────────┐
   GET /api/commands│   CommandRegistry (ядро)     │
   ◄────────────────┤   Builtin  ⊕  Handler        │
   │                │   validate · resolve · gate  │
   │                └──────────────┬──────────────┘
   ▼                               │ dispatch
 UI-композер (автодоп.)            ▼
 channels: setMyCommands   bootstrap.handle_command
 Discord register          builtin→fn · handler→enqueue · argsMenu→меню
   │                               ▲
   └── "/cmd args" как Message ────┘  (все каналы + web)
```

## Дескриптор `CommandSpec`

```rust
struct CommandSpec {
    name: String,              // канон, без слэша: "summarize_video"
    aliases: Vec<String>,      // ["sv"]
    description: String,       // резолвится по языку агента при сериализации
    category: CommandCategory, // session|options|status|management|media|tools
    scope: CommandScope,       // text | native | both
    args: Vec<CommandArg>,
    visibility: Visibility,    // all | base_only  (+ уважает allowlist/deny)
    source: CommandSourceKind, // Builtin | Handler { handler_id }
}
struct CommandArg {
    name: String,
    description: String,
    arg_type: ArgType,             // string|number|boolean
    required: bool,
    choices: Option<Choices>,      // Static(Vec<Choice>) | Dynamic(provider_key)
    capture_remaining: bool,       // хвост в один арг (url, текст, message)
    menu: bool,                    // участвует в argsMenu
}
```

- **Динамические choices** — именованные провайдеры в ядре (`thinking_levels`, `models`,
  `voices`), т.к. логика в Rust. Для валвсов обработчика choices берутся из определения
  `<config>`-поля.
- **Локализация — SoT в ядре (F5).** Описания резолвятся по языку агента при отдаче JSON
  (у хендлеров уже есть `descriptions[lang]`, у builtins — через `localization`). Сейчас
  описания команд для Telegram живут channel-side (`strings.cmdHelp`,
  [telegram.ts:200](../../channels/src/drivers/telegram.ts)), а для `/help` — в core
  `localization`. Реестр становится единственным источником: **ядро отдаёт локализованные
  описания, channels/UI их только потребляют** — channel-side `cmd*`-строки выпиливаются.
- **Нативное имя vs алиасы (F3).** `nativeName` (для `setMyCommands`/Discord) обязан
  соответствовать `[a-z0-9_]{1,32}` (Telegram) — генерируется санитизацией `name`. Алиасы с
  недопустимыми символами (`-`, дефисы) остаются **только text-scope** и в нативное меню не
  попадают. Валидация реестра проверяет это правило (порт `assertCommandRegistry`: text-only
  без nativeName, native без text-алиасов и т.д.).
- **argsMenu** («auto»): если у команды есть арг с choices и он не передан — вместо ошибки
  показываем меню-кнопки, переиспользуя инфраструктуру `handler_menu` (web rich-card +
  Telegram inline). Клик по кнопке переисполняет команду с выбранным значением.

## Источники команд

### BuiltinCommandSource

Миграция 14 существующих команд в `CommandSpec` + Rust-fn. Поведение сохраняется 1:1
(локализованные строки, gating). Изменения:

- `/help` и `/commands` — **генерируемые из реестра** (список видимых команд по
  категориям), а не статичная строка → всегда актуальны.
- Диспетчер из `match command { ... }` → резолв по реестру + вызов привязанного
  обработчика. Тела обработчиков (`/status`, `/goal`, …) переезжают почти без изменений,
  продолжая получать `CommandContext`.

### HandlerCommandSource

Для каждого манифеста обработчика деривируем:

- `name` = id обработчика (валидация `[a-z0-9_-]`), `description` из `descriptions[lang]`;
- один позиционный арг `source` (`capture_remaining`) — url или путь; необязателен, если
  файл приложен/недавний;
- **валвсы** (`<config>`-поля) → опциональные именованные арги с choices из определения
  поля;
- опциональный `<command name aliases args/>` в дескрипторе — оверрайд имени/алиасов/аргов;
- видимость: builtin-tier гейтится fse-allowlist, workspace-tier разрешён (как сейчас);
  async-only → enqueue `handler_jobs`.

**Приоритет и коллизии:** builtin-имена приоритетнее handler-деривированных; явный
`<command>` может просить алиас, но валидация реестра **отбрасывает конфликтующие алиасы с
предупреждением и никогда не перекрывает builtin** — `/status`, `/new` и пр. защищены.

## Исполнение и диспетчеризация

В `bootstrap`: `text.starts_with('/')` → `registry.resolve(name)`:

- **не найдено** → передаём в LLM (текущее поведение, без изменений);
- **builtin** → вызов fn с `CommandContext` (как сегодня);
- **handler** → резолв источника в порядке:
  1. явный url/путь-арг;
  2. вложение в том же сообщении — из `msg.attachments` (`bootstrap` уже получает их на
     шаге enrich, [bootstrap.rs:236](../../crates/opex-core/src/agent/pipeline/bootstrap.rs)),
     без обращения к БД;
  3. самый недавний файл в сессии — джойн `uploads`↔`messages`
     (`uploads.owner_type='client_upload' AND owner_id = <message uuid>` по сообщениям
     сессии, `created_at DESC`). **F4:** в `uploads` нет `session_id` — связь идёт через
     UUID сообщения в `owner_id` ([052_uploads_table.sql](../../migrations/052_uploads_table.sql));
  4. если обязательный арг пуст и есть choices → **argsMenu**; иначе короткий ответ
     «пришлите ссылку/файл».

  Затем enqueue `handler_job` (upload_id или source_ref) — **тот же путь, что у тула
  `file_handler` action=run**: переиспользуем `insert_handler_job` и трастовую проверку
  (handler обязан быть в matched-множестве — mime/домен/allowlist).

### Emission-контракт (F1 — правка контракта, не реализации)

Текущий ранний выход для slash-команд эмитит **только** `TextDelta`
([run.rs:172](../../crates/opex-core/src/agent/engine/run.rs) SSE,
[run.rs:394](../../crates/opex-core/src/agent/engine/run.rs) каналы), а `RichCard`
перехватывается лишь на пути инструментов
([sink.rs:142](../../crates/opex-core/src/agent/pipeline/sink.rs)). Команда минует
`execute`-цикл, поэтому `command_output: Option<String>` с `RICH_CARD_PREFIX` внутри уйдёт
**литеральным текстом**, а не меню. Меняем контракт вывода команды:

```rust
enum CommandOutcome {
    Text(String),                     // как сегодня → TextDelta + finalize Done
    Menu { card: serde_json::Value }, // argsMenu / handler-меню → RichCard event
}
// BootstrapOutcome.command_output: Option<CommandOutcome>
```

Ранний выход в `run.rs` учится эмитить `StreamEvent::RichCard` для варианта `Menu`, а
`ChannelStatusSink` и SSE-конвертер уже умеют его рендерить. Так команда получает тот же
меню-seam, что и тул `file_handler`, без прогонки через LLM. Всё по-прежнему уходит в
bootstrap → finalize, **в обход LLM**.

**Меню-callback.** Обобщаем существующую инфраструктуру. Сейчас `hm:<token>:<id>`
запускает handler по id; кнопка меню аргументов команды несёт `(command_name, arg_name,
value)` и при клике пере-входит в диспетчер с дозаполненными аргами. Web: rich-card постит
в обобщённый эндпоинт запуска команды (расширяем `/api/files/menu-run` либо новый
`/api/commands/run`). Новый card-type `command_args_menu` добавляется в матч
[sink.rs:144](../../crates/opex-core/src/agent/pipeline/sink.rs) (сейчас ловится только
`handler_menu`) и в web `card-registry.tsx`.

## Поверхности

**Web UI (композер).**
- при вводе `/` композер фетчит `GET /api/commands?agent=X` (кэш), рендерит
  автодополнение: имя, описание, арги; choices — чипами. Порт паттерна OpenClaw TUI
  `getSlashCommands`.
- argsMenu → новый rich-card `command_args_menu` в `card-registry.tsx` (рядом с
  существующим `handler_menu`).

**Telegram.**
- **Доставка списка команд (F2).** Сегодня `setMyCommands` — статичный хардкод из 7 команд
  ([telegram.ts:200](../../channels/src/drivers/telegram.ts)), у адаптера нет канала для
  динамического набора. Проектируем доставку: адаптер после handshake фетчит
  `GET /api/commands?scope=native&lang=<L>` (loopback, auth) и вызывает `setMyCommands`.
  Пер-язык — отдельный фетч + `setMyCommands(scope, language_code)`. Ограничение Telegram
  (нет аргументов в меню) — только имя+описание; арги вводятся текстом или через argsMenu.
  Статический список из driver удаляется.
- argsMenu → inline-кнопки (переиспользуем `send_buttons` +
  [существующий menu-callback](../../channels/src/drivers/telegram.ts) `MENU_CTX`).
- нативная команда приходит как обычный `/cmd args` текст → тот же диспетчер.

**Discord.**
- при старте адаптер регистрирует application (slash) commands с типизированными options +
  choices (Discord поддерживает нативно). Interaction → транслируется в inbound
  `/cmd args` → тот же диспетчер. Больше работы (обработка interaction, ack) → последняя
  фаза.

**Видимость per-agent.** Реестр фильтрует `GET /api/commands` и dispatch по агенту:
handler-команды уважают fse-allowlist + валвсы + tool-deny; `base_only` для системных при
необходимости. Non-base агенты уже авто-деноят часть тулов — зеркалим для соответствующих
команд.

## Безопасность (F6)

Команды идут **в обход LLM** и напрямую воздействуют на систему — поэтому явные контроли:

- **Трастовый гейт handler-команд.** Перед enqueue диспетчер пере-прогоняет
  `match_buttons`/`match_url_handlers`: handler обязан быть в matched-множестве для данного
  источника (mime/домен/allowlist/валвсы). Модель или пользователь **не могут** запустить
  denied/mismatched обработчик — тот же барьер, что у тула `file_handler`.
- **SSRF.** `/summarize_video <внутренний-url>` отсекается доменным фильтром
  `match_url_handlers` (обработчик запускается только если url подходит под его паттерн).
  Контроль называем явно, а не подразумеваем.
- **Флуд очереди.** Команда одним тапом enqueue-ит `handler_jobs` (та же экспозиция, что у
  меню-run, но команды делают её тривиальной) → применяем существующий rate-limit гейт +
  идемпотентность `insert_handler_job`; при необходимости — per-session троттлинг
  handler-команд.
- **Видимость = граница.** `GET /api/commands` и dispatch фильтруются по агенту (allowlist,
  deny, base_only); скрытая для агента команда не исполняется, даже если имя угадали.
- **Валидация имён.** Санитизация nativeName + `[a-zA-Z0-9_-]` на именах команд/алиасов —
  тот же анти-traversal инвариант, что для tool/MCP-имён.

## Тестирование (TDD)

- **Юнит:** валидация реестра (дубли имён/алиасов, консистентность scope — порт
  `assertCommandRegistry`); парсер аргов (positional, capture_remaining, валидация
  choices); резолв команды по алиасу.
- **Диспетчер:** паритет builtin с текущим `match`; резолв источника handler-команды
  (арг/вложение/недавний); эмиссия argsMenu.
- **API:** сериализация `/api/commands` + фильтрация по агенту/языку.
- **E2E на сервере (188.x):** `/summarize_video <url>` в Telegram ставит job;
  web-автодополнение; `setMyCommands` виден в клиенте; Discord slash (живой клик — юзер).
  CI: `cargo test --workspace` + tsc + gen-types drift (channels/UI).

## Фазы реализации (один спек)

1. **Фаза 1 — фундамент:** `CommandRegistry` + `CommandSpec` + `BuiltinCommandSource`
   (миграция 14 команд) + `GET /api/commands` + web-автодополнение. Полный паритет,
   ничего не ломаем.
2. **Фаза 2 — handlers + меню:** `HandlerCommandSource` (авто-деривация + `<command>` +
   валвсы) + резолв источника + argsMenu (web rich-card + Telegram inline) + Telegram
   `setMyCommands`.
3. **Фаза 3 — Discord:** регистрация slash-команд + обработка interaction.

### Заметки для планирования (F7–F10)

- **F7 — card-type `command_args_menu`:** добавить в матч
  [sink.rs:144](../../crates/opex-core/src/agent/pipeline/sink.rs) (сейчас только
  `handler_menu`) и в web `card-registry.tsx`. Фаза 2.
- **F8 — `GET /api/commands`:** auth-гейт (bearer, как остальной API) + инвалидация кэша.
  Набор handler-команд меняется вместе с `HandlerRegistry` (ETag toolgate) → отдавать
  версию/ETag, чтобы UI и каналы не держали устаревший список. Фаза 1 (базово) / Фаза 2
  (handler-версионирование).
- **F9 — парсер аргов:** детализировать алгоритм `argsParsing: positional` (кавычки,
  `choice`-арг + `capture_remaining` хвост) в плане; порт из OpenClaw `commands-args`.
  Фаза 1.
- **F10 — Discord greenfield:** в channels **нет** регистрации application-commands сегодня
  (в отличие от Telegram) — фаза 3 крупнее, чем читается: регистрация при старте, обработка
  `interactionCreate`, ack/deferReply, трансляция в inbound `/cmd args`.

## Не в объёме (явно)

- Skills и YAML-тулы как источники команд — отдельный цикл после фундамента.
- Плагины-команды в стиле OpenClaw dock-commands.
- Аргументы в нативном меню Telegram (ограничение платформы; решается argsMenu).

## Затрагиваемые компоненты

- **Ядро:** новый модуль `agent/commands/` (`registry.rs`, `spec.rs`, `sources/`),
  рефактор [commands.rs](../../crates/opex-core/src/agent/pipeline/commands.rs) под
  диспетчер, реюз `handler_registry.rs` / `insert_handler_job` / fse-allowlist / валвсы,
  новый handler в `gateway/handlers/` для `/api/commands` (+ обобщённый run-эндпоинт).
- **channels (TS):** `setMyCommands` (Telegram), регистрация slash (Discord), проброс
  argsMenu-кнопок через существующий menu-callback.
- **UI (Next.js):** автодополнение в композере, rich-card `command_args_menu`, типы в
  `types/api.ts` / `sse-events.ts`.
