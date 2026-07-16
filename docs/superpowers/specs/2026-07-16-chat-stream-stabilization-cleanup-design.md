# Стабилизация чат-стрима и уборка после миграции на sync-конверт

Дата: 2026-07-16
Статус: одобрено (дизайн-ревью с владельцем)
Источник: архитектурный аудит чат-UI от 2026-07-16 (3 параллельных исследования: фронтенд-архитектура, визуал/UX, бэкенд-контракт). Охват спеки — «волна 1» (баги) + «волна 3» (уборка) из аудита. UX-фичи (глобальный поиск, превью вложений, regenerate-с-моделью) и визуальная нормализация токенов — отдельные будущие спеки.

## Контекст

15 июля 2026 чат переведён на server-authoritative протокол: `POST /api/chat` = 202 + старт, единый `GET /api/chat/{id}/stream` отдаёт sync-конверт (`sync_begin → replay → sync_end → live`). Миграция прошла успешно (T8b/F1 фактически закрыты — подтверждено аудитом), но оставила мёртвый код, три сломанных механизма и один незакрытый пункт протокола (`truncated`). Эта спека закрывает их и попутно убирает накопившийся долг, чтобы будущие UX-фичи ложились на чистую базу.

## Цели

1. Исправить три бага: layout inline-редактирования сообщения, ложный visibility-stale reconnect, потеря событий при переполнении replay-буфера.
2. Убрать мёртвый код эпохи старого транспорта (non-batch путь, `onStreamDone`, `extractSseEventId`, комментарии-призраки).
3. Свести три дублирующих механизма finishing→history к одному.
4. Закрыть структурный дрейф WS-типов через codegen (как уже сделано для SSE).
5. Удалить мёртвые REST-endpoint'ы, дедуплицировать `update()`-хелпер.
6. Разбить переросшие `page.tsx` (971 строка) и `ChatComposer.tsx` (1232 строки) без изменения поведения.

## Не-цели

- Глобальный поиск по истории (endpoint `GET /api/sessions/search` сохраняется для будущей волны 2, UI не строится).
- Пагинация сайдбара сверх 40 сессий, превью вложений, regenerate с выбором модели (волна 2).
- Нормализация визуальных токенов, mermaid-синглтон, CometLoader (волна 4).
- Любые изменения протокола sync-конверта, `mergeRender`, generation-guard, `tts-speaker.ts` — лучшие части кодовой базы не трогаем.

## Структура: 4 батча

Работа в master (правило проекта). Каждый батч: TDD → серверный гейт (clippy `-D warnings` + cargo test на сервере) → деплой → E2E на проде. Порядок B1→B2→B3→B4; B4 независим и может идти параллельно.

| Батч | Содержание | Слои |
| --- | --- | --- |
| B1 | Слой стрима: уборка non-batch, visibility-stale, компакция буфера, консолидация finishing→history | Rust + UI (деплой парой) |
| B2 | WS-codegen | Rust + UI (деплой парой) |
| B3 | Мёртвые endpoint'ы, дедуп `update()`, комментарии | Rust + UI |
| B4 | Edit-layout, декомпозиция page.tsx / ChatComposer.tsx | UI |

---

## B1. Слой стрима

### B1.1 Уборка мёртвого non-batch пути

Все вызыватели `processStream` идут через `stream/chat-stream.ts` с `batchMode: true` — legacy-транспорта не осталось.

- `ui/src/stores/stream/stream-processor.ts`: удалить параметр `batchMode` и все ветки `if (!batchMode)` (строки ~522, ~534 и родственные), legacy-логику в `finally`, комментарий про «non-batchMode legacy transport» (~654-656), мёртвую ссылку на удалённый `step-finish` (~391-393).
- Удалить мёртвый колбэк `onStreamDone` (определение ~47, вызов ~664) — его потребителей не осталось.
- `ui/src/stores/sse-events.ts`: удалить `extractSseEventId` (используется только в тестах; `id:`-строки в процессоре явно пропускаются).
- `ui/src/stores/stream/chat-stream.ts`: убрать устаревший заголовочный комментарий «NOT wired into the app yet — cutover happens in T7».

Критерий приёмки: существующие vitest-тесты batch-пути зелёные без изменений семантики; grep по `batchMode` в `ui/src` пуст.

### B1.2 Фикс visibility-stale (ложный reconnect)

Проблема: после миграции `onEventActivity` не передаётся в новый транспорт (`chat-stream.ts:83-95` не пробрасывает колбэк), поэтому `recordEventActivity` вызывается только в `connect()`/`sendTurn()`. `_lastEventTime` = «время последнего connect», и порог `VISIBILITY_STALE_MS = 15_000` (`streaming-renderer.ts:56`) при возврате на вкладку ошибочно признаёт живой стрим протухшим (`streaming-renderer.ts:411`) и переоткрывает его.

Решение: `streaming-renderer.connect()` передаёт `onEventActivity` в `openTurnStream` → `processStream`, который уже вызывает его на каждом событии (`stream-processor.ts:163`). Семантика «время последнего события» восстанавливается.

Тест (vitest): стрим с событиями каждые 5 с при скрытой вкладке 20+ с → возврат видимости НЕ вызывает reconnect; стрим без событий 20+ с → reconnect происходит (существующее поведение сохраняется).

### B1.3 Компакция replay-буфера (замена клиентского truncated-восстановления)

Семантика переполнения сегодня (`crates/opex-core/src/gateway/stream_registry.rs:175-183`): буфер хранит **первые** `MAX_BUFFER_SIZE = 10_000` событий; последующие уходят только в live-broadcast, `truncated=true`. Поздний подписчик получает head-снапшот и live с момента подписки — **середина хода теряется**. Задуманное в комментарии `sse.rs` клиентское восстановление «частичный текст из REST + хвост буфера» отвергнуто на дизайн-ревью: у персистнутого текста нет seq-выравнивания с буфером, корректная стыковка нереализуема без дублей/дыр, и она добавляет сложность в `stream-processor.ts`, откуда мы её убираем.

Решение — серверная компакция:

- При достижении `MAX_BUFFER_SIZE` в `push_event` выполнить компакцию `events`: смежные события `text-delta` (и `reasoning-delta`) одного блока (`id` блока) сливаются в одно событие с конкатенированной `delta`; событию присваивается seq-id **последнего** из слитых событий — монотонность сохраняется, seq-cutoff у подписчиков (`stream.rs:198-206`) продолжает работать без изменений.
- Компакция выполняется под уже взятым per-stream Mutex; события хранятся как JSON-строки, поэтому компакция включает parse → merge → serialize только для delta-событий (не-delta копируются как есть).
- После компакции буфер продолжает принимать события. Повторные компакции — по мере повторного заполнения.
- Fallback: если компакция не уменьшила буфер ниже порога (патология: буфер из 10k не-delta событий, например tool-событий), поведение остаётся прежним (broadcast-only) и ставится `truncated=true`.
- Клиент: `truncated=true` в `sync_begin` (теперь достижимо только в патологическом случае) → ненавязчивый баннер «ответ длинный, полный текст появится по завершении» и опора на финальный refetch. Никакой стыковочной логики.

Поле `sync_begin.truncated` в протоколе сохраняется (обратная совместимость), комментарий в `crates/opex-types/src/sse.rs` о клиентском восстановлении переписывается под новую семантику.

Тесты:

- `#[sqlx::test]` (с seed FK-строки sessions — готча проекта): 10k+ text-delta → `subscribe()` возвращает семантически полный replay (конкатенация дельт равна исходной), seq монотонный, `truncated=false`;
- патологический кейс (10k+ не-delta) → `truncated=true`;
- существующий `overflow_sets_truncated` адаптируется под новую семантику.
- vitest: `sync_begin.truncated=true` → рендерится баннер.

### B1.4 Консолидация finishing→history

Сейчас один переход live→history обслуживают три механизма: post-finally в `stream-processor.ts:671-731`, `onFinished` в `streaming-renderer.ts:214-227`, `finalizeHandoff`-effect в `ChatThread.tsx:113-121`. Все три срабатывают, давая двойную-тройную инвалидацию одних query.

Решение: авторитетным остаётся **post-finally в stream-processor** (ближе всех к данным, уже содержит id-guard `assistantPersisted`, который держит `finishing`-оверлей до реального появления assistant-row в refetch).

- `onFinished` в renderer: остаётся только UI-side (перевод фазы в idle), его `invalidateQueries` (sessions+messages) удаляется.
- `finalizeHandoff`-effect в `ChatThread.tsx` удаляется вместе с действием `finalizeHandoff` в `navigation.ts`, если других потребителей нет.

Тест (vitest): гонка «finish пришёл, refetch ещё не содержит assistant-row» → оверлей остаётся в `finishing`, после refetch с row — переход в `history`, ровно одна инвалидация messages-query на завершение хода.

## B2. WS-codegen

Проблема: `ui/src/types/ws.ts` — ручной union; источник на бэке размазан по `ui_event_tx.send(json!({...}))` в 7 модулях (`approval_manager.rs`, `engine/stream.rs`, `bootstrap.rs`, `canvas.rs`, `files.rs`, `main.rs`, `sessions.rs`). Уже есть жертва дрейфа: `agent_joined` шлётся бэком (`sessions.rs:577-584`), фронт о нём не знает.

Решение — повторить проверенный SSE-паттерн:

- Новый `crates/opex-types/src/ws.rs`: `enum WsEvent` с `#[derive(Debug, Clone, Serialize, TS)]`, `#[serde(tag = "type", rename_all = "snake_case")]`. Варианты — все текущие события: `Notification`, `AgentProcessing`, `ApprovalRequested`, `ApprovalResolved`, `SessionUpdated`, `FileJobProgress`, `CanvasUpdate`, `ChannelsChanged`, `Log`, `AuditEvent`, `AgentJoined`. Поля вариантов снимаются с фактических `json!`-мест (wire-совместимость 1:1, никаких переименований — это рефакторинг сериализации, не протокола).
- Все `ui_event_tx.send(json!({...}))` переводятся на типизированные варианты. Способ (смена типа канала на `WsEvent` либо сериализация на границе send) выбирается в плане реализации по наименьшему диффу.
- Codegen: ts-rs → `ui/src/types/ws.generated.ts`; `ui/src/types/ws.ts` становится реэкспортом (по образцу `sse-events.ts`).
- Контракт-тест: Rust-тест генерирует фикстуры всех WS-событий (по образцу `sse_wire.rs`), vitest валидирует их против generated-типов с жёстким счётчиком вариантов. Готча проекта: фикстуры без финального перевода строки.
- Фронт: добавить подписку `useWsSubscription("agent_joined")` в chat page → `updateSessionParticipants` (сейчас участники обновляются только через SSE/REST — WS-путь закроет запаздывание при invite из другого клиента).

Критерий приёмки: grep `ui_event_tx.send(json!` пуст; счётчики фикстур совпадают; CI gen-types drift зелёный.

## B3. Малая уборка

### B3.1 Удаление мёртвых endpoint'ов

Удаляются (фронт не вызывает, назначение закрыто другими путями):

| Endpoint | Замена/причина |
| --- | --- |
| `GET /api/sessions/{id}/active-path` | UI резолвит путь клиентски (`chat-history.ts::resolveActivePath`) |
| `GET /api/sessions/latest` | UI использует список + `?s=` URL-параметр |
| `GET /api/sessions/{id}/export` | UI строит markdown клиентски (`session-crud.ts::sessionToMarkdown`) |
| `PATCH /api/messages/{id}` (edit content) | edit идёт через `POST /api/sessions/{id}/fork` |
| `POST /api/sessions/{id}/compact` | `/compact` уходит как обычное сообщение (command registry) |

Предохранитель: перед удалением — grep на прод-сервере по `~/opex/workspace/tools/*.yaml`, `~/opex/workspace/skills/`, `~/opex/config/skills/` (агенты могли завязаться на эти URL). Найденное — сначала мигрировать/вычистить, потом удалять.

Сохраняются: `GET /api/sessions/search` (нужен волне 2 — глобальный поиск), `POST /api/sessions/{id}/retry` и `GET /api/sessions/stuck` (watchdog), `GET /api/sessions/failures` (админ-UI).

### B3.2 Дедуп store-хелперов

Идентичный `update(agent, patch)` скопирован 4 раза (`streaming-renderer.ts:84-89`, `navigation.ts:34-39`, `composer.ts:13-18`, `session-crud.ts:47-52`), `ensure()` — в `navigation.ts:21-32`. Вынести в `ui/src/stores/chat/actions/_shared.ts`, все копии заменить импортом.

### B3.3 Комментарии-призраки

- `chat-types.ts:159-171` — описание удалённых фаз `reconnecting`/`complete` переписать под текущую FSM.
- Прочие следы T6/T7-эпохи по результатам grep (`step-finish`, «cutover», «legacy transport»).

## B4. UI

### B4.1 Фикс layout inline-редактирования

Проблема: `EditButton` в edit-режиме рендерит textarea внутри header-контейнера экшенов (`flex shrink-0 items-center gap-1`, `MessageItem.tsx:201-211`) — форма оказывается в правом верхнем углу шапки и сжимается на узких экранах.

Решение: состояние `editing` поднимается на уровень `MessageItem`. В edit-режиме тело user-сообщения заменяется full-width формой (textarea + Save/Cancel; Enter = save, Esc = cancel, Shift+Enter = перенос строки), header-экшены на время редактирования скрываются. Логика сабмита не меняется (`forkAndRegenerate`). Swipe-right-to-edit продолжает включать тот же режим.

Тест (vitest): в edit-режиме textarea находится в теле сообщения (не в header), Esc возвращает исходный рендер, save вызывает `forkAndRegenerate` с новым текстом.

### B4.2 Декомпозиция `page.tsx` (971 → ~400 строк)

Выносим без изменения поведения (чистый перенос, никаких «попутных улучшений» в ломком restore-коде):

- Restore-машина (5 приоритетов, `overrideUrlSession` с трёхзначной логикой, `restoredAgents` ref, cross-agent URL-resolver, `urlResolveFetched` ref — строки ~78-258) → `hooks/use-session-restore.ts`.
- WS-подписки (`agent_processing`, `session_updated`, `file_job_progress` — строки ~286-330) → `hooks/use-chat-ws.ts`.
- Сайдбар сессий (список, фильтр, multi-select delete, rename) → `SessionSidebar.tsx`.

### B4.3 Декомпозиция `ChatComposer.tsx` (1232 → ~600 строк)

- Голосовой контур (STT/TTS/VAD/continuous/voice-settings, rising/falling-edge эффекты ~508-558) → хук `useVoiceReply` + компонент `VoiceControls.tsx`. Порядок эффектов сохраняется бит-в-бит — это подтверждённо хрупкое место (память проекта: голосовой UX-редизайн 2026-07-15); переносим, не переписываем.
- Композер после выноса: textarea, вложения, слэш/@mention-автокомплиты, model dropdown, send/stop/queue.

Критерий приёмки B4.2/B4.3: поведенческих изменений нет; существующие vitest-тесты зелёные; ручной E2E голосового контура (микрофон → транскрипт → озвучка ответа) и restore-сценариев (deep-link `?s=`, переключение агентов, refresh во время стрима) на проде.

## Тестирование и деплой (сводно)

- TDD: тест до правки (правило проекта).
- UI: vitest строго из `ui/` (готча worktree/CWD).
- Rust: тесты на сервере, BIN-таргет (`--bin opex-core`), сборка `CARGO_BUILD_JOBS=4 nice ionice` detached; `#[sqlx::test]` с `migrations = "../../migrations"` и seed FK-родителей.
- Перед push: workspace-тесты + tsc + gen-types drift (все три — CI гоняет больше, чем `make test-db`). Push только с явного подтверждения владельца.
- Деплой: B1, B2 — парой (server-deploy.sh + deploy-ui.sh, протокольно связанные слои); B3 — по затронутому слою; B4 — только deploy-ui.sh.
- E2E на проде после каждого батча: B1 — длинный ход с reconnect + скрытая вкладка; B2 — approval/notification/invite сценарии; B4 — edit сообщения на мобильной ширине, голосовой круг, restore-сценарии.

## Риски

| Риск | Митигация |
| --- | --- |
| Компакция буфера ломает seq-cutoff у подписчиков | id слитого события = id последнего из слитых; sqlx-тест на монотонность + существующие тесты replay |
| Смена типа WS-канала затрагивает 7 модулей | Wire-формат фиксируется фикстурами ДО рефакторинга; варианты снимаются с фактических json!-мест |
| Декомпозиция composer ломает голосовой контур | Чистый перенос без переписывания; ручной E2E голоса в приёмке |
| Удалённый endpoint нужен агентскому YAML-тулу | Grep по workspace/config на проде до удаления |
| Консолидация finishing→history оголяет гонку, которую закрывал backstop | Тест гонки «finish до refetch»; id-guard `assistantPersisted` сохраняется |
