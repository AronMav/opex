# Chat UX Wave 2 — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Поиск-палитра Ctrl+K с переходом к сообщению (включая неактивные ветки), букмарки, infinite-scroll сайдбара, превью вложений, одноразовый выбор модели при regenerate, scroll-позиция сессии, библиотека промптов.

**Architecture:** 4 батча (W2-A палитра → W2-B букмарки → W2-C пагинация → W2-D мелкие), каждый со своим тест-циклом и деплоем парой. Спека: `docs/superpowers/specs/2026-07-16-chat-ux-wave2-design.md` (ревизия 2 — все line-refs и структуры в ней провалидированы адверсариальным ревью против кода).

**Tech Stack:** Rust (axum, sqlx FTS/ts_headline), Next.js 16 / React 19 / Zustand / react-virtuoso / vitest.

## Global Constraints

- Работа в **master**; push ТОЛЬКО с явного подтверждения владельца; никакой Claude-атрибуции в коммитах.
- НИКОГДА `git add -A`/`git add .` — только явные пути (в master параллельно работает владелец).
- vitest ТОЛЬКО из `ui/`; Rust-тесты НЕ на Windows — серверный контур: bundle → `ssh aronmav@188.246.224.118 'bash -lc "..."'` (bash -lc обязателен для cargo PATH), clone в `~/wip-test-w2`, `make test-db-up`, `DATABASE_URL=postgres://opex_test:opex_test@127.0.0.1:5434/opex_test cargo test -p opex-core --bin opex-core`, всё под `CARGO_BUILD_JOBS=4 nice -n 15 ionice -c3`.
- `cargo clippy --all-targets -- -D warnings` обязателен; `#[sqlx::test(migrations = "../../migrations")]` + seed FK-родителей.
- `make gen-types` (`cargo run --features ts-gen --bin gen_ts_types -p opex-core`) без диффа перед КАЖДЫМ push; W2-B меняет ts-экспортированный `MessageRow` → regen+commit generated В ТОМ ЖЕ батче. `make` не в Win PATH — команды из Makefile напрямую.
- Никаких новых сырых design-значений (ESLint `no-raw-design-values`); только токены.
- FTS: везде `plainto_tsquery('russian', …)` bind-параметром — конфиг обязан совпадать с писателем tsv (m011).
- Деплой: каждый батч парой — `ssh aronmav@188.246.224.118 'bash ~/opex-src/scripts/server-deploy.sh'` + `bash scripts/deploy-ui.sh`; E2E-смоук после каждого.

---

## Батч W2-A — поиск-палитра

### Task 1: Бэкенд поиска — message_id, session_title, сниппет, all-режим, секция сессий

**Files:**

- Modify: `crates/opex-db/src/sessions.rs` (`search_messages` ~1597-1666, `SearchResult` ~1659; новая `search_session_titles`)
- Modify: `crates/opex-core/src/gateway/handlers/sessions.rs` — handler `api_search_sessions` (~448-487) + `SessionSearchQuery`
- Test: `#[sqlx::test]` в `crates/opex-core/src/gateway/handlers/sessions.rs` `mod tests` (или рядом с существующими тестами файла — следовать локальному паттерну)

**Interfaces:**

- Produces: `search_messages(db, agent_id: Option<&str>, query, limit) -> Vec<SearchResult>` — `agent_id: None` = все агенты; `SearchResult` + `message_id: Uuid`, `session_title: Option<String>`, `agent_id: String`, `snippet: String`; `search_session_titles(db, agent_id: Option<&str>, query, limit) -> Vec<SessionTitleHit>` (`{session_id, title, agent_id, last_message_at}`); ответ handler'а: `{"messages":[...], "sessions":[...], "count": N}`.

- [ ] **Step 1: Падающий sqlx-тест**

```rust
    #[sqlx::test(migrations = "../../../migrations")]
    async fn search_returns_message_id_snippet_and_all_mode(pool: sqlx::PgPool) {
        // seed: 2 агента, по сессии, по сообщению с уникальным словом
        for (agent, word) in [("A", "квантовый"), ("B", "квантовый")] {
            let sid = uuid::Uuid::new_v4();
            sqlx::query("INSERT INTO sessions (id, agent_id, user_id, channel, title) VALUES ($1,$2,'u','ui','Тестовая сессия')")
                .bind(sid).bind(agent).execute(&pool).await.unwrap();
            sqlx::query("INSERT INTO messages (session_id, agent_id, role, content) VALUES ($1,$2,'user',$3)")
                .bind(sid).bind(agent).bind(format!("обсуждаем {word} компьютер")).execute(&pool).await.unwrap();
        }
        // per-agent: только A
        let r = crate::db::sessions::search_messages(&pool, Some("A"), "квантовый", 10).await.unwrap();
        assert_eq!(r.len(), 1);
        assert_ne!(r[0].message_id, uuid::Uuid::nil());
        assert_eq!(r[0].agent_id, "A");
        assert_eq!(r[0].session_title.as_deref(), Some("Тестовая сессия"));
        assert!(r[0].snippet.contains("<b>"), "ts_headline russian must highlight the stemmed match: {}", r[0].snippet);
        // all-режим: оба
        let all = crate::db::sessions::search_messages(&pool, None, "квантовый", 10).await.unwrap();
        assert_eq!(all.len(), 2);
        // секция сессий по title
        let hits = crate::db::sessions::search_session_titles(&pool, Some("A"), "тестов", 10).await.unwrap();
        assert_eq!(hits.len(), 1);
    }
```

(Путь `migrations` подогнать по фактическому расположению теста; если тесты кладутся в opex-db — `"../../migrations"`.)

- [ ] **Step 2: RED на сервере** — bundle→clone→`cargo test -p opex-core --bin opex-core search_returns -- --nocapture` (или `-p opex-db`, где лёг тест). Expected: FAIL (нет полей/функции).

- [ ] **Step 3: Реализация opex-db**

`search_messages`: параметр `agent_id: Option<&str>`; SQL FTS-ветки:

```sql
SELECT m.id AS message_id, m.content, s.id AS session_id, s.title AS session_title,
       s.agent_id, s.user_id, s.channel, m.role, m.created_at,
       ts_rank_cd(m.tsv, plainto_tsquery('russian', $2))::float8 AS rank,
       ts_headline('russian', m.content, plainto_tsquery('russian', $2),
                   'MaxWords=18, MinWords=8, ShortWord=2') AS snippet
FROM messages m JOIN sessions s ON m.session_id = s.id
WHERE ($1::text IS NULL OR s.agent_id = $1) AND m.tsv @@ plainto_tsquery('russian', $2)
ORDER BY rank DESC, m.created_at DESC LIMIT $3
```

ILIKE-fallback-ветку (F114, только SQLSTATE 42703) дополнить теми же полями (`snippet` = `left(m.content, 160)`; `0.0 rank`). `SearchResult` += `message_id: Uuid, session_title: Option<String>, agent_id: String, snippet: String`. Новая:

```rust
#[derive(Debug, FromRow)]
pub struct SessionTitleHit {
    pub session_id: Uuid,
    pub title: Option<String>,
    pub agent_id: String,
    pub last_message_at: DateTime<Utc>,
}

pub async fn search_session_titles(db: &PgPool, agent_id: Option<&str>, query: &str, limit: i64) -> Result<Vec<SessionTitleHit>> {
    let escaped = query.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
    Ok(sqlx::query_as::<_, SessionTitleHit>(
        "SELECT id AS session_id, title, agent_id, last_message_at FROM sessions \
         WHERE ($1::text IS NULL OR agent_id = $1) AND title ILIKE '%' || $2 || '%' ESCAPE '\\' \
         ORDER BY last_message_at DESC LIMIT $3",
    ).bind(agent_id).bind(&escaped).bind(limit).fetch_all(db).await?)
}
```

- [ ] **Step 4: Handler** — `SessionSearchQuery` += `all: Option<bool>`; правило: `all==Some(true)` → `agent_id=None` в обе функции; иначе `?agent=` обязателен (существующий BadRequest, контракт 2026-05-08 не трогать). Лимиты: messages `limit.unwrap_or(30).min(100)`, sessions 10. Ответ `{"messages": [...с message_id/agent_id/session_title/snippet...], "sessions": [...], "count"}`. Существующее поле `content` в ответе сохранить (обратная совместимость не нужна — UI не вызывал, но content полезен как fallback).

- [ ] **Step 5: GREEN на сервере + clippy** (workspace `-D warnings`). Expected: тест зелёный, старые search-тесты (если есть) обновлены под новую сигнатуру.

- [ ] **Step 6: Commit** — `git add crates/opex-db/src/sessions.rs crates/opex-core/src/gateway/handlers/sessions.rs && git commit -m "feat(search): message_id, agent-wide mode, russian ts_headline snippet, session-title section"`

### Task 2: SearchPalette + Ctrl+K

**Files:**

- Create: `ui/src/components/chat/SearchPalette.tsx`
- Create: `ui/src/lib/search-api.ts`
- Modify: `ui/src/app/layout.tsx` (хоткей + рендер палитры)
- Modify: `ui/src/components/chat/ShortcutHelp.tsx` (строка Ctrl+K)
- Modify: `ui/src/i18n/locales/en.json`, `ru.json` (ключи `palette.*`)
- Test: `ui/src/__tests__/search-palette.test.tsx`

**Interfaces:**

- Consumes: Task 1 ответ `{messages, sessions}`; `apiGet` из `@/lib/api`.
- Produces: `<SearchPalette />` (самодостаточный: своё open-состояние + подписка на Ctrl+K через zustand-стор `ui/src/stores/palette-store.ts` — создать: `{open, setOpen, target: {sessionId, messageId}|null, setTarget}`); `searchAll(q, {all}): Promise<SearchResponse>` в search-api.ts. Поле `target` потребляет Task 3.

- [ ] **Step 1: Падающий тест** — рендер палитры с замоканным `searchAll`: (а) ввод 1 символа не вызывает API; (б) 2+ символа после debounce вызывает; (в) секции «Сессии»/«Сообщения» рендерятся; (г) стрелки/Enter выбирают результат (spy на onSelect); (д) тогл «по всем» перезапрашивает с `all=true` и рендерит бейджи агентов у ВСЕХ строк; (е) состояние тогла персистится (localStorage mock). Использовать `@testing-library/react` + fake timers по паттерну существующих компонент-тестов.

- [ ] **Step 2: RED** — `cd ui && npx vitest run src/__tests__/search-palette.test.tsx`.

- [ ] **Step 3: Реализация** — `search-api.ts`: `apiGet<SearchResponse>(`/api/sessions/search?q=${encodeURIComponent(q)}&${all ? "all=true" : `agent=${encodeURIComponent(agent)}`}&limit=30`)`. Палитра на `Dialog` (`components/ui/dialog.tsx`): input сверху, debounce 250мс, список с keyboard-nav по паттерну `command-autocomplete.tsx` (индекс + ArrowUp/Down/Enter/Esc), секции заголовками, сниппет: разбить строку по `<b>`/`</b>` маркерам и отрендерить `<mark className="bg-primary/20 text-foreground rounded-sm">` (токены!), НИКАКОГО dangerouslySetInnerHTML. Тогл «по всем» — Switch + `localStorage("palette_all_agents")`. Пустой запрос — пока пустое состояние (Task 7 добавит избранное). Ctrl+K/Cmd+K листенер в layout.tsx (`e.key.toLowerCase()==="k" && (e.ctrlKey||e.metaKey)` → preventDefault + setOpen(true)); `<SearchPalette />` рендерится в layout рядом с toaster'ом.

- [ ] **Step 4: GREEN + tsc** — `cd ui && npx vitest run && npx tsc --noEmit`.

- [ ] **Step 5: Commit** — явные пути, `feat(ui): Ctrl+K search palette with agent-wide toggle and FTS snippets`

### Task 3: Механизм перехода-к-сообщению (ветки, догрузка, подсветка)

**Files:**

- Create: `ui/src/app/(authenticated)/chat/hooks/use-scroll-to-message.ts`
- Modify: `ui/src/stores/palette-store.ts` (поле target из Task 2)
- Modify: `ui/src/app/(authenticated)/chat/MessageList.tsx` (проброс `virtuosoRef`-скролла + data-подсветка)
- Modify: `ui/src/app/(authenticated)/chat/ChatThread.tsx` (вызов хука)
- Modify: `ui/src/i18n/locales/*.json` (`palette.too_deep`, `palette.blocked_streaming`)
- Test: `ui/src/app/(authenticated)/chat/hooks/__tests__/use-scroll-to-message.test.tsx`

**Interfaces:**

- Consumes: `palette-store.target`; `getCachedRawMessages(sessionId, agent)` и `resolveActivePath(rows, branches)` из `@/stores/chat-history`; `loadPreviousMessages(agent)` store-action; `selectedBranches` в AgentState; `virtuosoRef.scrollToIndex`.
- Produces: хук `useScrollToMessage(agent, activeSessionId)` — сам потребляет/очищает target; store-мутация `setSelectedBranch(agent, parentMessageId, childId)` если её нет (проверить существующий `switchBranch` в navigation.ts — реиспользовать его сигнатуру).

**Алгоритм (политика веток — из спеки, дословно):**
1. Если стрим активен (`isActivePhase`) — toast `palette.blocked_streaming`, target очистить.
2. rows = `getCachedRawMessages(sessionId, agent)`. Если `messageId` ЕСТЬ в rows: построить путь от messageId подъёмом по `parent_message_id` до корня; на каждой развилке (родитель с >1 детей) выставить `selectedBranches[parentId] = childOnPath` (через `switchBranch`/эквивалент); дождаться пересчёта activePath; поднять `renderLimit` до `max(renderLimit, activePath.length)`; `scrollToIndex` на индекс сообщения в отрендеренном списке; подсветка 2с (state `highlightedMessageId` в palette-store, MessageItem рендерит класс `ring-2 ring-primary/40 transition-opacity duration-1000` по совпадению id — токены).
3. Если messageId НЕТ в rows: `loadPreviousMessages(agent)` (одна страница), повторить (2); максимум 20 итераций; исчерпали и есть ещё история — toast `palette.too_deep`; истории больше нет и id не найден — тоже toast (сообщение могло быть удалено).
4. Восстановление scroll-позиции (Task 13) использует тот же хук с флагом `silent: true` — без подсветки и toast'ов (тихие fallback'и).

- [ ] **Step 1: Падающий тест** — стор с 2 ветками (родитель → дети A/B; активная A, target на ветке B): хук переключает `selectedBranches` на B и вызывает scrollToIndex (мок virtuosoRef); второй кейс: target отсутствует во всех rows и `hasMoreHistory=false` → toast + target очищен; третий: silent-режим не зовёт toast.

- [ ] **Step 2: RED.** — `cd ui && npx vitest run .../use-scroll-to-message.test.tsx`

- [ ] **Step 3: Реализация** по алгоритму. Подъём по дереву:

```ts
function pathToRoot(rows: RawMessage[], id: string): Map<string, string> {
  // parentId -> childId выборы вдоль пути от корня к target
  const byId = new Map(rows.map((r) => [r.id, r]));
  const picks = new Map<string, string>();
  let cur = byId.get(id);
  while (cur?.parent_message_id) {
    picks.set(cur.parent_message_id, cur.id);
    cur = byId.get(cur.parent_message_id);
  }
  return picks;
}
```

- [ ] **Step 4: GREEN + полный vitest + tsc.**

- [ ] **Step 5: Commit** — `feat(ui): branch-aware jump-to-message with paged backfill and highlight`

### Task 4: Проводка палитра → навигация (same-agent и cross-agent)

**Files:**

- Modify: `ui/src/components/chat/SearchPalette.tsx` (onSelect)
- Modify: `ui/src/app/(authenticated)/chat/hooks/use-scroll-to-message.ts` / ChatThread — потребление target ПОСЛЕ загрузки первой страницы истории
- Test: расширение `search-palette.test.tsx`

**Interfaces:**

- Consumes: Task 2 палитра, Task 3 target-механика, `selectSession` store-action, router push `/chat?agent=X&s=Y`.

- [ ] **Step 1: Тест** — выбор результата-сообщения того же агента: вызван `selectSession` + target выставлен; выбор результата другого агента: `router.push("/chat?agent=B&s=...")` + target выставлен (потребится после загрузки истории — эффект хука срабатывает только когда `sessionMessagesData` резолвнут и `activeSessionId === target.sessionId`, это уже заложено в хук Task 3 — здесь тест-фиксация).
- [ ] **Step 2: RED → реализация onSelect (сессия-результат = selectSession без target; сообщение-результат = selectSession/push + setTarget) → GREEN + tsc.**
- [ ] **Step 3: Commit** — `feat(ui): palette result navigation incl. cross-agent jump`

### Task 5: Гейт и деплой W2-A

- [ ] **Step 1:** `cd ui && npx tsc --noEmit && npx vitest run && npm run build`; сервер: bundle→`~/wip-test-w2`→полные тесты opex-core BIN + clippy workspace; `make gen-types` diff пуст.
- [ ] **Step 2:** Push (СПРОСИТЬ владельца) + деплой парой.
- [ ] **Step 3: E2E:** Ctrl+K с /chat и с /settings; поиск русского слова со спецсимволами (`кот %_\`); переход к сообщению на НЕАКТИВНОЙ ветке длинной сессии (создать edit-ветку заранее); переход к глубокому сообщению (>100); toggle «по всем» + кросс-агентный переход; `make logs`-эквивалент чист.

---

## Батч W2-B — букмарки

### Task 6: Миграция + bookmark API + MessageRow.bookmarked_at + gen-types

**Files:**

- Create: `migrations/0NN_message_bookmarks.sql` (NN = следующий свободный; проверить `ls migrations/ | tail -3`)
- Modify: `crates/opex-core/src/gateway/handlers/sessions.rs` (routes + 2 handler'а)
- Modify: `crates/opex-db/src/sessions.rs` (`get_messages_page` ~2048-2125 оба SELECT'а, `MessageRow` ~733, новые `toggle_bookmark`/`list_bookmarked`)
- Modify: `ui/src/types/api.generated.ts` (через `make gen-types` — НЕ руками)
- Test: sqlx-тесты рядом с Task 1

**Interfaces:**

- Produces: `PATCH /api/messages/{id}/bookmark?agent=` тело `{"bookmarked": bool}` → 204/404; `GET /api/messages/bookmarked?agent=&all=&limit=` → `{"items":[{message_id, session_id, session_title, agent_id, preview, role, bookmarked_at}]}`; `MessageRow.bookmarked_at: Option<DateTime<Utc>>`; Rust-хелпер `text_preview(content: &str, max_chars: usize) -> String` (извлечение текстовых частей из JSON-массива частей или plain-текста; плейсхолдеры «изображение»/«вложение»; обрезка `chars().take()` — НЕ байтовый срез).

- [ ] **Step 1: Миграция**

```sql
-- Message bookmarks (wave 2): NULL = not bookmarked.
ALTER TABLE messages ADD COLUMN bookmarked_at TIMESTAMPTZ;
CREATE INDEX idx_messages_bookmarked ON messages (bookmarked_at DESC) WHERE bookmarked_at IS NOT NULL;
```

- [ ] **Step 2: Падающие sqlx-тесты** — (а) toggle: INSERT сессия+сообщение агента A → `toggle_bookmark(db, msg_id, "A", true)` → rows_affected 1, `bookmarked_at` не NULL; агент B → 0 rows (handler мапит в 404); повторный toggle false → NULL; (б) list: 2 букмарка A + 1 у B → `list_bookmarked(db, Some("A"), 20)` = 2, `list_bookmarked(db, None, 20)` = 3; превью мультимодального content (вставить content = `[{"type":"text","text":"привет мир"},{"type":"file","url":"..."}]` как строку) содержит «привет» и НЕ содержит `{"type"`; content без текстовых частей → плейсхолдер.

- [ ] **Step 3: RED на сервере → реализация**

```rust
pub async fn toggle_bookmark(db: &PgPool, message_id: Uuid, agent_id: &str, on: bool) -> Result<u64> {
    let r = sqlx::query(
        "UPDATE messages SET bookmarked_at = CASE WHEN $3 THEN now() ELSE NULL END \
         WHERE id = $1 AND session_id IN (SELECT id FROM sessions WHERE agent_id = $2)",
    ).bind(message_id).bind(agent_id).bind(on).execute(db).await?;
    Ok(r.rows_affected())
}
```

`list_bookmarked`: JOIN sessions (орфаны исчезнувших сессий отпадают сами), `WHERE m.bookmarked_at IS NOT NULL AND ($1::text IS NULL OR s.agent_id=$1) ORDER BY m.bookmarked_at DESC LIMIT $2`; превью через `text_preview(&content, 160)` в handler'е (serde_json::from_str → массив частей → собрать `text`-части; ошибка парса = plain-текст; пусто → по первой не-текстовой части плейсхолдер). Роуты: `.route("/api/messages/bookmarked", get(api_list_bookmarked))` СТРОКОЙ ВЫШЕ `/api/messages/{id}` + `.route("/api/messages/{id}/bookmark", patch(api_toggle_bookmark))`. `get_messages_page`: `bookmarked_at` в оба SELECT-списка + `MessageRow` (ts-экспортирован!).

- [ ] **Step 4: GREEN на сервере + clippy + `make gen-types`** → диф `api.generated.ts` (+`bookmarked_at`) закоммитить вместе с задачей.

- [ ] **Step 5: Commit** — `feat(bookmarks): bookmarked_at column, toggle/list API with IDOR guard, MessageRow field + gen-types`

### Task 7: UI букмарок — звёздочка + секция «Избранное» в палитре

**Files:**

- Modify: `ui/src/app/(authenticated)/chat/MessageActions.tsx` (BookmarkButton)
- Modify: `ui/src/components/chat/SearchPalette.tsx` (пустой запрос → избранное)
- Modify: `ui/src/lib/search-api.ts` (`listBookmarked`, `toggleBookmark`)
- Modify: `ui/src/stores/chat-types.ts` / `chat-history.ts` — пробросить `bookmarked_at` из RawMessage в ChatMessage (поле `bookmarkedAt?: string|null`)
- Modify: `ui/src/i18n/locales/*.json`
- Test: `ui/src/__tests__/bookmarks.test.tsx`

**Interfaces:**

- Consumes: Task 6 API; Task 3 переход (клик по избранному = setTarget); Task 2 палитра.

- [ ] **Step 1: Падающий тест** — (а) звёздочка: optimistic-переключение иконки до резолва fetch (mock), откат при 404; (б) палитра с пустым запросом рендерит секцию «Избранное» из мока `listBookmarked` с превью и бейджем агента в all-режиме; (в) клик по элементу вызывает setTarget; (г) элемент с исчезнувшей сессией (клик → API selectSession падает 404) → toast «сессия удалена», навигации нет.
- [ ] **Step 2: RED → реализация** (Bookmark/BookmarkCheck иконки lucide; кнопка в обоих режимах showReload — рядом с Copy; optimistic через локальный state + инвалидация messages-query) → **GREEN + tsc**.
- [ ] **Step 3: Commit** — `feat(ui): bookmark star + palette favourites section`

### Task 8: Гейт и деплой W2-B

- [ ] Как Task 5: полный UI-гейт + серверные тесты + gen-types diff пуст → push (СПРОСИТЬ) → деплой парой → E2E: звёздочка переживает reload; пустая палитра показывает избранное; букмарк на неактивной ветке переходит корректно; `curl PATCH .../bookmark` с чужим `?agent=` → 404.

---

## Батч W2-C — пагинация сайдбара

### Task 9: Keyset-курсор в api_list_sessions

**Files:**

- Modify: `crates/opex-core/src/gateway/handlers/sessions.rs` (`SessionsQuery` + `api_list_sessions` ~43-100)
- Modify: `crates/opex-db/src/sessions.rs` (функция листинга — найти по вызову из handler'а)
- Test: sqlx

**Interfaces:**

- Produces: query-параметры `before_last_message_at: Option<DateTime<Utc>>` + `before_id: Option<Uuid>` (оба или ни одного); WHERE `(last_message_at, id) < ($cursor_ts, $cursor_id)` (row-comparison), ORDER BY `last_message_at DESC, id DESC`; ответ прежней формы `{sessions, total}`.

- [ ] **Step 1: Падающий sqlx-тест** — 3 сессии, две с ОДИНАКОВЫМ `last_message_at`: страница limit=2 → вторая страница по курсору из последнего элемента; объединение страниц = все 3 без дублей/потерь (tie-break по id).
- [ ] **Step 2: RED на сервере → реализация → GREEN + clippy.**
- [ ] **Step 3: Commit** — `feat(sessions): keyset pagination on (last_message_at, id)`

### Task 10: useInfiniteQuery + endReached + адаптация потребителей

**Files:**

- Modify: `ui/src/lib/queries.ts` (`useSessions` ~499 → useInfiniteQuery; экспорт плоского селектора `flatSessions(data): SessionRow[]`)
- Modify: потребители формы кэша (ровно три + моки): `ui/src/stores/stream/stream-processor.ts:190` (getQueryData по infinite-форме → искать по pages), `ui/src/app/(authenticated)/chat/page.tsx:68-70`, `ui/src/app/(authenticated)/chat/MessageList.tsx:244`; `SessionSidebar.tsx` (endReached + спиннер + «N из M»); `session-crud.ts` (инвалидации — syncFirstPage-merge по паттерну notifications: `ui/src/lib/queries.ts` секция notifications, скопировать подход)
- Test: `ui/src/__tests__/sessions-infinite.test.ts` + правка ~8 тест-моков (tsc укажет)

**Interfaces:**

- Consumes: Task 9 курсор. Produces: `useSessions(agent)` возвращает `{sessions: SessionRow[] /* уже плоский */, total, fetchNextPage, hasNextPage, isFetchingNextPage}` — обёртка, чтобы потребители получали ПРЕЖНЮЮ плоскую форму и трогать пришлось минимум.

- [ ] **Step 1: Тест** — merge двух страниц без дублей; создание новой сессии → syncFirstPage вставляет её без сброса остальных страниц; `endReached` вызывает fetchNextPage и при активном фильтре (fixture с filter).
- [ ] **Step 2: RED → реализация → полный vitest (моки!) + tsc → GREEN.**
- [ ] **Step 3: Commit** — `feat(ui): infinite session list with keyset cursor and stable scroll`

### Task 11: Гейт и деплой W2-C

- [ ] Как Task 5 → E2E: сайдбар догружает до конца (>40 сессий на проде есть); rename не сбрасывает позицию; фильтр+скролл; «N из M» совпадает с реальностью.

---

## Батч W2-D — мелкие фичи

### Task 12: Per-turn model override (Rust, сквозная прокачка)

**Files:**

- Modify: `crates/opex-core/src/gateway/handlers/chat/sse.rs` (`ChatSseRequest` +`model: Option<String>` c `#[serde(default)]`; прокинуть в bootstrap-вызов)
- Modify: `crates/opex-core/src/agent/pipeline/bootstrap.rs` (`BootstrapOutcome`/контекст — поле `turn_model_override`)
- Modify: `crates/opex-core/src/agent/pipeline/execute.rs` + `pipeline/llm_call.rs` (пронести в `CallOptions`)
- Modify: `crates/opex-core/src/agent/providers/mod.rs` (`CallOptions.model_override: Option<String>` + doc) и КАЖДЫЙ провайдер (`providers/openai/*`, `anthropic/request.rs`, `google.rs`, `http.rs`, `claude_cli.rs`): при `Some(m)` подставлять `m` вместо `current_model()` в тело запроса
- Test: юнит-тесты провайдеров (где есть request-builder тесты — например `anthropic/request.rs`, openai) + sqlx/интеграционный на непersистентность

**Interfaces:**

- Produces: `CallOptions { …, pub model_override: Option<String> }`; `ChatSseRequest.model`. НЕ трогает `set_model_override`/`model_overrides` таблицу.

- [ ] **Step 1: Падающие тесты** — (а) в существующем request-builder тесте anthropic/openai: `CallOptions{model_override: Some("test-model"), ..Default::default()}` → тело запроса содержит `"model":"test-model"`; без override — штатная модель; (б) непersистентность: после построения запроса с override `provider.current_model()` не изменился (чистая проверка на структуре, без live LLM).
- [ ] **Step 2: RED на сервере → реализация по цепочке sse.rs → bootstrap → execute → llm_call → CallOptions → провайдеры.** `CallOptions` уже проходит весь путь (см. doc providers/mod.rs:170) — добавляется поле, НЕ новая цепочка. Провайдеры: заменить точку чтения модели на `opts.model_override.as_deref().unwrap_or(&current_model)`.
- [ ] **Step 3: GREEN + clippy на сервере.**
- [ ] **Step 4: Commit** — `feat(core): per-turn model override via CallOptions — no shared-engine mutation`

### Task 13: UI regenerate-с-моделью + превью вложений + scroll-позиция

Три независимые фичи, ТРИ отдельных коммита в одном диспатче.

**Files (13a regenerate):**

- Modify: `ui/src/stores/streaming-renderer.ts` (`sendTurn` → options-объект `{attachments?, userMessageId?, model?}`; 3 вызывателя), `ui/src/stores/chat/actions/stream-control.ts` (`regenerate(opts?)`, `regenerateFrom(id, opts?)`, `forkAndStream` прокидывает `model`), `ui/src/app/(authenticated)/chat/MessageActions.tsx` (сплит-кнопка: ReloadButton + шеврон-дропдаун моделей — источник тот же hook, что у `composer/ModelDropdown.tsx`)
- Test: `ui/src/__tests__/regenerate-model.test.ts` — regenerate с моделью POST'ит `body.model`; следующий обычный send БЕЗ model; ОБА пути (regenerate и regenerateFrom).

**Files (13b превью, D1):** `ui/src/app/(authenticated)/chat/composer/ChatComposer.tsx` (чипы вложений): image-чип = `<ImageLightbox src={content.data} className="h-12 w-12 rounded object-cover" />` (компонент — самотриггер), кнопка удаления — sibling. Test: чип с image рендерит `img`, удаление работает, не-image чип прежний.

**Files (13c scroll-позиция, D2):** Create `ui/src/app/(authenticated)/chat/hooks/use-scroll-memory.ts`: запись `scroll_pos:{sessionId}` (id первого видимого из Virtuoso `rangeChanged`, debounce 500мс, ТОЛЬКО при `!shouldFollow`; LRU-50 — ключи в `scroll_pos_index` массиве), восстановление при открытии НЕ-стримящей сессии через `useScrollToMessage` c `silent:true`, очистка при возврате к низу. Подключение в MessageList (rangeChanged) + ChatThread. Test: запись при detach; restore вызывает target c silent; отсутствующий id → тихо к низу; стрим игнорирует.

- [ ] **Step 1-3:** По каждой фиче: RED → реализация → GREEN; полный `npx vitest run && npx tsc --noEmit` перед каждым коммитом.
- [ ] **Step 4: Коммиты** — `feat(ui): one-off model pick on regenerate`, `feat(ui): composer image attachment thumbnails`, `feat(ui): per-session scroll position memory`

### Task 14: Библиотека промптов

**Files:**

- Modify: `crates/opex-core/src/agent/workspace.rs:27` (`MEMORY_INDEX_EXCLUDE_FILES` += `"prompts.md"`)
- Create: `ui/src/lib/prompts.ts` (парсер + `usePrompts()` React Query по `apiGet<WorkspaceFile>('/api/workspace/prompts.md')`, 404 → пустой список)
- Modify: `ui/src/components/chat/command-autocomplete.tsx` (item-модель `{kind: "command"|"prompt", name, body?}`; секция «Промпты» после команд; промпты рендерятся БЕЗ `/`-префикса; `onPick(item)` вместо `onPick(name)`)
- Modify: `ui/src/app/(authenticated)/chat/composer/ChatComposer.tsx` (потребитель onPick: command — как сейчас; prompt — ЗАМЕНИТЬ текст поля телом, курсор в конец, БЕЗ отправки)
- Modify: `ui/src/app/(authenticated)/chat/ChatWelcomeScreen.tsx` (первые 3 промпта, fallback на статику)
- Test: `ui/src/__tests__/prompts.test.tsx`

**Interfaces:**

- Produces: `parsePrompts(md: string): Array<{title: string, body: string}>` (секции `## Заголовок`, тело до следующего `##`, пустые отбрасываются); `usePrompts(): {prompts, isLoading}`.

- [ ] **Step 1: Падающий тест** — парсер (2 секции; файл без секций → []; пустой → []); выбор промпта в автокомплите заменяет input и НЕ вызывает send (spy); промпт с именем `compact` не перехватывает команду `/compact` (обе строки в списке, выбор команды работает); welcome рендерит 3 промпта, при пустом ответе — статичные подсказки.
- [ ] **Step 2: RED → реализация → GREEN + tsc.** Rust-часть — однострочник в массиве + существующие тесты workspace.rs (если есть на exclude-список — дополнить).
- [ ] **Step 3: Commits** — `feat(core): exclude prompts.md from memory indexing` (+ cargo check локально) и `feat(ui): workspace prompt library in slash menu and welcome screen`

### Task 15: Гейт и деплой W2-D

- [ ] Полный UI-гейт + серверные тесты (Task 12 — Rust) + gen-types diff пуст → push (СПРОСИТЬ) → деплой парой → E2E: regenerate ходом модели X (видно в usage/логах), следующий ход штатной; миниатюра+лайтбокс; scroll-позиция после reload; `## Тест` в workspace/prompts.md появляется в `/`-меню и на welcome; правка prompts.md НЕ создаёт memory_chunks строк (проверить `SELECT count(*) FROM memory_chunks WHERE source LIKE '%prompts.md%'`).

### Task 16: Финальное whole-branch ревью волны 2

- [ ] review-package от начала волны (SHA зафиксировать в ledger при старте) → финальный ревьюер (самая сильная доступная модель) со спекой + ledger'ом Minor-находок → фиксы одним сабагентом → финальный гейт → push/деплой остатка → обновление памяти проекта.

---

## Порядок и зависимости

```text
Task 1 → 2 → 3 → 4 → 5   (W2-A; 3 зависит от 2 только полем target)
Task 6 → 7 → 8            (W2-B; 7 требует палитру из W2-A)
Task 9 → 10 → 11          (W2-C; независим от A/B)
Task 12 → 13 → 14 → 15    (W2-D; 13a требует 12; 13b/13c/14 независимы; 13c требует Task 3)
Task 16 — после всех.
```
