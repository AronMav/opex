# Chat Visual Wave 4 — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Визуальная полировка чат-поверхностей: два уровня вторичного текста, подключение объявленных шкал (sidebar/toolbar/z/elevation), точные v4-размеры вместо arbitrary-px, разделение ролей CometLoader, mermaid-синглтон, хвосты прошлых волн.

**Architecture:** Один UI-only деплой-батч, 6 задач. Все file:line в этом плане провалидированы адверсариальным ревью спеки против кода (rev-2). Rust не затрагивается — серверный гейт не нужен. Спека: `docs/superpowers/specs/2026-07-17-chat-visual-wave4-design.md`.

**Tech Stack:** Next.js 16 / Tailwind 4 (dynamic spacing, @utility) / vitest / mermaid 11.

## Global Constraints

- Работа в **master**; push/деплой предодобрены владельцем для этой волны только после финального мини-ревью; никакой Claude-атрибуции.
- НИКОГДА `git add -A` — только явные пути (в дереве бывают чужие файлы параллельной сессии).
- vitest ТОЛЬКО из `ui/`; никаких cargo-команд (Rust не трогается).
- W4-1 — осознанный re-leveling (НЕ zero-delta); исключения `/* tone-exception: <причина> */`, бюджет ~10.
- Тени → существующая `@utility shadow-elev-*` (НЕ `shadow-[var(...)]`); размеры → точные v4-утилиты (нулевой дрейф); z-шкала НЕ применяется к sticky-триаде (page.tsx:358 z-10 / :382 z-20 / layout.tsx:150 z-30) и MentionAutocomplete:71 z-50.
- Mermaid: маппинг light→"neutral" СОХРАНЯЕТСЯ; полный набор опций (startOnLoad:false, securityLevel:"strict", flowchart{...}).
- `data-testid="streaming-cursor"` сохраняется (переносится на caret).
- Форма персистируемых query не меняется — buster НЕ бампать.
- Гейт каждой задачи: `cd ui && npx tsc --noEmit && npx vitest run`; финальный: + `npx eslint .` + `npm run build`.

---

### Task 1: ESLint-scope + нормализация серых прозрачностей (W4-1)

**Files:**

- Modify: `ui/eslint.config.mjs` (~47-70 — блок scope правила `no-raw-design-values`)
- Modify: все файлы охвата с `text-muted-foreground/30|/50|/60` (инвентаризация: /30×2, /50×30, /60×8 в `ui/src/components/chat/**`, `ui/src/app/(authenticated)/chat/**`, названных ui-примитивах, SearchPalette.tsx)
- Test: существующий полный vitest (рендер-тесты не ассертят классы прозрачностей — проверить grep'ом и обновить, если найдутся)

**Interfaces:**

- Produces: правило `no-raw-design-values` дополнительно покрывает `src/components/chat/**`; в охвате не остаётся `text-muted-foreground/<n>` кроме tone-exception-мест.

- [ ] **Step 1: Расширить ESLint-scope** — в `eslint.config.mjs` в files-массив блока правила добавить `"src/components/chat/**/*.{ts,tsx}"`. Запустить `cd ui && npx eslint src/components/chat --quiet` — зафиксировать список НОВЫХ нарушений (ожидаются px/rem-arbitrary из Task 2 — их чинит Task 2; если всплывёт что-то вне Task 2 — чинить здесь же по духу правила).
- [ ] **Step 2: Механическая замена** по таблице спеки: `/30`, `/50` → `text-muted-foreground-subtle`; `/60` → `text-muted-foreground`; hover-пары → `…-subtle hover:text-muted-foreground`. Grep-развёртка: `cd ui && grep -rn "text-muted-foreground/" src/components/chat src/app/\(authenticated\)/chat src/components/ui/{markdown,code-block,mermaid-block,message,citation-tooltip,card-registry,rich-card,loader}.tsx src/components/chat/SearchPalette.tsx` (пути уточнить под фактическую структуру; SearchPalette лежит в components/chat). Каждое место — глазная оценка: если solid-subtle явно слишком плотен для роли (timestamp и т.п.) — оставить alpha с `/* tone-exception: ... */` (бюджет ~10).
- [ ] **Step 3: bg-muted — только точные дубликаты.** `/20`,`/30`,`/50` НЕ сводить между собой (спека rev-2); менять только если два соседних элемента одного назначения используют разные значения без причины — тогда к значению большинства, с упоминанием в отчёте.
- [ ] **Step 4: Гейт** — `cd ui && npx eslint . && npx tsc --noEmit && npx vitest run` (обновить тесты, если какие-то ассертили старые классы). Expected: зелёно; `grep -rn "text-muted-foreground/" <охват>` возвращает только tone-exception строки.
- [ ] **Step 5: Commit** — `git add <явные файлы>` → `feat(ui): two-level muted-text semantics across chat surfaces + eslint scope`

### Task 2: Шкалы, размеры, тени, z-index (W4-2)

**Files (точные места из ревью спеки):**

- Modify: `ui/src/app/(authenticated)/chat/page.tsx:351` (w-70), `:358` (h-14; z-10 НЕ трогать)
- Modify: `ui/src/app/(authenticated)/workspace/page.tsx:398` (h-14), `:407` (md:w-70)
- Modify: `ui/src/app/(authenticated)/layout.tsx:150` (h-14; z-30 НЕ трогать)
- Modify: `ui/src/app/(authenticated)/chat/composer/ChatComposer.tsx:528` (тени), `:539` (drag-оверлей z → `z-[var(--z-overlay)]`)
- Modify: `ui/src/components/chat/ToolCallPartView.tsx:81,86,219` (max-h-[300px]→max-h-75), `:194` (max-h-[150px]→max-h-37.5)
- Modify: `ui/src/components/chat/ApprovalCard.tsx:179` (max-h-[200px]→max-h-50)
- Modify: `ui/src/components/chat/ApprovalArgsEditor.tsx:61` (min-h-[120px]→min-h-30, max-h-[300px]→max-h-75)
- Modify: `ui/src/components/ui/mermaid-block.tsx:104` (min-w-[3rem]→min-w-12), `:142` (inline maxHeight:"400px"→класс max-h-100), `:151` (min-h-[120px]→min-h-30); фуллскрин-оверлей mermaid (если есть z-литерал) → `z-[var(--z-modal)]`
- Modify: `ui/src/components/ui/code-block.tsx:37` (min-h-[44px]→tap-target)
- Modify: `ui/src/components/chat/ShortcutHelp.tsx:38` (min-w-[24px]→min-w-6)
- Modify: `ui/src/components/chat/ImageLightbox.tsx` (оверлей z-литерал → `z-[var(--z-modal)]`, если это слой UI; `min-w-[3ch]`:111 и `max-h-[90dvh]`:146 НЕ трогать)

**Interfaces:**

- Produces: в охвате нет px/rem-arbitrary (dvh/ch/min() остаются); `shadow-elev-2` в композере; `w-[var(--sidebar-w)]`/`h-[var(--toolbar-h)]` в 5 точках.

- [ ] **Step 1: Замены по списку выше.** Тени: `shadow-black/8` → `shadow-elev-2`, `focus-within:shadow-primary/8` → `focus-within:shadow-elev-2` (если фокус-वариант выглядит хуже на скриншоте — оставить focus-вариант как был с tone-exception комментарием). НЕ трогать: sticky-триаду z-10/20/30, MentionAutocomplete z-50, `ClarifyCard:78 shadow-primary/30` (glow), все dvh/ch/min() значения. Каждому нетронутому «сомнительному» z — комментарий `/* local stacking, not layered UI */` там, где рука тянулась поменять.
- [ ] **Step 2: Гейт** — `cd ui && npx eslint . && npx tsc --noEmit && npx vitest run`. Expected: зелёно; `grep -rn "max-h-\[[0-9]" <охват>` пуст; `grep -rn "shadow-black/8" ui/src` пуст.
- [ ] **Step 3: Commit** — `feat(ui): wire declared sidebar/toolbar/z/elevation scales, exact v4 sizes in chat surfaces`

### Task 3: Роли лоадеров (W4-3)

**Files:**

- Modify: `ui/src/components/ui/loader.tsx` (+`StreamingCaret`, +`PartSkeleton`)
- Modify: `ui/src/app/(authenticated)/chat/parts/TextPart.tsx` (инлайн-caret при streaming)
- Modify: `ui/src/app/(authenticated)/chat/MessageList.tsx:405-409` (удалить блочный курсор)
- Modify: `ui/src/app/(authenticated)/chat/MessageItem.tsx:76-77` (EmptyPartView → PartSkeleton)
- Test: `ui/src/__tests__/message-list.test.tsx` (:287 и курсор-ассерты), `ui/src/app/(authenticated)/chat/parts/__tests__/text-part.test.tsx` (+caret-кейсы)

**Interfaces:**

- Produces: `StreamingCaret()` и `PartSkeleton()` в loader.tsx; `TextPart` получает/использует существующий `streaming`-проп (проверить фактическое имя пропа в text-part — он уже есть для auto-expand reasoning-логики соседнего файла; если у TextPart пропа нет — добавить `streaming?: boolean` и прокинуть из renderAllParts в MessageItem, который уже знает `!isComplete`).

- [ ] **Step 1: Падающие тесты**

```tsx
// в text-part.test.tsx
it("renders inline streaming caret while streaming", () => {
  render(<TextPart text="Привет" streaming />);
  expect(screen.getByTestId("streaming-cursor")).toBeInTheDocument();
});
it("no caret when complete", () => {
  render(<TextPart text="Привет" />);
  expect(screen.queryByTestId("streaming-cursor")).toBeNull();
});
```

(Сигнатуру рендера подогнать под фактические пропсы TextPart; ассерты — как выше.) В message-list.test.tsx: тест :287 переориентировать — sr-only «Loading»-лейбл остаётся, ассерт «внутри CometLoader» заменить на ассерт skeleton-элемента (`data-testid="part-skeleton"`); блочный streaming-cursor-тест (если ассертит расположение под сообщением) — обновить на отсутствие блочного варианта.

- [ ] **Step 2: RED** — `cd ui && npx vitest run "src/app/(authenticated)/chat/parts/__tests__/text-part.test.tsx" src/__tests__/message-list.test.tsx`
- [ ] **Step 3: Реализация**

```tsx
// loader.tsx — дополнение
/** Inline blinking caret shown at the end of actively streaming text.
 *  Deliberately NOT the CometLoader: the comet means "thinking", the caret
 *  means "text is arriving right here". */
export function StreamingCaret() {
  return (
    <span
      data-testid="streaming-cursor"
      aria-hidden="true"
      className="ml-0.5 inline-block h-[1em] w-[2px] translate-y-[0.125em] rounded-sm bg-primary/70 animate-pulse"
    />
  );
}

/** Quiet placeholder for an assistant part that exists but has no content yet. */
export function PartSkeleton() {
  return (
    <div data-testid="part-skeleton" className="py-1">
      <span className="sr-only">Loading</span>
      <div className="h-3 w-24 animate-pulse rounded bg-muted/50" />
    </div>
  );
}
```

TextPart: в конец отрендеренного markdown-контейнера при `streaming` дописывается `<StreamingCaret />` (последний inline-элемент; если markdown-рендер блочный — caret отдельным inline-спаном сразу после контейнера, визуально у конца последней строки — проверить глазами на живом стриме в Task 6). MessageList:405-409 — блок удалить. MessageItem EmptyPartView → `<PartSkeleton />` (sr-only Loading сохраняется внутри скелетона — тест :287 продолжает находить лейбл).

- [ ] **Step 4: GREEN + полный vitest + tsc.** `streaming-perf.test.ts:252` (мок CometLoader→null) не ломается — проверить.
- [ ] **Step 5: Commit** — `feat(ui): split loader roles — comet=thinking, inline caret=streaming, skeleton=empty part`

### Task 4: Mermaid-синглтон (W4-4)

**Files:**

- Create: `ui/src/lib/mermaid-singleton.ts`
- Modify: `ui/src/components/ui/mermaid-block.tsx:22-59`
- Test: `ui/src/__tests__/mermaid-singleton.test.ts` (создать)

**Interfaces:**

- Produces: `getMermaid(resolvedTheme: "light" | "dark"): Promise<typeof import("mermaid").default>`.

- [ ] **Step 1: Падающий тест**

```ts
import { describe, it, expect, vi, beforeEach } from "vitest";
const initialize = vi.fn();
vi.mock("mermaid", () => ({ default: { initialize, render: vi.fn() } }));

describe("mermaid singleton", () => {
  beforeEach(() => { vi.resetModules(); initialize.mockClear(); });
  it("initializes once per theme, remaps light->neutral", async () => {
    const { getMermaid } = await import("@/lib/mermaid-singleton");
    await getMermaid("light");
    await getMermaid("light"); // второй блок той же темы
    expect(initialize).toHaveBeenCalledTimes(1);
    expect(initialize.mock.calls[0][0]).toMatchObject({ theme: "neutral", securityLevel: "strict", startOnLoad: false });
    await getMermaid("dark");
    expect(initialize).toHaveBeenCalledTimes(2);
    expect(initialize.mock.calls[1][0]).toMatchObject({ theme: "dark" });
  });
  it("single-flight: parallel calls initialize once", async () => {
    const { getMermaid } = await import("@/lib/mermaid-singleton");
    await Promise.all([getMermaid("light"), getMermaid("light"), getMermaid("light")]);
    expect(initialize).toHaveBeenCalledTimes(1);
  });
});
```

- [ ] **Step 2: RED.**
- [ ] **Step 3: Реализация** — `mermaid-singleton.ts`:

```ts
// Single mermaid.initialize per theme. mermaid keeps global config; re-running
// initialize on every block render (the old per-render pattern) is wasted work
// and re-parses. Options below are carried verbatim from mermaid-block.tsx —
// INCLUDING the light->"neutral" mapping (mermaid's own "light" theme renders
// differently and was never used here).
import type mermaidType from "mermaid";

let inited: "light" | "dark" | null = null;
let inflight: Promise<typeof mermaidType> | null = null;

export function getMermaid(resolvedTheme: "light" | "dark"): Promise<typeof mermaidType> {
  if (inited === resolvedTheme && inflight) return inflight;
  inflight = (async () => {
    const mermaid = (await import("mermaid")).default;
    mermaid.initialize({
      startOnLoad: false,
      securityLevel: "strict",
      theme: resolvedTheme === "dark" ? "dark" : "neutral",
      // ВЕСЬ блок flowchart{...} перенести из mermaid-block.tsx:22-59 ДОСЛОВНО
      // (реализатор: скопируй фактические опции, не сокращай)
      flowchart: { /* copy verbatim from mermaid-block.tsx */ },
    });
    inited = resolvedTheme;
    return mermaid;
  })();
  return inflight;
}
```

(Точная форма single-flight — на усмотрение с сохранением семантики теста; блок flowchart копируется дословно.) `mermaid-block.tsx`: убрать собственный initialize; эффект — `const mermaid = await getMermaid(resolvedTheme === "dark" ? "dark" : "light")` → прежний render+DOMPurify.

- [ ] **Step 4: GREEN + полный vitest + tsc.**
- [ ] **Step 5: Commit** — `perf(ui): mermaid initialize singleton per theme`

### Task 5: Хвосты (W4-5)

**Files:**

- Modify: `ui/src/app/(authenticated)/chat/ChatThread.tsx:337-340` (баннер → `rounded-lg border border-primary/30 bg-muted/30 px-3 py-2 text-sm`)
- Modify: `ui/src/stores/chat-types.ts:303` (RU-комментарий → EN: `/** sync_begin.truncated — replay is incomplete (pathological buffer overflow); banner shows until the turn ends. */`)
- Modify: `ui/src/stores/sse-events.ts:62` (mid-file import → в блок импортов сверху)

- [ ] **Step 1: Три правки.** Комментарии/импорт — ноль поведения; баннер — только классы.
- [ ] **Step 2: Гейт** — `cd ui && npx tsc --noEmit && npx vitest run` (тест баннера из волны 1+3 ассертит рендер, не классы — проверить).
- [ ] **Step 3: Commit** — `polish(ui): banner weight parity, EN comment, import placement`

### Task 6: Финальный гейт, скриншоты, мини-ревью, деплой

- [ ] **Step 1: Полный гейт** — `cd ui && npx eslint . && npx tsc --noEmit && npx vitest run && npm run build`. Всё зелёное.
- [ ] **Step 2: Скриншоты ДО/ПОСЛЕ** — Playwright-сессией (авторизация токеном из .auth-token, как в инциденте W2): `/chat` открытая сессия с код-блоком и mermaid, light+dark, 1280px и 390px. «До» = прод (текущий), «после» = локальный `npm run dev`/предпросмотр либо после деплоя. Сложить в `.superpowers/sdd-w4/screenshots/`.
- [ ] **Step 3: Финальное мини-ревью волны** — один ревьюер: дифф волны + скриншоты; вердикт READY.
- [ ] **Step 4: Push + `bash scripts/deploy-ui.sh`** (предодобрено после READY). E2E-смоук: стрим (caret инлайн у конца текста, комета только на thinking, skeleton на пустом парте), mermaid обе темы, сайдбар/тулбар без сдвигов, композер-тень.
- [ ] **Step 5: Ledger + память** — обновить память проекта (волна 4 закрыта, аудит исчерпан).

---

## Порядок

```text
Task 1 → 2 (обе — массовые классы; последовательность обязательна из-за общего ESLint-скоупа)
Task 3, 4, 5 — независимы (последовательно из-за одного рабочего дерева)
Task 6 — финал.
```
