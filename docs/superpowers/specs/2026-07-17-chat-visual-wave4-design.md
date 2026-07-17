# Волна 4: визуальная полировка чат-UI

Дата: 2026-07-17 (ревизия 2 — после адверсариального тех-ревью спеки против кода: 8 поправок внесено)
Статус: одобрено (дизайн-ревью с владельцем; правки ревью внесены)
Источник: архитектурный аудит чат-UI от 2026-07-16, «волна 4» + визуальные миноры ledger'ов волн 1-3/2.

## Контекст

Дизайн-система трёхслойная (`@theme` → `:root`-vars → hex, ESLint `no-raw-design-values`), но в чате: «оттенок серого» задаётся смесью alpha-модификаторов (`/30`×2, `/50`×30, `/60`×8 в охвате — инвентаризация ревью) и семантического токена; объявленные шкалы (`--sidebar-w`, `--toolbar-h`, `--z-30..60`, `--elevation-1..4` + **уже существующие** `@utility shadow-elev-1..4`, globals.css:259, под защитой tokens.test.ts) подключены не везде; `CometLoader` играет три роли; `mermaid.initialize` — на каждый рендер блока (mermaid-block.tsx:22-59, mapping **light→"neutral"**, dark→"dark").

## Цели

1. Два семантических уровня вторичного текста вместо alpha-зоопарка (чат-поверхности) — **осознанный re-leveling**, не zero-delta.
2. Подключить объявленные шкалы; убрать arbitrary-тени и px-размеры (точными v4-утилитами, без дрейфа).
3. Разделить три роли CometLoader.
4. Mermaid-инициализация — синглтон на тему.
5. Визуальные хвосты прошлых волн.

## Не-цели

- Страницы вне охвата; редизайн; изменение поведения; новые размерные токены.
- Автоматический скриншот-дифф (инфра Playwright есть — `ui/playwright.config.ts`, `src/__e2e__/` — но baseline'ов `toHaveScreenshot` нет; проверка = ручные скриншоты, глазами).
- Гард alpha-модификаторов в ESLint (осознанный пропуск: правило их не флагает; дисциплина — ревью).

## Охват файлов

`ui/src/components/chat/**`, `ui/src/app/(authenticated)/chat/**`, `ui/src/components/ui/{markdown,code-block,mermaid-block,message,citation-tooltip,card-registry,rich-card,loader}.tsx`, `SearchPalette.tsx`; шелл-точки подключения шкал: `app/(authenticated)/chat/page.tsx` (w-70:351, h-14:358), `app/(authenticated)/layout.tsx:150` (h-14 мобильного хедера — добавлен в охват по ревью), `app/(authenticated)/workspace/page.tsx` (h-14:398, md:w-70:407). Общие ui-примитивы (dialog, button, …) не трогаются.

## W4-1. Нормализация серых прозрачностей

**Это re-leveling, не zero-delta:** маппинг alpha→solid делает третичный текст немного читаемее (в этом смысл полировки). Гейт приемлемости — скриншоты 4 конфигураций (light/dark × desktop/mobile) до/после, глазами.

| Было (инвентаризация: только эти варианты в охвате) | Становится |
| --- | --- |
| `text-muted-foreground/30` (×2), `/50` (×30) | `text-muted-foreground-subtle` |
| `text-muted-foreground/60` (×8) | `text-muted-foreground` |
| hover-пары | `…-subtle hover:text-muted-foreground` (направление сохранено) |

Исключения — комментарий `/* tone-exception: <причина> */`, бюджет **до ~10** (ревью: /50→solid заметно плотнее; исключения ожидаются среди timestamp'ов и глубоко-третичных мест).

**`bg-muted/*` — СУЖЕНО по ревью:** `/20` и `/30` НЕ сводятся к `/50` (видимое потемнение поверхностей mermaid/approval/checkpoint). Сводятся только точные дубликаты одного намерения в соседних элементах (если найдутся); иначе bg-слой не трогается.

## W4-2. Подключение объявленных шкал

- `w-70`→`w-[var(--sidebar-w)]` (page.tsx:351, workspace/page.tsx:407), `h-14`→`h-[var(--toolbar-h)]` (page.tsx:358, workspace/page.tsx:398, layout.tsx:150). Значения идентичны (280px/56px, v4 dynamic spacing) — визуально ноль. Синтаксис `x-[var(--…)]` проверен прецедентами (select.tsx, AgentEditDialog:978) и проходит ESLint-правило (не матчит `\d+(px|rem)`).
- **Z-index — ЯВНЫЕ ИСКЛЮЧЕНИЯ (по ревью):** триада sticky-хедеров `page.tsx:358 z-10` / `:382 z-20` / `layout.tsx:150 z-30` — намеренный sibling-порядок 10<20<30, НЕ трогать (комментарий `/* local stacking */`); `MentionAutocomplete:71 z-50` — локальный стек над композером, НЕ понижать до z-dropdown:40. Шкала `z-[var(--z-*)]` применяется только к однозначным слоям UI: drag-оверлей композера (`ChatComposer:539` → `--z-overlay`), лайтбокс/mermaid-фуллскрин (→ `--z-modal`), прочие по месту с обоснованием. Сомнение = оставить литерал с комментарием.
- **Тени:** `shadow-black/8` + `focus-within:shadow-primary/8` (ChatComposer:528) → **`shadow-elev-2`** (существующая @utility, НЕ `shadow-[var(...)]` — у той нулевой прецедент; elev-тени имеют dark-варианты). `ClarifyCard:78 shadow-primary/30` — glow по смыслу, остаётся.
- **Размеры — точные v4-утилиты, нулевой дрейф (по ревью, v4 dynamic spacing принимает любые множители):** `max-h-[300px]`→`max-h-75` (ToolCallPartView:81,86,219; ApprovalArgsEditor:61), `max-h-[200px]`→`max-h-50` (ApprovalCard:179), `max-h-[150px]`→`max-h-37.5` (ToolCallPartView:194), `min-h-[120px]`→`min-h-30` (ApprovalArgsEditor:61, mermaid-block:151), inline `maxHeight:"400px"`→класс `max-h-100` (mermaid-block:142), `min-w-[3rem]`→`min-w-12` (mermaid-block:104), `min-h-[44px]`→`tap-target` (code-block:37), `min-w-[24px]`→`min-w-6` (ShortcutHelp:38). НЕ трогать: `dvh`/`ch`/`min(...)`-значения (вьюпорт-относительные: SearchPalette:310, CheckpointPanel:155, ImageLightbox:111,146, MentionAutocomplete:71).
- **ESLint-охват (по ревью):** правило `no-raw-design-values` сейчас покрывает только `src/app/**`; расширить его scope в `eslint.config.mjs` на `src/components/chat/**` и исправить всё, что оно там подсветит (это наша полируемая поверхность). `components/ui/**` — не расширять (будущая итерация).

## W4-3. Разделение ролей CometLoader

Потребители (подтверждено ревью): thinking — `MessageList.tsx:55-67` (ThinkingMessage); streaming-cursor — `MessageList.tsx:405-409` (`data-testid="streaming-cursor"`, отдельный блок `pb-1 pl-12` ПОД сообщением); empty-part — `MessageItem.tsx:76-77` (EmptyPartView при `!hasParts`).

- **Thinking:** остаётся CometLoader.
- **StreamingCaret — монтируется ВНУТРИ текстовой части** (по ревью: текущий курсор — отдельная строка под сообщением; каретка на своей строке была бы хуже). Реализация: `TextPart` (parts/TextPart.tsx) при `streaming` рендерит caret инлайн в конце текста (`inline-block w-[2px] h-[1em] bg-primary/70 animate-pulse rounded-sm align-baseline`, компонент в loader.tsx). Отдельный блок-курсор в MessageList:405-409 удаляется. `data-testid="streaming-cursor"` переносится на caret (сохранить контракт e2e/тестов).
- **EmptyPartView:** НЕ «ничего» (по ревью: `message-list.test.tsx:287` — изолированный empty-assistant без ThinkingMessage-соседа остался бы без индикатора). Заменяется на минимальный skeleton-бар (`h-3 w-24 rounded bg-muted/50 animate-pulse` + прежний sr-only «Loading»-лейбл — тест :287 продолжает проходить по лейблу, ассерты по комете обновить). Комета уходит из этой роли.
- Тесты: рендер StreamingCaret в TextPart при streaming и отсутствие при complete; ThinkingMessage → CometLoader; EmptyPartView → skeleton + sr-only label; обновить `message-list.test.tsx` (:287 и стриминг-курсор-ассерты); `streaming-perf.test.ts:252` (мок CometLoader) не ломается.

## W4-4. Mermaid-синглтон

- `ui/src/lib/mermaid-singleton.ts`: `getMermaid(resolvedTheme: "light" | "dark")` — ленивый импорт + `initialize` один раз на тему, single-flight промис; повторная инициализация только при смене темы. **Маппинг темы сохраняется: light→"neutral", dark→"dark"** (по ревью — НЕ передавать "light" в mermaid). Полный набор опций переносится как есть: `startOnLoad:false`, `securityLevel:"strict"`, `theme`, весь блок `flowchart{...}`.
- `mermaid-block.tsx`: эффект → `await getMermaid(resolvedTheme)` → рендер; пер-блочный render + DOMPurify без изменений. Известный residual: гонка theme-flip мид-рендер существует и сейчас — не ухудшаем, не чиним (не-цель).
- Тест (mock mermaid): 2 блока → initialize ×1; смена темы → ровно второй вызов; опции содержат neutral/dark соответственно.

## W4-5. Визуальные хвосты

- Баннер replayTruncated (ChatThread:337-340) → вес ReconnectingIndicator, включая цвета: `rounded-lg border border-primary/30 bg-muted/30 px-3 py-2 text-sm` (цветовая пара уточнена по ревью).
- `chat-types.ts:303` RU-комментарий → EN.
- `sse-events.ts:62` mid-file import → к остальным импортам.

## Тестирование и деплой

- TDD для W4-3/W4-4; W4-1/W4-2 — механические замены + полный vitest + расширенный ESLint; гейт `cd ui && npx eslint . && npx tsc --noEmit && npx vitest run && npm run build`.
- Rust не затрагивается; деплой только `deploy-ui.sh`, один батч. Форма персистируемых query не меняется — buster не бампается.
- Ручные Playwright-скриншоты (light/dark × desktop/mobile) до/после — в финальное ревью волны; E2E-смоук: стрим (caret инлайн, комета на thinking, skeleton на пустом парте), mermaid в обеих темах, сайдбар/тулбары без сдвигов.

## Риски

| Риск | Митигация |
| --- | --- |
| Re-leveling серых ломает иерархию точечно | Политика + tone-exception (бюджет ~10) + скриншоты ×4 |
| Расширение ESLint-scope на components/chat подсветит старые нарушения | Это и есть цель — чинится в этой же волне (поверхность наша) |
| Инлайн-caret ломает layout последней строки | `h-[1em] align-baseline` + рендер-тест + глазная проверка стрима |
| Mermaid-гонка при смене темы | Single-flight промис; residual-гонка не ухудшается (задокументировано) |
| Z-замены меняют стекинг | Явный список исключений (sticky-триада, MentionAutocomplete); сомнение = литерал+комментарий |
