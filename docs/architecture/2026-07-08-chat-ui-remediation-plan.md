# Chat UI — план ремедиации (a11y + i18n + контраст + perf)

**Дата:** 2026-07-08
**Область:** `ui/src/app/(authenticated)/chat/`, `ui/src/components/chat/`, `ui/src/stores/chat*`, `ui/src/app/globals.css`, `ui/src/i18n/locales/`
**Метод:** многоагентный аудит (4 параллельных агента: layout, компоненты, state/SSE, a11y/UX) + адверсариальная верификация фактов по реальным файлам + TDD-реализация.
**Итог аудита:** 2 критических, 6 высоких, 7 средних, ~12 низких/перформанс/багфикс проблем.
**Доставка:** один PR, все 26 TDD-циклов.

> **Статус (2026-07-08):** ✅ РЕАЛИЗОВАНО. Все фазы (0–4) выполнены через TDD; верификация зелёная: `vitest` 1263/1263, `tsc --noEmit` чисто, `eslint` 0 ошибок, `next build` (static export) успешен.
>
> **Ключевая поправка при исполнении — M6 (server-side `lang`) невозможен.** План (и E4) предполагали SSR через `next/headers` `cookies()`, но UI собирается как **статический экспорт** (`output: "export"` в `next.config.ts`) — серверных cookies при билде нет, а `cookies()` роняет prerender `/_not-found`. При этом `<LanguageSync>` **уже** синхронизирует `document.documentElement.lang` на клиенте после гидрации. Итог: server-side/cookie-подход откачен, M6 закрыт клиентским тестом `src/components/__tests__/language-sync.test.tsx`.
>
> **Прочие отклонения от буквы плана (эквивалентны по покрытию):** тесты MessageList-a11y и chat-page-a11y размещены в существующих харнессах (`src/__tests__/message-list.test.tsx`, новый `chat-page-a11y.test.tsx`) вместо дублирования 180-строчных моков; `ThreadErrorBoundary` вынесен в отдельный модуль (`ThreadErrorBoundary.tsx`) ради тестируемости без тяжёлых зависимостей ChatThread; `CompressionDivider` использует новый ключ `chat.segment` с `segmentIndex` (без `+1`, для согласованности с видимым текстом). Пути компонентов в плане (`src/components/chat/`) частично реально живут в `src/app/(authenticated)/chat/**` — исправлено по факту.

---

## Контекст и сильные стороны (сохранить)

Кодовая база чата в крепком состоянии: нет TODO/FIXME маркеров в production-коде, продуманная state-архитектура (`streamGeneration`-защита от stale-write, `StreamSession` с 50ms throttle, экспоненциальный backoff с jitter, WeakMap-кеш рендера parts PERF-03). TDD-конвенция уже санционирована (`CheckpointPanel.test.tsx:2`: *"TDD: тест написан до реализации"*). ARIA-combobox для slash/mention-меню, `prefers-reduced-motion` глобально honoured, `tap-target` 44px (WCAG 2.5.5).

Главная проблема — **стримящийся текст ассистента невидим для скринридеров**, плюс несколько кастомных оверлеев без focus-management.

---

## Зафиксированные решения

1. **ImageLightbox** → инлайн-фикс фокуса (ручной focus trap + restore, без миграции на Radix).
2. **Контраст (M1)** → правка дизайн-токенов глобально в `globals.css` + точечная чистка `/30`–`/60` классов.
3. **i18n** → только `ui/src/i18n/locales/ru.json` + `en.json`.
4. **CLAUDE.md (D1)** → в этом же PR.
5. **M6 (server-side `lang`)** → включён с cookie-sync.
6. **TDD-тесты** → все 26 циклов.

---

## Уточнения после верификации (5 исправлений первоначального драфта)

| # | Неверное предположение | Реальность | Действие |
|---|------------------------|------------|----------|
| E1 | i18n в `ui/src/messages/` | `ui/src/i18n/locales/{ru,en}.json` (1255 строк, плоские ключи, `{{var}}`, `_one/_few/_many/_other`) | Поправить пути |
| E2 | `abort-reason-label` рефактор не сломает тест | Тест регексами проверяет английские подстроки; рефактор ломает и тест, и call site `MessageItem.tsx:318` | Обновлять тест + call site одновременно; TDD через stub `t` |
| E3 | `role="log"` на wrapper-диве безопасно | Wrapper содержит ещё `<ScrollToBottomButton>` + вложенные `role="status"` карточки → двойные анонсы | Новый внутренний `<section role="log" aria-relevant="additions">` только вокруг Virtuoso |
| E4 | M6 тривиален | Локаль в localStorage, не в cookie → server-side lang требует cookie-sync | Включён в план (расширенное решение) |
| E5 | Нет contrast-теста | `src/app/__tests__/contrast.test.ts` уже парсит globals.css и считает WCAG-контраст | TDD: расширить assertions → red → правка токена → green |

---

## Контраст-пороги (M1, зафиксированные значения)

| Токен | Light сейчас | Контраст к `--background: #dde3ee` | New Light | Контраст |
|-------|--------------|-----------------------------------|-----------|----------|
| `--muted-foreground` | `#5a6270` | ~4.5:1 (borderline AA) | `#4d5560` | ~6.3:1 ✓ |
| `--muted-foreground-subtle` | `#5e6675` | ~4.3:1 | `#525a66` | ~5.8:1 ✓ |
| Dark `--muted-foreground` | `#8b96a8` | ~6.0:1 ✓ | без изменений | — |
| Dark `--muted-foreground-subtle` | `#9aa4b2` | ~7.0:1 ✓ | без изменений | — |

После правки токенов: `/30` → `/60` (placeholder), `/50` оставить для чисто декоративных иконок, но информативный текст поднять до основного `text-muted-foreground` без alpha.

**Распределение `text-muted-foreground/(30|40|50|60)`:** 36 из 51 матча (71%) — в chat-поверхности, что подтверждает фокус. Breakdown: `/30` — 3 (placeholder, disabled), `/50` — ~22 (б bulk, MessageActions×9), `/60` — ~8.

---

## Новые i18n-ключи (23 шт., плоские)

Добавить в ОБА файла `ru.json` + `en.json` (в `ru.json` первым — иначе `TranslationKey` из `i18n/types.ts:4` не выведется и call site не скомпилируется):

```
chat.abort_reason_max_duration
chat.abort_reason_inactivity
chat.abort_reason_user_cancelled
chat.abort_reason_shutdown_drain
chat.abort_reason_timeout
chat.abort_reason_unknown
chat.abort_reason_default
chat.video_phase_download
chat.video_phase_transcribe
chat.video_phase_digest
chat.video_phase_saving
chat.previous_session
chat.untitled_session
chat.mention_targeting
chat.cache_write
chat.cache_read
chat.reasoning_tokens
chat.segment
chat.title
chat.message_thread
chat.thinking
common.retry
common.agent
common.skip_to_content
```

Конвенция: плоские dot-namespace ключи, `{{var}}` интерполяция (например `"chat.segment": "Сегмент {{current}} из {{total}}"`).

---

# Фазы реализации (TDD)

Каждая задача — Red (тест падает) → Green (минимальная правка) → Refactor.

## Фаза 0 — Фундамент

### 0.1 — i18n-ключи [нет TDD, инфра]
- [ ] `ui/src/i18n/locales/ru.json` + `en.json` — 23 новых ключа

### 0.3 → 0.2 — Контраст токенов [TDD]
- [ ] **Red:** расширить `src/app/__tests__/contrast.test.ts` assertions для `--muted-foreground` ≥5.5:1 против `--card`
- [ ] **Green:** `ui/src/app/globals.css` — `--muted-foreground: #4d5560`, `--muted-foreground-subtle: #525a66` (light)

---

## Фаза 1 — Critical

### 1.1 — C1 + H1: MessageList live region + list semantics [TDD]
- [ ] **Red:** `src/app/(authenticated)/chat/__tests__/message-list-a11y.test.tsx` — `getByRole("log")`, `aria-live="polite"`, `aria-relevant="additions"`, `aria-label` локализован; строки `role="listitem"`
- [ ] **Green:** `MessageList.tsx:267-268` — обернуть `<Virtuoso>` в `<section role="log" aria-live="polite" aria-relevant="additions" aria-label={t("chat.message_thread")}>…</section>`. `<ScrollToBottomButton>` — sibling ВНЕ section. На каждую строку в `itemContent` (`MessageList.tsx:283-344`) — `role="listitem"`

### 1.2 — C2: Textarea aria-label [TDD]
- [ ] **Red:** расширить `composer/__tests__/ChatComposer.mention-keyboard.test.tsx` (или новый `ChatComposer.a11y.test.tsx`) — `getByRole("textbox", {name: /сообщение|message/i})` найден
- [ ] **Green:** `ChatComposer.tsx:710` — `aria-label={t("chat.message_placeholder")}` на `<textarea>`

---

## Фаза 2 — High

### 2.1 — H3: ThinkingMessage live region [TDD]
- [ ] **Red:** тест `getByRole("status")` внутри ThinkingMessage
- [ ] **Green:** `MessageList.tsx:52-58` — обернуть `ThinkingMessage` в `<div role="status" aria-live="polite" aria-label={t("chat.thinking")}><CometLoader/></div>`

### 2.2 — H2: ImageLightbox focus trap + restore [TDD]
- [ ] **Red:** новый `src/components/chat/__tests__/ImageLightbox.focus.test.tsx` — рендер с `open`, assert `document.activeElement` === dialog; `fireEvent.keyDown(dialog,{key:"Escape"})` → `activeElement` === trigger
- [ ] **Green:** `ImageLightbox.tsx:72-135`:
  - `useEffect` при open: `dialogRef.current?.focus()`
  - Tab-ловушка: на `keyDown Tab` вычислить первый/последний tabbable (4 элемента тулбара), зациклить
  - На Escape/close: `triggerRef.current?.focus()`

### 2.3 — H5: Voice-settings popover Escape + focus [TDD]
- [ ] **Red:** расширить composer-тест — открыть поповер, `fireEvent.keyDown(panel,{key:"Escape"})`, assert закрыт
- [ ] **Green:** `ChatComposer.tsx:832-891`:
  - on open: `firstInputRef.current?.focus()`
  - `onKeyDown` Escape на panel
  - on close: `triggerRef.current?.focus()`

### 2.4 — H4: Timestamps focus-fallback [без TDD, className]
- [ ] `MessageItem.tsx:187,302` — `opacity-0 group-hover:opacity-100` → `md:opacity-0 md:group-hover:opacity-100 md:group-focus-within:opacity-100`

### 2.5 — H6: Skip-to-content link [TDD]
- [ ] **Red:** новый `src/app/(authenticated)/__tests__/layout.a11y.test.tsx` — `getByRole("link",{name:/skip/i})`, `toHaveAttribute("href","#main-content")`, `main` имеет `id="main-content"`
- [ ] **Green:** `app/(authenticated)/layout.tsx` перед `<AppSidebar>`:
  ```tsx
  <a href="#main-content" className="sr-only focus:not-sr-only focus:absolute focus:top-2 focus:left-2 focus:z-50 focus:rounded focus:bg-background focus:px-4 focus:py-2 focus:shadow">{t("common.skip_to_content")}</a>
  ```
  На `<main>` (`layout.tsx:162`): `id="main-content" tabIndex={-1}`

### 2.6 — L6 + B1: Error boundary `role="alert"` + onRetry [TDD]
- [ ] **Red:** расширить ChatThread-тест — триггерить throw в child, assert `getByRole("alert")`; клик Retry → вызван `onRetry`
- [ ] **Green:** `ChatThread.tsx:60-73`:
  - `<p role="alert">` вместо `<p>`
  - retry handler: `() => { setState({error:null}); this.props.onRetry?.(); }`

---

## Фаза 3 — Medium остатки

### 3.1 — M2: Sessions list-семантика [TDD]
- [ ] **Red:** тест на `page.tsx` sessions Virtuoso — `findAllByRole("listitem")` найдены
- [ ] **Green:** `page.tsx` — `role="list"` на контейнер Virtuoso, `role="listitem"` на строки

### 3.2 — M3: `<h1>` на chat page [TDD]
- [ ] **Red:** assert `getByRole("heading",{level:1})` найден в layout-рендере
- [ ] **Green:** `<h1 className="sr-only">{t("chat.title")}</h1>` в `app/(authenticated)/layout.tsx`

### 3.3 — M4: CompressionDivider exposes info [TDD]
- [ ] **Red:** обновить `src/components/chat/__tests__/CompressionDivider.test.tsx` — `getByRole("separator")` + `toHaveAttribute("aria-label", /segment/i)`
- [ ] **Green:** `CompressionDivider.tsx:13` — убрать `aria-hidden`, добавить `role="separator" aria-label={t("chat.segment",{current:segmentIndex+1,total:totalSegments})}`

### 3.4a — M5: abort-reason-label i18n refactor [TDD]
- [ ] **Red:** переписать `src/components/chat/__tests__/abort-reason-label.test.ts` — новая сигнатура `abortReasonLabel(reason, t)`, stub `t` возвращает ключ; assert каждый reason → правильный ключ; unknown → `t("chat.abort_reason_unknown",{reason})`
- [ ] **Green:** рефактор `abort-reason-label.ts` → switch возвращает `t(key, params)`; обновить `MessageItem.tsx:318` → `abortReasonLabel(message.abortReason, t)` (получить `t` из `useTranslation` в `MessageItemImpl`)

### 3.4b — M5 + M7: VideoProgressIndicator i18n + live region [TDD]
- [ ] **Red:** обновить `src/components/chat/__tests__/VideoProgressIndicator.test.tsx` — мок `use-translation` возвращает ключи, assert `getByRole("status")` + `aria-live="polite"` + текст без emoji в aria
- [ ] **Green:** `VideoProgressIndicator.tsx`:
  - Убрать `PHASE_LABELS`, использовать `t("chat.video_phase_*")`
  - Обернуть в `<div role="status" aria-live="polite">`
  - Визуальные emoji вынести в `<span aria-hidden>📥</span>` + локализованный текст

### 3.4c — M5: точечные i18n fallbacks
- [ ] `ParentBadge.tsx:18` — `parentTitle ?? t("chat.previous_session")`
- [ ] `CompactChainBanner.tsx:66` — `entry.title || t("chat.untitled_session")`
- [ ] `ContextBar.tsx:126-130` — `"↑ cache write:"` → `t("chat.cache_write")` и т.д.
- [ ] `RoleAvatar.tsx:57` — alt fallback `t("common.agent")`
- [ ] `ChatThread.tsx:69` — fallback `"Retry"` → `t("common.retry")`

### 3.5 — L5: Combobox aria-controls [TDD]
- [ ] **Red:** расширить `ChatComposer.mention-keyboard.test.tsx` — при открытом меню assert textarea имеет `aria-controls` → указывает на id listbox; listbox имеет matching `id`
- [ ] **Green:**
  - `MentionAutocomplete.tsx:59` — `const listboxId = useId(); ... id={listboxId}` на контейнере
  - `ChatComposer.tsx:716` — `aria-controls={mentionOpen ? listboxId : undefined}`

### 3.6 — M6: Server-side `lang` через cookie-sync [TDD]
- [ ] **Red 1:** `src/stores/__tests__/language-store.test.ts` — `setLocale("en")` → `document.cookie` содержит `opex.locale=en`
- [ ] **Red 2:** `src/app/__tests__/layout-lang.test.tsx` — мок `next/headers` `cookies()` возвращает `{value:"en"}`, assert `<html lang="en">`
- [ ] **Green 1:** `language-store.ts` — в `setLocale` добавить `document.cookie = \`opex.locale=${locale}; path=/; max-age=31536000; SameSite=Lax\``
- [ ] **Green 2:** `app/layout.tsx` — `import { cookies } from "next/headers"; const locale = cookies().get("opex.locale")?.value === "en" ? "en" : "ru"; <html lang={locale} suppressHydrationWarning>`

---

## Фаза 4 — Perf + cleanup

### 4.1 — P2: `memo()` для ReasoningPart + ClarifyCard [TDD]
- [ ] **Red:** тест рендерит компонент с теми же props дважды, assert внутренний mock-fn вызван 1 раз
- [ ] **Green:** `ReasoningPart.tsx`, `ClarifyCard.tsx` — обернуть в `memo()`

### 4.2 — P1: convertHistory cache [TDD]
- [ ] **Red:** расширить `src/stores/__tests__/chat-history.test.ts` — вызвать `convertHistory(rows)` дважды с тем же массивом, assert второе возвращаемое `===` первое (referential equality)
- [ ] **Green:** `chat-history.ts` — WeakMap<`MessageRow[]`, `ChatMessage[]`> keyed by input reference

### 4.3 — B2: Multi-file drop/paste [TDD]
- [ ] **Red:** расширить `composer/__tests__/ChatComposer.upload-id.test.tsx` — дроп/вставка 2 файлов, assert оба добавлены
- [ ] **Green:** `ChatComposer.tsx:567,591` — цикл по `files` в `handleFileAdd`

### 4.4 — B3: Double-fetch guard [TDD]
- [ ] **Red:** расширить `src/__tests__/message-list.test.tsx` — `startReached` + клик Header-кнопки, assert `onLoadEarlier` вызван 1 раз
- [ ] **Green:** `inFlightRef` в `use-chat-autoscroll.ts` (или `MessageList.tsx`)

### 4.5 — D1: CLAUDE.md update
- [ ] `ContinuationSeparator` → `CompressionDivider`
- [ ] `HandoffDivider` → `AgentTransitionDivider`
- [ ] Удалить `StepGroup`
- [ ] Удалить ссылку на `chat-reconciliation.ts` → `chat-overlay-dedup.ts`
- [ ] chat-store: «451 lines» → «74 lines (decomposed в `chat/actions/`)»
- [ ] Указать реальные пути i18n (`i18n/locales/`, не `messages/`)

---

## Фаза 5 — Верификация

```bash
cd ui && npm test          # все тесты зелёные (включая новые 26)
cd ui && npm run build     # production-сборка (RSC flattening)
cd ui && npx tsc --noEmit  # typecheck (включая новые TranslationKey)
cd ui && npm run lint      # ESLint
```

**Regression-чеки:**
- `contrast.test.ts` проходит с новыми порогами
- `abort-reason-label.test.ts` (обновлённый) проходит со stub `t`
- `CompressionDivider.test.tsx` (обновлённый) проходит с role=separator
- `VideoProgressIndicator.test.tsx` (обновлённый) проходит без emoji в aria-text
- `reconnecting-indicator.test.tsx` — не сломан (родитель role=log не удаляет потомка role=status)

---

# Сводка исполнимых правок

| Категория | Файлов | Тестов |
|-----------|--------|--------|
| Токены/i18n фундамент | 3 (globals.css, ru.json, en.json) | 1 обновлённый (contrast) |
| Production-код | ~17 | — |
| Новые тесты | — | ~14 новых |
| Обновлённые тесты | — | ~12 обновлённых |
| Документация | 1 (CLAUDE.md) | — |

**Итого TDD-циклов:** 26 (Red→Green→Refactor)
**Порядок внутри PR:** 0 → 1 → 2 → 3 → 4 → 5

---

## Риски и митигации

| Риск | Митигация |
|------|-----------|
| `role="log"` + Virtuoso ломает внутренний DOM | Section — внешний wrapper, не `ScrollerComponent`. Virtuoso не инспектирует ARIA на ancestor |
| Focus trap в ImageLightbox через fireEvent | Tab не двигает фокус в jsdom — manual `.focus()` + assert `document.activeElement` |
| Server-side `lang` требует cookie | Расширяем `language-store.setLocale` писать cookie; SSR читает `next/headers` |
| `convertHistory` кеш инвалидируется | WeakMap по `MessageRow[]` reference — RQ возвращает новый массив на refetch, кеш сбрасывается автоматически |
| i18n-рефактор `abort-reason-label` ломает снепшоты | Тест переписывается под stub `t`; call site `MessageItem.tsx` обновляется одновременно |
| Вложенные live regions (role=log + role=status) | `aria-relevant="additions"` на log-секции игнорирует мутации внутри существующих строк |

---

## Что НЕ в этом PR (follow-up)

- **C-уровневый audit остальных страниц** (workspace, monitor, settings) — scope был только chat
- **`jest-axe` автоматизированный a11y-scan** — инфраструктурное расширение, отдельная задача
- **Декомпозиция `ChatComposer.tsx` (938 LoC) и `page.tsx` (962 LoC)** — P4, большой рефакторинг, рискованный для одного PR
- **`@testing-library/user-event` introduction** — конвенция использует fireEvent, миграция отдельная задача

---

## Источники

- Аудит-репорты 4 агентов (layout, components, state/SSE, a11y/UX) — в контексте сессии
- `docs/architecture/2026-07-02-ui-design-review-plan.md` — предыдущий раунд UI-аудита (закрыт 2026-07-03)
- `src/app/__tests__/contrast.test.ts` — существующий WCAG-регрессор
- `ui/src/components/chat/__tests__/abort-reason-label.test.ts` — референс TDD-паттерна для чистых функций
- `ui/src/components/chat/__tests__/CompressionDivider.test.tsx` — референс TDD-паттерна для компонентов с i18n-mock
