# Дизайн-ревью Web UI — фаза 2: доработка по итогам верификации

**Дата:** 2026-07-02
**Основание:** верификация выполнения [фазы 1](2026-07-02-ui-design-review-plan.md) (~36 коммитов, 58 файлов в `ui/src`).
**Метод верификации:** сверка диффа с планом + прогон живого UI (next dev, Playwright: десктоп 1440 / мобильный 390, клики по реальным кнопкам) + точечные греп-проверки.
**Итог фазы 1:** выполнено ~60–65%. Этапы 1 (контраст) и 5 (дизайн-система) — добротно, местами сверх плана (design-guard lint, SearchInput для 5 страниц); этапы 2–3 — большей частью; **этап 4 (i18n) — наполовину; этап 6 (чат/workspace) — практически не тронут**.

---

## ✅ СТАТУС ВЫПОЛНЕНИЯ ФАЗЫ 2 — 2026-07-03 (ЗАКРЫТО, subagent-driven)

Все блоки A–D реализованы, каждый батч с локальным гейтом (eslint design-guard + vitest + `npm run build`) и ревью. Коммиты: A `c29493d6` · B `51d57f16` · C-chat `d3d9db40` · C-editors `7cdafd12` · C-workspace/monitor `2093572d` · D `6ef00cb6` (+ фикс мока `bfa89b67`). Полный suite **1211 passed / 145 файлов**, build OK, i18n-паритет **en=ru=1218**.

- **Блок A:** A1 `{chars}`→`{{chars}}` + i18n single-brace guard-тест; A2 превью payload ApprovalCard; A3 подтверждение удаления/сброса handler; A4 no-match empty-state + reset на Агентах.
- **Блок B:** B1 CheckpointPanel/ContextBar → `checkpoints.*`; B2 HandlerEditor → `tools.handler_*`; B3 плюрализация `Intl.PluralRules` (обратно-совместимо); B4 офлайн/«Размышление»/Canvas; B5 sentence-case (8 CTA); B6 sweep (drag-overlay, ImageLightbox aria, mention, args-editor).
- **Блок C:** C1 Reasoning-Collapsible (пульс только при стриме); C2 @-меню клавиатура + подавление submit (+ пойман тонкий баг порядка событий); C3 CodeMirror светлая тема (3 редактора); C4 YAML/TOML подсветка (`@codemirror/lang-yaml`+TOML, default plain-text); C5 компактный ContextBar на мобильном; C6 focus-within hover-кнопок; C7 workspace «Сохранить и продолжить» + иконки по типу + мобильный save-фидбек; C8 график (найден inline) Y-ось/focus-тултип/нулевые дни.
- **Блок D:** D1 sweep `text-muted-foreground/NN`→`-subtle` (48 из 114, консервативно); D2 мёртвые токены `--neu-shadow-*`/`--chat-duration-*` удалены; D4 мёртвый портал `#mobile-page-actions` удалён; D5 watchdog «...»→Skeleton; D6 tap-target ≥44px на 9 mobile-кнопках MessageActions.

**Задокументированные исключения (не баги плана):** C8.1 audit-поиск — честно помечен «из загруженных строк» вместо серверного (бэкенд `/api/audit` не имеет search-параметра — это отдельная Rust-правка, вне UI-скоупа). ~~D3 error-banner — уже через `t()`, сырые дампы не выводятся (пункт был неактуален).~~ **Опровергнуто верификацией 2026-07-03** (баннер показывал `<!DOCTYPE html>…`: шаблон шёл через `t()`, но `{{error}}` подставлял сырое тело ответа). Закрыто отдельным фиксом: `extractError()` в `ui/src/lib/api.ts` схлопывает HTML-тела в `HTTP <status>` и обрезает тексты >300 символов (+6 юнит-тестов в `src/lib/__tests__/api-error.test.ts`); чинит и баннеры, и тосты. Проверено вживую: баннер показывает «HTTP 404».

## Подтверждено выполненным — не переделывать

Проверено в рантайме и/или коде:

- Контрастные токены светлой темы: `--success #166534`, `--warning #854d0e`, пары `-foreground`, `--muted-foreground-subtle`, тёмная `--primary-foreground #1c2130` (`globals.css`).
- Pinch-zoom восстановлен: meta viewport без `maximum-scale` (проверено в рендере).
- Двойная мобильная шапка на `/chat/`//`/workspace/` устранена (`pageHasOwnHeader()`; в рантайме — одна шапка, один trigger).
- «Перезапустить ядро» → ConfirmDialog с описанием последствий (клик проверен). Restore бэкапа: `confirmLabel={t("backups.restore")}` — кнопка «Delete» побеждена; тост успеха + per-row «Restoring».
- ConfirmDialog расползся на 18 файлов (integrations, webhooks regenerate, monitor, workspace, secrets...).
- Секреты: CopyableCode + live-region отсчёта.
- ScrollableTabsList с fade-аффордансом (виден на мобильном мониторе); строки аудита адаптированы (`md:w-20`).
- Плейсхолдеры поиска: `agents/skills/memory/logs/audit.search_placeholder`.
- ~20 новых примитивов + миграция 16 страниц; `text-[10/11px]` 149→29; `neu-flat` изжит до rich-card; primary-действие Tools в шапке; аватары/статусные чипы на chart-токенах.

---

## Блок A — быстрые фиксы (доделать в первую очередь)

- [x] **Интерполяция `{chars}`** — `ui/src/i18n/locales/en.json:226` и `ru.json:226` (`chat.tool_show_full`) используют одинарные скобки, движок `t()` заменяет только `{{…}}` (`ui/src/hooks/use-translation.ts:19`). Пользователь видит сырой «Show {chars}K more...». Исправить на `{{chars}}` + линт/тест на одинарные плейсхолдеры в словарях.
- [x] **ApprovalCard: payload скрыт** — `ui/src/components/chat/ApprovalCard.tsx:113` `<Collapsible>` без `defaultOpen`; админ одобряет действие, не видя аргументов. Превью 2–3 строки по умолчанию (стиль `bg-muted/40 rounded p-2 text-xs font-mono` уже в файле, строка 139), Collapsible — для полного JSON.
- [x] **Подтверждение удаления/сброса обработчика** — `ui/src/app/(authenticated)/tools/page.tsx` не импортирует ConfirmDialog: удаление/сброс handler'а по-прежнему мгновенные. Расширить существующий deleteConfirm-флоу на `kind: "handler"`.
- [x] **No-results на Агентах** — фильтр без совпадений даёт пустую страницу (воспроизведено: поиск `zzzzz` → только шапка). EmptyState «ничего не найдено» + кнопка сброса фильтра; проверить то же на Навыках.

## Блок B — i18n (остатки этапа 4)

- [x] **Чекпоинты хардкодом по-русски** — `ui/src/app/(authenticated)/chat/CheckpointPanel.tsx` («Откатить», «Откатить чекпойнт», :120/:161/:163) и `ContextBar.tsx` → namespace `checkpoints.*` в оба словаря, всё через `t()`.
- [x] **HandlerEditor: остатки хардкода** — `ui/src/app/(authenticated)/tools/HandlerEditor.tsx:264` `label="Handler ID"` и соседние строки → ключи `tools.handler_*`.
- [x] **Плюрализация** — `Intl.PluralRules(locale)` в `use-translation.ts` (без новой зависимости), варианты `_one/_few/_many/_other`; заменить «1 sessions», «{{count}} правил(о)».
- [x] **Терминология ru.json**: `nav.canvas` «Холст» vs `canvas.*` «Canvas» (:41 vs :399/:894) — унифицировать; `channels.offline` «оффлайн» (:783) → «офлайн»; `chat.slash_think_*` «Режим думы» (:280-285) → «Размышление» (термин уже используется приложением).
- [x] **EN sentence case** — привести кнопки Title Case → sentence case (сверка по en.json).
- [x] Прогнать grep на оставшиеся хардкод-строки мимо `t()` (кириллица/латиница в JSX): drag-overlay чата, ImageLightbox, error-boundary, aria-подписи.

## Блок C — чат и workspace (этап 6, не начат)

- [x] **Reasoning-блоки** — `ui/src/app/(authenticated)/chat/parts/ReasoningPart.tsx`: всегда развёрнуты, точка `animate-pulse` горит постоянно (:11). Обернуть в Collapsible (паттерн ToolCallPartView): свёрнуто по умолчанию для завершённых, авто-раскрытие при стриме; пульс — только при стриме.
- [x] **Клавиатура @-меню** — `MentionAutocomplete.tsx` имеет `activeIdx`/`aria-selected`, но обработчика клавиш нет: **проверено вживую — ArrowDown+Enter при открытом меню отправляет недописанное сообщение «@» в чат**. Скопировать capture-keydown из SlashMenu (стрелки/Enter/Escape), подавить submit в `ChatComposer` при `mentionQuery !== null`, добавить `aria-activedescendant`.
- [x] **CodeMirror: светлая тема** — `code-editor.tsx:68` и `obsidian-editor.tsx:107` хардкодят `theme={oneDark}`; то же в `tools/HandlerEditor.tsx`. Читать `resolvedTheme` из next-themes (паттерн уже есть в `code-block.tsx`), для light — светлая тема.
- [x] **YAML/TOML подсветка** — `code-editor.tsx:17-27`: `getLangFromFilename` уже распознаёт yaml/toml, но `getExtension` не имеет для них веток и **default возвращает `markdown()`**. Добавить `@codemirror/lang-yaml`, TOML через `StreamLanguage` (legacy-modes), default — plain text.
- [x] **ContextBar на мобильном** — `chat/page.tsx`: ContextBar (:758) внутри `hidden ... lg:flex`-шапки (:753); мобильная шапка (:777, `lg:hidden`) его не содержит. Компактный вариант: бейдж модели + прогресс токенов + триггер чекпоинтов.
- [x] **Фокус-видимость hover-действий** — `MessageActions.tsx:380`, `chat/page.tsx:705`: `md:opacity-0 md:group-hover:opacity-100` без `md:group-focus-within:opacity-100` — Tab ходит по невидимым кнопкам (паттерн уже есть в memory/page.tsx).
- [x] Workspace-хвосты из фазы 1: guard несохранённых изменений без «Сохранить и продолжить»; одна иконка на все типы файлов; постоянная «Удалить папку» в шапке; save-фидбек на мобильном.
- [x] DailyChart (ось Y, тултип на focus/touch, нулевые дни в таймлайне) и честный поиск по аудиту (фильтрует только 50 загруженных строк).

## Блок D — зачистки

- [x] **`text-muted-foreground/NN` sweep** — осталось 109 мест в 31 файле (лидеры: monitor 16, chat/page 13, ChatComposer 11, MessageActions 9): осмысленный текст → `--muted-foreground-subtle`, opacity — только декоративному.
- [x] **Мёртвые токены** — `--neu-shadow-*` (globals.css:72-73, 154-155) и `--chat-duration-*` (:104-106) так и не удалены (новые «seeded»-токены фазы 1 помечены намеренными — их не трогать, но убедиться, что потребители появятся).
- [x] **error-banner: сырые дампы** — `ui/src/components/ui/error-banner.tsx:87` выводит тело ответа как есть (в рантайме — баннеры с `<!DOCTYPE html>...` на каждой странице с упавшим API). Закрыто 2026-07-03 у источника: `extractError()` (`ui/src/lib/api.ts`) — HTML-тела → `HTTP <status>`, длинные тексты обрезаются; JSON `{error}` бэкенда сохраняется.
- [x] **Портал `#mobile-page-actions`** — `(authenticated)/layout.tsx:152` по-прежнему мёртв: подключить через PageHeader/`createPortal` или удалить вместе с разделителем.
- [x] Watchdog-карточки: literal «...» → Skeleton (примитив уже используется соседними вкладками).
- [x] Тач-цели: пройтись по row-действиям (`h-9 md:h-7` / `size-9 md:size-6`) — в фазе 1 не проверено сплошняком.

## Критерии приёмки (как верифицировать)

1. `@` в композере → ArrowDown → Enter **подставляет упоминание**, сообщение не уходит.
2. Reasoning в завершённом сообщении свёрнут, точка статична; при стриме — развёрнут.
3. Workspace в светлой теме: фон редактора совпадает с темой приложения; `.yaml`/`.toml` подсвечены не как Markdown.
4. Обрезанный tool-output показывает «Показать ещё 12K...», не `{chars}`.
5. UI на EN не содержит кириллицы (чекпоинты, диалоги), UI на RU — латиницы вне кода/названий.
6. Pending-approval карточка показывает аргументы без клика.
7. Удаление handler'а спрашивает подтверждение.
8. При упавшем API баннер не содержит `<!DOCTYPE`.
9. Мобильный чат показывает модель/токены; Tab по треду не попадает на невидимые кнопки.
10. Поиск агентов без совпадений показывает empty-state с кнопкой сброса.

По завершении — отметить чекбоксы здесь и соответствующие пункты в [плане фазы 1](2026-07-02-ui-design-review-plan.md).
