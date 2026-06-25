# UI для decision-hooks + checkpoint/rollback

**Дата:** 2026-06-25
**Статус:** Design v1 (одобрено в brainstorming по 4 секциям; ожидает финального spec review)

## Цель

Дать UI двум уже-задеплоенным backend-фичам, у которых сейчас управление только через TOML/PUT API и slash-команды:
1. **Decision-hooks editor** — редактирование decision-webhooks агента в `AgentEditDialog` (+ backend: отдать `webhooks` в GET-DTO).
2. **Checkpoint/rollback панель** — панель в чате (список/diff/откат) + новый REST API поверх `CheckpointManager` (сейчас только slash `/rollback`).

## Контекст (как сейчас — по UI-аудиту)

- **Clarify UI** уже полный (`ClarifyCard.tsx`) — НЕ трогаем.
- **AgentEditDialog** (`ui/src/app/(authenticated)/agents/AgentEditDialog.tsx`): вкладка behavior, секция hooks с `hooksLogAll` (toggle) + `hooksBlockTools` (input) (~604-611). Сохранение через PATCH/PUT агента. `webhooks` в форме НЕТ.
- **Backend GET-DTO** `AgentDetailHooksDto` (`gateway/handlers/agents/dto.rs`; ts-rs → `ui/src/types/api.generated.ts:28`) = только `log_all_tool_calls`, `block_tools`. **`webhooks` НЕ возвращается** (подтверждено E2E). PUT (`HooksPayload.webhooks`, `schema.rs:94`) уже принимает.
- **Checkpoint:** backend `CheckpointManager` (`agent/checkpoint_manager.rs`) с `list_checkpoints`/`diff`/`restore`/`enabled`; доступен через `engine.cfg().checkpoint_manager: Option<Arc<…>>`. HTTP API **отсутствует** — только slash `/rollback` (`pipeline/commands.rs`). Резолв engine: `AgentCore::get_engine(name)`.
- **Чат:** `ui/src/app/(authenticated)/chat/` — `ContextBar.tsx` (хедер/тулбар), `MessageItem.tsx`, store `chat-store` (`currentAgent`). API-слой `ui/src/lib/api.ts`. React Query (`lib/queries`).
- **shadcn:** `sheet.tsx`, `dialog.tsx`, `alert-dialog.tsx`, `confirm-dialog.tsx` — присутствуют.
- **Сущности webhook:** backend `WebhookConfig { url, events, mode, tool_matcher, on_failure, timeout_ms, allow_internal }`; `CheckpointMeta { n, commit, created, summary }`; `RestoreReport { n, files, new_checkpoint }`.

## Решения (brainstorming, 4 секции — одобрены)

1. **Hooks-редактор** → в существующей секции hooks `AgentEditDialog` (НЕ отдельная страница).
2. **Checkpoint-панель** → в чате (`Sheet` из `ContextBar`-триггера), НЕ в AgentEditDialog/отдельной странице. Чекпойнты per-agent → панель для `currentAgent`.
3. **Checkpoint REST API** — новый, под `/api/agents/{name}/` (паттерн sub-router `routes()→merge`).
4. **Hooks GET-DTO** — расширить `webhooks` (закрывает gap E2E).
5. **Restore деструктивен** (откатывает файлы агента) → confirm-dialog обязателен.
6. **Decision-only поля** (tool_matcher/on_failure/timeout_ms/allow_internal) — progressive disclosure, видны только при `mode=decision`.

## Non-goals (YAGNI)

- Создание чекпойнтов вручную из UI (чекпойнты авто, перед мутацией; spec checkpoint §non-goals).
- SSE-события для хуков/чекпойнтов (UI читает по запросу/refetch, не push).
- branching чекпойнтов, отдельная страница хуков, управление async-webhooks отдельно от decision (один список).
- Изменение clarify UI.

## Компоненты

### 1. Backend — Checkpoint REST API

Новый sub-router `gateway/handlers/agents/checkpoints.rs`, `pub(crate) fn routes() -> Router<AppState>`, merge в `gateway/handlers/mod.rs` (или в agents-mod, как собираются прочие agents-роуты). Все хендлеры: Bearer-auth (middleware), `agent_name` валидируется (charset), резолв `AgentCore::get_engine(name)` → `cfg().checkpoint_manager`.

- `GET /api/agents/{name}/checkpoints` → `200 [{n, commit, created, summary}]` (serde of `CheckpointMeta`). Нет агента → 404; `checkpoint_manager` None/`!enabled` → `200 []` (пустой, чтобы UI показал «нет»).
- `GET /api/agents/{name}/checkpoints/{n}/diff` → `200 {"diff": string}`. Невалидный N → 400/404 (из `diff` Err).
- `POST /api/agents/{name}/checkpoints/{n}/restore` body `{"file": string?}` → `200 {"n", "files": [..], "new_checkpoint": n?}` (serde of `RestoreReport`). Невалидный N / path-traversal → 400 (из `restore` Err — менеджер валидирует).
- DTO-структуры (serde) для ответов: `CheckpointMetaDto`, `RestoreReportDto` (или прямой serde на `CheckpointMeta`/`RestoreReport` — добавить `#[derive(Serialize)]` если нет).

### 2. Backend — Hooks GET-DTO

В `gateway/handlers/agents/dto.rs` расширить `AgentDetailHooksDto`: добавить `webhooks: Vec<WebhookConfig>` (или `Vec<WebhookDto>` с полями url/events/mode/tool_matcher/on_failure/timeout_ms/allow_internal). Заполнять из `agent_cfg.agent.hooks.webhooks` при сборке DTO. `WebhookConfig` уже `Serialize` (нужен `#[derive(TS)]`/ts-rs если api.generated.ts генерится — сверить как у соседних DTO; если `WebhookConfig` не ts-rs-аннотирован, добавить либо ввести `WebhookDto` с `#[derive(TS)]`). ts-rs codegen: `cargo run --bin gen_ts_types` обновит `api.generated.ts`. PUT-путь не трогаем (уже принимает `HooksPayload.webhooks`).

### 3. UI — Checkpoint-панель (`chat/CheckpointPanel.tsx`)

- **Триггер:** кнопка-иконка в `ContextBar.tsx` (history/rewind), вызывает открытие `Sheet`.
- **`Sheet`** (shadcn, справа): заголовок «Чекпойнты — {currentAgent}». Список из `useCheckpoints(currentAgent)` (React Query, GET endpoint), refetch при открытии (`enabled: open`).
- **Строка:** `N` · relative time (`created`, через существующий util относительного времени или `Intl`) · `summary`. Кнопки: **Diff** (GET diff → `Dialog` с `<pre className="diff">` +/− подсветкой), **Откатить** (`confirm-dialog` → `useRestoreCheckpoint` POST → toast (`sonner`) «Откат к N выполнен» + `invalidateQueries(["checkpoints", agent])`).
- **Пусто:** «Чекпойнтов нет».
- **Ошибки:** toast.error на API-сбой; панель не валит чат.

### 4. UI — API + хуки

- `lib/api.ts`: `listCheckpoints(agent)`, `diffCheckpoint(agent, n)`, `restoreCheckpoint(agent, n, file?)` — fetch с auth-заголовком (как прочие в api.ts).
- React Query (`lib/queries`): `useCheckpoints(agent)` (key `["checkpoints", agent]`), `useRestoreCheckpoint()` (mutation, invalidate на успех).

### 5. UI — Hooks-редактор (`AgentEditDialog.tsx`)

- В секции hooks добавить подсекцию «Decision webhooks»: массив строк (локальный state, init из `data.hooks.webhooks`).
- Строка: `url` (Input), `events` (чекбоксы BeforeMessage/BeforeToolCall/AfterToolResult), `mode` (Select async|decision), и при `mode=decision`: `tool_matcher` (Input), `on_failure` (Select open|closed), `timeout_ms` (Input number), `allow_internal` (Switch). «×» удалить; «+ Добавить webhook».
- Сохранение: включить `webhooks` массив в payload hooks при PATCH/PUT (рядом с log_all/block_tools). Пустой массив = нет хуков.
- Типы: `WebhookDto` из `api.generated.ts` (после Секции 2 codegen).

## Данные / поток

- Checkpoint: UI → GET/POST REST → handler → `CheckpointManager` (per-agent) → git shadow-store. Restore деструктивен (confirm). React Query кэш инвалидируется после restore.
- Hooks: GET агента → DTO с webhooks → форма; правка → PUT → config hot-reload → `set_webhooks` пере-компилирует. Никакого SSE.

## Тестирование (TDD)

**Backend (Rust):**
- Checkpoint handlers: GET checkpoints → 200 + JSON shape; 401 без токена; GET diff невалидный N → 4xx; POST restore невалидный N → 4xx; `agent_name` с `/`/`..` → reject; manager None → `[]`.
- Hooks DTO: serde — `AgentDetailHooksDto` содержит `webhooks` со всеми полями (round-trip).

**UI (vitest + @testing-library/react):**
- `CheckpointPanel`: рендер списка (мок useCheckpoints); пустое «Чекпойнтов нет»; «Откатить» → confirm → restore API (мок) вызван + invalidate; «Diff» → diff показан.
- Hooks-редактор: add/edit/remove webhook-строки; `mode=decision` показывает decision-поля, `async` — скрывает; payload содержит `webhooks` при сохранении.
- API-fns: `listCheckpoints`/`diffCheckpoint`/`restoreCheckpoint` → корректные URL/метод/заголовки.

## Open questions / future

- SSE push-обновление панели чекпойнтов при авто-чекпойнте (сейчас refetch).
- Ручное создание чекпойнта из UI.
- Управление async-webhooks (сейчас один список с decision).
- UI-индикатор «хуки активны» на агенте в списке.
