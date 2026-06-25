# UI для decision-hooks + checkpoint/rollback

**Дата:** 2026-06-25
**Статус:** Design v2 (одобрено в brainstorming + адверсариальное ревью учтено; ожидает финального spec review)

## Ревью (адверсариальное) — учтено в v2

CRIT: (1) **data-loss** — PUT `HooksPayload.webhooks=unwrap_or_default()` + UI не шлёт webhooks → сохранение агента обнуляет webhooks с диска → backend **preserve-on-omit** + UI всегда шлёт полный массив (read+write atomic); (2) `get_engine` резолвит только ЗАПУЩЕННЫХ агентов → checkpoint REST резолвит **shared `CheckpointManager` из AppState** (process-wide singleton), не через engine. HIGH: (3) `WebhookConfig` не ts-rs → `WebhookDto` (TS-аннот.); (4) `CheckpointMeta`/`RestoreReport` `pub(crate)` без Serialize → `CheckpointMetaDto`/`RestoreReportDto`. MED: GET checkpoints → `{enabled, items}`; `created` ISO8601 → `Intl.RelativeTimeFormat`. **Декомпозиция: 2 плана** (A=hooks, B=checkpoint).

## Декомпозиция (2 плана)

- **План A — Decision-hooks UI + data-loss фикс** (atomic): backend preserve-on-omit + `webhooks` в GET-DTO (`WebhookDto`) + UI-редактор в AgentEditDialog (всегда шлёт webhooks). ПЕРВЫМ (закрывает CRIT data-loss).
- **План B — Checkpoint REST + панель**: REST API (резолв из AppState) + `CheckpointMetaDto`/`RestoreReportDto` + панель в чате.

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

Новый sub-router `gateway/handlers/agents/checkpoints.rs`, `pub(crate) fn routes() -> Router<AppState>`, merge как прочие agents-роуты. Bearer-auth (middleware), `agent_name` валидируется (charset). **Резолв CheckpointManager — из AppState/AgentDeps (process-wide shared `Arc<CheckpointManager>`), НЕ через `get_engine`** (ревью CRIT-2: get_engine отдаёт только ЗАПУЩЕННЫХ агентов → чекпойнты стопнутого агента были бы недоступны). `workspace_dir` — из `AgentDeps.workspace_dir` (глобальный root; менеджер сам строит `agents/{agent}`).

- `GET /api/agents/{name}/checkpoints` → `200 {"enabled": bool, "items": [CheckpointMetaDto]}` (ревью MED: отличить disabled от пусто). `list_checkpoints` при `!enabled` уже даёт `[]`; `enabled` берём из `manager.enabled()`.
- `GET /api/agents/{name}/checkpoints/{n}/diff` → `200 {"diff": string}`. Невалидный N / disabled → 4xx (из `diff` Err).
- `POST /api/agents/{name}/checkpoints/{n}/restore` body `{"file": string?}` → `200 RestoreReportDto`. Невалидный N / path-traversal / disabled → 4xx (из `restore` Err — менеджер валидирует).
- **DTO обязательны** (ревью HIGH-2: `CheckpointMeta`/`RestoreReport` — `pub(crate)` без Serialize): `CheckpointMetaDto {n, commit, created, summary}` + `RestoreReportDto {n, files, new_checkpoint}` в handler-модуле, `#[derive(Serialize, TS)]`; маппинг из менеджер-типов. `created` = ISO8601-строка (`%cI`).

### 2. Backend — Hooks GET-DTO

**`WebhookDto` (новый, ts-rs)** — ревью HIGH-1: `WebhookConfig`/`WebhookMode`/`FailureMode` не `#[derive(TS)]`, codegen их не выдаст. Ввести `WebhookDto { url:String, events:Vec<String>, mode:String("async"|"decision"), tool_matcher:Option<String>, on_failure:String("open"|"closed"), timeout_ms:u64, allow_internal:bool }` с `#[derive(Serialize, Deserialize, TS)]` (в `dto.rs`/dto_structs). Расширить `AgentDetailHooksDto`: `webhooks: Vec<WebhookDto>`, заполнять из `agent_cfg.agent.hooks.webhooks` (маппинг enum→lowercase string). Зарегистрировать `WebhookDto` в `gen_ts_types.rs` + поднять min-count guard. `cargo run --bin gen_ts_types` → `api.generated.ts`.

**CRIT data-loss фикс (preserve-on-omit) — ОБЯЗАТЕЛЕН в этом же плане:** сейчас PUT-обработчик `HooksPayload.webhooks.unwrap_or_default()` (`schema.rs:282`) → при отсутствии webhooks в payload **обнуляет** webhooks с диска; текущий UI-payload (`agents/page.tsx`) webhooks не шлёт. Фикс: в update-обработчике если `payload.hooks.webhooks` = `None` → **сохранить существующие** webhooks агента (как base/delegation «preserved from disk»), НЕ затирать. Плюс UI (Компонент 5) ВСЕГДА шлёт полный `webhooks`. Read (GET-DTO) + write (preserve + UI) — один atomic-инкремент (План A).

### 3. UI — Checkpoint-панель (`chat/CheckpointPanel.tsx`)

- **Триггер:** кнопка-иконка в `ContextBar.tsx` (history/rewind), вызывает открытие `Sheet`.
- **`Sheet`** (shadcn, справа): заголовок «Чекпойнты — {currentAgent}». Данные из `useCheckpoints(currentAgent)` (React Query, GET → `{enabled, items}`), refetch при открытии (`enabled: open`).
- **Строка:** `N` · relative time (`Intl.RelativeTimeFormat` от `Date.parse(created)` — created = ISO8601) · `summary`. Кнопки: **Diff** (GET diff → `Dialog` с `<pre className="diff">` +/− подсветкой), **Откатить** (`confirm-dialog` → `useRestoreCheckpoint` POST → toast (`sonner`) «Откат к N выполнен» + `invalidateQueries(["checkpoints", agent])`).
- **Состояния:** `enabled=false` → «Чекпойнты отключены»; `enabled=true` && `items=[]` → «Чекпойнтов нет» (различаем по полю `enabled`).
- **Ошибки:** toast.error на API-сбой; панель не валит чат.

### 4. UI — API + хуки

- `lib/api.ts`: `listCheckpoints(agent)`, `diffCheckpoint(agent, n)`, `restoreCheckpoint(agent, n, file?)` — fetch с auth-заголовком (как прочие в api.ts).
- React Query (`lib/queries`): `useCheckpoints(agent)` (key `["checkpoints", agent]`), `useRestoreCheckpoint()` (mutation, invalidate на успех).

### 5. UI — Hooks-редактор (`AgentEditDialog.tsx`)

- В секции hooks добавить подсекцию «Decision webhooks»: массив строк (локальный state, init из `data.hooks.webhooks`).
- Строка: `url` (Input), `events` (чекбоксы BeforeMessage/BeforeToolCall/AfterToolResult), `mode` (Select async|decision), и при `mode=decision`: `tool_matcher` (Input), `on_failure` (Select open|closed), `timeout_ms` (Input number), `allow_internal` (Switch). «×» удалить; «+ Добавить webhook».
- Сохранение: **ВСЕГДА включать полный `webhooks` массив** в payload hooks при PATCH/PUT (рядом с log_all/block_tools) — даже пустой (`[]` = осознанно нет хуков). Вместе с backend preserve-on-omit это закрывает CRIT data-loss.
- Типы: `WebhookDto` из `api.generated.ts` (после Компонента 2 codegen). Форма: events — чекбоксы → `string[]`; mode/on_failure — `"async"|"decision"`/`"open"|"closed"` (lowercase, как serde).

## Данные / поток

- Checkpoint: UI → GET/POST REST → handler → `CheckpointManager` (per-agent) → git shadow-store. Restore деструктивен (confirm). React Query кэш инвалидируется после restore.
- Hooks: GET агента → DTO с webhooks → форма; правка → PUT → config hot-reload → `set_webhooks` пере-компилирует. Никакого SSE.

## Тестирование (TDD)

**Backend (Rust) — План A (hooks):**
- **data-loss preserve (CRIT):** PUT агента с `hooks` БЕЗ `webhooks` → существующие webhooks на диске СОХРАНЯЮТСЯ (не обнулены). PUT с `webhooks=[...]` → заменяет.
- Hooks DTO: serde — `AgentDetailHooksDto` содержит `webhooks: Vec<WebhookDto>` со всеми полями (round-trip, mode/on_failure lowercase).
- gen_ts_types: `WebhookDto` присутствует в выводе (codegen-тест/min-count).

**Backend (Rust) — План B (checkpoint):**
- GET checkpoints → 200 `{enabled, items}` JSON shape; 401 без токена.
- **stopped-agent:** checkpoints резолвятся даже когда engine не запущен (резолв из AppState, не get_engine).
- GET diff невалидный N → 4xx; POST restore невалидный N / path-traversal → 4xx; `agent_name` с `/`/`..` → reject; `!enabled` → diff/restore 4xx, GET → `{enabled:false, items:[]}`.

**UI (vitest + @testing-library/react):**
- `CheckpointPanel`: рендер списка (мок useCheckpoints); пустое «Чекпойнтов нет»; «Откатить» → confirm → restore API (мок) вызван + invalidate; «Diff» → diff показан.
- Hooks-редактор: add/edit/remove webhook-строки; `mode=decision` показывает decision-поля, `async` — скрывает; payload содержит `webhooks` при сохранении.
- API-fns: `listCheckpoints`/`diffCheckpoint`/`restoreCheckpoint` → корректные URL/метод/заголовки.

## Open questions / future

- SSE push-обновление панели чекпойнтов при авто-чекпойнте (сейчас refetch).
- Ручное создание чекпойнта из UI.
- Управление async-webhooks (сейчас один список с decision).
- UI-индикатор «хуки активны» на агенте в списке.
