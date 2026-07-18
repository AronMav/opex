# Provider Fields Unification — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Единая семья компонентов `ProviderSelect` / `ModelCombobox` / `VoiceSelect` в `ui/src/components/provider-fields/`, заменяющая шесть разных реализаций полей выбора провайдера/модели/голоса.

**Architecture:** Три презентационных компонента поверх существующих эндпоинтов (`/api/providers`, `/api/providers/{id}/models`, `/api/tts/voices`) и React Query-хуков (`useProviders`, `useProviderModelsDetailed`, новый `useTtsVoices`). Бэкенд не меняется. Спека: `docs/superpowers/specs/2026-07-18-provider-fields-unification-design.md`.

**Tech Stack:** Next.js 16 / React 19, TypeScript, TanStack Query 5, shadcn/ui (Select, Input), vitest + @testing-library/react.

## Global Constraints

- Только `ui/` — ни одного изменения в `crates/`, `toolgate/`, `channels/`, `migrations/`.
- Никаких новых npm-зависимостей (в ките нет cmdk/popover — combobox пишется руками на Input + absolute-список).
- Все vitest-команды запускать СТРОГО из каталога `ui/` (готча репо: из корня vitest не работает).
- `TranslationKey` выводится из `ui/src/i18n/locales/ru.json` — каждый новый ключ добавлять И в `ru.json`, И в `en.json` (парность локалей).
- Существующие `data-testid` в ProfileEditor (`profile-slot-*`, `profile-row-*`, `profile-model-*`) сохранить как есть.
- Коммиты без Co-Authored-By и без атрибуции Claude. Работать в `master`, не пушить.
- TDD: в каждой задаче сначала тест, затем реализация.

## Справка: существующие API, на которые опираемся

```ts
// ui/src/lib/queries.ts (уже есть)
export interface ProviderModel {
  id: string
  owned_by?: string
  context_window?: number
  vision?: boolean
  reasoning?: boolean
  reasoning_content?: boolean
  tools?: boolean
}
export function useProviderModelsDetailed(id: string | null) // GET /api/providers/{id}/models, enabled: !!id, staleTime 60s
export function useProviders() // GET /api/providers → Provider[] (поля: id, name, type, provider_type, default_model, …)

// ui/src/components/model-badges.tsx (уже есть)
export function ModelBadges({ m, className }: { m: Pick<ProviderModel, "context_window" | "vision" | "reasoning" | "reasoning_content" | "tools">; className?: string })
```

---

### Task 1: `ModelCombobox`

**Files:**
- Create: `ui/src/components/provider-fields/ModelCombobox.tsx`
- Create: `ui/src/components/provider-fields/index.ts`
- Create: `ui/src/components/provider-fields/__tests__/ModelCombobox.test.tsx`
- Modify: `ui/src/i18n/locales/ru.json` (в конец файла, перед закрывающей `}`)
- Modify: `ui/src/i18n/locales/en.json` (аналогично)

**Interfaces:**
- Consumes: `useProviderModelsDetailed`, `ProviderModel` из `@/lib/queries`; `ModelBadges` из `@/components/model-badges`; `Input` из `@/components/ui/input`.
- Produces (используется задачами 4–7):

```ts
export interface ModelComboboxProps {
  value: string;
  onChange: (value: string) => void;
  /** UUID сохранённого провайдера — список лениво грузится из GET /api/providers/{id}/models при первом открытии. */
  providerId?: string | null;
  /** Статичные подсказки для pre-create потоков (setup-визард, create-формы), когда провайдера ещё нет. Игнорируется, если задан providerId. */
  staticOptions?: string[];
  placeholder?: string;
  disabled?: boolean;
  id?: string;
  className?: string;
  "data-testid"?: string;
}
export function ModelCombobox(props: ModelComboboxProps): JSX.Element
```

- [ ] **Step 1: Добавить i18n-ключи**

В `ui/src/i18n/locales/ru.json` (внутрь корневого объекта, рядом с другими ключами — файл плоский, ключи с точками):

```json
  "fields.model_loading": "Загрузка моделей…",
  "fields.model_list_unavailable": "Список недоступен — введите id модели вручную",
  "fields.model_no_match": "Совпадений нет — значение будет использовано как есть",
  "fields.select_provider_first": "Сначала выберите провайдера",
  "fields.voice_loading": "Загрузка голосов…"
```

В `ui/src/i18n/locales/en.json`:

```json
  "fields.model_loading": "Loading models…",
  "fields.model_list_unavailable": "List unavailable — type the model id manually",
  "fields.model_no_match": "No matches — the value will be used as-is",
  "fields.select_provider_first": "Select a provider first",
  "fields.voice_loading": "Loading voices…"
```

(`fields.voice_loading` добавляется сейчас же, чтобы не трогать локали три раза; используется в Task 3.)

- [ ] **Step 2: Написать падающий тест**

`ui/src/components/provider-fields/__tests__/ModelCombobox.test.tsx`:

```tsx
import React from "react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import "@testing-library/jest-dom/vitest";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";

vi.mock("@/hooks/use-translation", () => ({
  useTranslation: () => ({ t: (key: string) => key, locale: "en" }),
}));

const { apiGet } = vi.hoisted(() => ({ apiGet: vi.fn() }));
vi.mock("@/lib/api", () => ({ apiGet }));

import { ModelCombobox } from "../ModelCombobox";

function wrap(ui: React.ReactElement) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(<QueryClientProvider client={qc}>{ui}</QueryClientProvider>);
}

// Stateful harness for behaviours that depend on `value` updating after
// onChange (the component is CONTROLLED — it filters by its `value` prop, so a
// static vi.fn() mock would leave value="" and the filter would never engage).
function Controlled({ providerId, initial = "" }: { providerId?: string | null; initial?: string }) {
  const [v, setV] = React.useState(initial);
  return <ModelCombobox value={v} onChange={setV} providerId={providerId} data-testid="cb" />;
}

describe("ModelCombobox", () => {
  beforeEach(() => {
    apiGet.mockReset();
  });

  it("does not fetch until opened, then lazily loads models for providerId", async () => {
    apiGet.mockResolvedValue({ models: [{ id: "glm-5.2", context_window: 200000 }, { id: "glm-5-air" }] });
    wrap(<ModelCombobox value="" onChange={vi.fn()} providerId="p1" data-testid="cb" />);

    expect(apiGet).not.toHaveBeenCalled();

    fireEvent.focus(screen.getByTestId("cb"));
    expect(await screen.findByRole("option", { name: /glm-5\.2/ })).toBeInTheDocument();
    expect(apiGet).toHaveBeenCalledWith("/api/providers/p1/models");
  });

  it("clicking an option calls onChange with the model id and closes the list", async () => {
    apiGet.mockResolvedValue({ models: [{ id: "glm-5.2" }, { id: "glm-5-air" }] });
    const onChange = vi.fn();
    wrap(<ModelCombobox value="" onChange={onChange} providerId="p1" data-testid="cb" />);

    fireEvent.focus(screen.getByTestId("cb"));
    fireEvent.mouseDown(await screen.findByRole("option", { name: /glm-5-air/ }));

    expect(onChange).toHaveBeenCalledWith("glm-5-air");
    expect(screen.queryByRole("listbox")).not.toBeInTheDocument();
  });

  it("typing filters the list case-insensitively", async () => {
    // Controlled harness: value must update after onChange for the filter (which
    // reads `value`) to engage — a static mock would leave value="".
    apiGet.mockResolvedValue({ models: [{ id: "glm-5.2" }, { id: "MiniMax-M2.5" }] });
    wrap(<Controlled providerId="p1" />);

    const input = screen.getByTestId("cb");
    fireEvent.focus(input);
    await screen.findByRole("option", { name: /glm-5\.2/ });
    fireEvent.change(input, { target: { value: "minimax" } });

    expect(input).toHaveValue("minimax"); // free text is legal
    expect(screen.getAllByRole("option")).toHaveLength(1);
    expect(screen.getByRole("option", { name: /MiniMax-M2\.5/ })).toBeInTheDocument();
  });

  it("reopening after selecting a value shows the full list (filter only after typing)", async () => {
    apiGet.mockResolvedValue({ models: [{ id: "glm-5.2" }, { id: "MiniMax-M2.5" }] });
    wrap(<Controlled providerId="p1" initial="glm-5.2" />);

    const input = screen.getByTestId("cb");
    fireEvent.focus(input);
    // value is "glm-5.2" but filterActive is false on fresh open → both options show
    expect(await screen.findByRole("option", { name: /MiniMax-M2\.5/ })).toBeInTheDocument();
    expect(screen.getAllByRole("option")).toHaveLength(2);
  });

  it("value not present in the list is allowed (free text, no error UI)", async () => {
    apiGet.mockResolvedValue({ models: [{ id: "glm-5.2" }] });
    wrap(<ModelCombobox value="custom/model-id" onChange={vi.fn()} providerId="p1" data-testid="cb" />);
    expect(screen.getByTestId("cb")).toHaveValue("custom/model-id");
  });

  it("empty discovery result shows the 'list unavailable' hint row", async () => {
    apiGet.mockResolvedValue({ models: [] });
    wrap(<ModelCombobox value="" onChange={vi.fn()} providerId="p1" data-testid="cb" />);
    fireEvent.focus(screen.getByTestId("cb"));
    expect(await screen.findByText("fields.model_list_unavailable")).toBeInTheDocument();
  });

  it("fetch error degrades to the same hint row (no toast, no crash)", async () => {
    apiGet.mockRejectedValue(new Error("boom"));
    wrap(<ModelCombobox value="" onChange={vi.fn()} providerId="p1" data-testid="cb" />);
    fireEvent.focus(screen.getByTestId("cb"));
    expect(await screen.findByText("fields.model_list_unavailable")).toBeInTheDocument();
  });

  it("staticOptions mode renders without any network call", async () => {
    wrap(<ModelCombobox value="" onChange={vi.fn()} staticOptions={["gpt-4.1", "o3"]} data-testid="cb" />);
    fireEvent.focus(screen.getByTestId("cb"));
    expect(await screen.findByRole("option", { name: /gpt-4\.1/ })).toBeInTheDocument();
    expect(screen.getByRole("option", { name: /^o3$/ })).toBeInTheDocument();
    expect(apiGet).not.toHaveBeenCalled();
  });

  it("disabled input does not open the list", () => {
    wrap(<ModelCombobox value="" onChange={vi.fn()} providerId="p1" disabled data-testid="cb" />);
    fireEvent.focus(screen.getByTestId("cb"));
    expect(screen.queryByRole("listbox")).not.toBeInTheDocument();
    expect(apiGet).not.toHaveBeenCalled();
  });

  it("keyboard: ArrowDown + Enter selects the highlighted option", async () => {
    apiGet.mockResolvedValue({ models: [{ id: "a-model" }, { id: "b-model" }] });
    const onChange = vi.fn();
    wrap(<ModelCombobox value="" onChange={onChange} providerId="p1" data-testid="cb" />);

    const input = screen.getByTestId("cb");
    fireEvent.focus(input);
    await screen.findByRole("option", { name: /a-model/ });
    fireEvent.keyDown(input, { key: "ArrowDown" });
    fireEvent.keyDown(input, { key: "ArrowDown" });
    fireEvent.keyDown(input, { key: "Enter" });
    expect(onChange).toHaveBeenCalledWith("b-model");
  });
});
```

- [ ] **Step 3: Запустить тест — убедиться, что падает**

```powershell
cd ui; npx vitest run src/components/provider-fields/__tests__/ModelCombobox.test.tsx
```

Expected: FAIL — `Cannot find module '../ModelCombobox'` (или аналогичная ошибка резолва).

- [ ] **Step 4: Реализация**

`ui/src/components/provider-fields/ModelCombobox.tsx`:

```tsx
"use client";

import { useCallback, useEffect, useId, useRef, useState } from "react";
import { Loader2 } from "lucide-react";
import { useTranslation } from "@/hooks/use-translation";
import { Input } from "@/components/ui/input";
import { useProviderModelsDetailed, type ProviderModel } from "@/lib/queries";
import { ModelBadges } from "@/components/model-badges";

export interface ModelComboboxProps {
  value: string;
  onChange: (value: string) => void;
  /** UUID of a saved provider — the list is lazy-loaded from
   *  GET /api/providers/{id}/models on first open. */
  providerId?: string | null;
  /** Static suggestion list for pre-create flows (setup wizard, provider
   *  create form) where no provider row exists yet. Ignored when providerId
   *  is set. */
  staticOptions?: string[];
  placeholder?: string;
  disabled?: boolean;
  id?: string;
  className?: string;
  "data-testid"?: string;
}

/** Unified model field: free-text Input + suggestion dropdown fed by the
 *  provider-models aggregator. Values outside the list are legal by design
 *  (custom model ids, providers without model listing). */
export function ModelCombobox({
  value,
  onChange,
  providerId,
  staticOptions,
  placeholder,
  disabled,
  id,
  className = "",
  "data-testid": testId,
}: ModelComboboxProps) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  // Lazy-load gate: the query only runs after the first open.
  const [activated, setActivated] = useState(false);
  // The input doubles as the filter box, but only AFTER the user types while
  // the list is open — otherwise reopening with a selected value would show
  // just that one option.
  const [filterActive, setFilterActive] = useState(false);
  const [highlight, setHighlight] = useState(-1);
  const rootRef = useRef<HTMLDivElement>(null);
  const listId = useId();

  const query = useProviderModelsDetailed(activated && providerId ? providerId : null);
  const options: ProviderModel[] = providerId
    ? (query.data ?? [])
    : (staticOptions ?? []).map((m) => ({ id: m }));
  const loading = Boolean(providerId) && activated && query.isLoading;

  const text = value.trim().toLowerCase();
  const filtered = filterActive && text
    ? options.filter((o) => o.id.toLowerCase().includes(text))
    : options;

  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      if (rootRef.current && !rootRef.current.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener("mousedown", onDown);
    return () => document.removeEventListener("mousedown", onDown);
  }, [open]);

  const openList = useCallback(() => {
    if (disabled) return;
    setActivated(true);
    setFilterActive(false);
    setHighlight(-1);
    setOpen(true);
  }, [disabled]);

  const pick = (modelId: string) => {
    onChange(modelId);
    setOpen(false);
  };

  const onKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "Escape") { setOpen(false); return; }
    if (e.key === "ArrowDown") {
      e.preventDefault();
      if (!open) { openList(); return; }
      setHighlight((h) => Math.min(h + 1, filtered.length - 1));
      return;
    }
    if (e.key === "ArrowUp") { e.preventDefault(); setHighlight((h) => Math.max(h - 1, 0)); return; }
    if (e.key === "Enter" && open && highlight >= 0 && filtered[highlight]) {
      e.preventDefault();
      pick(filtered[highlight].id);
    }
  };

  return (
    <div ref={rootRef} className={`relative min-w-0 ${className}`}>
      <Input
        id={id}
        role="combobox"
        aria-expanded={open}
        aria-controls={listId}
        aria-autocomplete="list"
        autoComplete="off"
        value={value}
        placeholder={placeholder}
        disabled={disabled}
        data-testid={testId}
        className="font-mono text-sm"
        onFocus={openList}
        onClick={openList}
        onChange={(e) => {
          onChange(e.target.value);
          setFilterActive(true);
          setHighlight(-1);
          if (!open) openList();
        }}
        onKeyDown={onKeyDown}
      />
      {open && (
        <ul
          id={listId}
          role="listbox"
          className="absolute left-0 right-0 top-full z-50 mt-1 max-h-64 overflow-y-auto overscroll-contain rounded-md border border-border bg-popover p-1 shadow-md"
        >
          {loading ? (
            <li className="flex items-center gap-2 px-2 py-1.5 text-xs text-muted-foreground">
              <Loader2 className="h-3.5 w-3.5 animate-spin" /> {t("fields.model_loading")}
            </li>
          ) : options.length === 0 ? (
            <li className="px-2 py-1.5 text-xs text-muted-foreground-subtle italic">
              {t("fields.model_list_unavailable")}
            </li>
          ) : filtered.length === 0 ? (
            <li className="px-2 py-1.5 text-xs text-muted-foreground-subtle italic">
              {t("fields.model_no_match")}
            </li>
          ) : (
            filtered.map((m, i) => (
              <li
                key={m.id}
                role="option"
                aria-selected={m.id === value}
                className={`flex cursor-pointer items-center justify-between gap-3 rounded-sm px-2 py-1.5 font-mono text-xs ${
                  i === highlight ? "bg-accent text-accent-foreground" : "hover:bg-accent/50"
                }`}
                onMouseDown={(e) => { e.preventDefault(); pick(m.id); }}
                onMouseEnter={() => setHighlight(i)}
              >
                <span className="truncate">{m.id}</span>
                <ModelBadges m={m} className="shrink-0" />
              </li>
            ))
          )}
        </ul>
      )}
    </div>
  );
}
```

`ui/src/components/provider-fields/index.ts` (пока один экспорт; Task 2/3 дополнят):

```ts
export { ModelCombobox, type ModelComboboxProps } from "./ModelCombobox";
```

Примечание (осознанное ограничение v1): выпадашка — absolute-элемент без портала; внутри скроллящегося DialogBody она прокручивается вместе с контентом. Это ок для списков max-h-64; НЕ добавлять портал/новые зависимости.

- [ ] **Step 5: Запустить тест — убедиться, что проходит**

```powershell
cd ui; npx vitest run src/components/provider-fields/__tests__/ModelCombobox.test.tsx
```

Expected: PASS (10 тестов).

- [ ] **Step 6: Commit**

```powershell
git add ui/src/components/provider-fields ui/src/i18n/locales/ru.json ui/src/i18n/locales/en.json
git commit -m "feat(ui): ModelCombobox — unified model field over the provider-models aggregator"
```

---

### Task 2: `ProviderSelect`

**Files:**
- Create: `ui/src/components/provider-fields/ProviderSelect.tsx`
- Modify: `ui/src/components/provider-fields/index.ts`
- Create: `ui/src/components/provider-fields/__tests__/ProviderSelect.test.tsx`

**Interfaces:**
- Consumes: `useProviders` из `@/lib/queries`; `Select*` из `@/components/ui/select`.
- Produces (используется задачами 4–5):

```ts
export interface ProviderSelectProps {
  value: string;                 // имя провайдера, "" = не выбран
  onChange: (name: string) => void;
  categories: string[];          // фильтр по Provider.type, напр. ["text","llm"]
  allowNone?: boolean;           // пункт «—» → onChange("")
  placeholder?: string;          // default: t("profiles.provider_placeholder")
  disabled?: boolean;
  size?: "sm" | "default";       // проброс в SelectTrigger
  className?: string;            // проброс в SelectTrigger
  id?: string;
}
export function ProviderSelect(props: ProviderSelectProps): JSX.Element
```

- [ ] **Step 1: Написать падающий тест**

`ui/src/components/provider-fields/__tests__/ProviderSelect.test.tsx`:

```tsx
import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import "@testing-library/jest-dom/vitest";

vi.mock("@/hooks/use-translation", () => ({
  useTranslation: () => ({ t: (key: string) => key, locale: "en" }),
}));

vi.mock("@/lib/queries", () => ({
  useProviders: () => ({
    data: [
      { id: "p1", name: "my-openai", type: "text", provider_type: "openai_compat", default_model: "gpt-4.1", enabled: true },
      { id: "p2", name: "legacy-llm", type: "llm", provider_type: "openai_compat", default_model: "old-model", enabled: true },
      { id: "p3", name: "whisper", type: "stt", provider_type: "openai-compatible", default_model: null, enabled: true },
    ],
  }),
}));

import { ProviderSelect } from "../ProviderSelect";

// jsdom не реализует scrollIntoView/pointer capture, которые дергает Radix Select.
window.HTMLElement.prototype.scrollIntoView = vi.fn();
window.HTMLElement.prototype.hasPointerCapture = vi.fn();
window.HTMLElement.prototype.releasePointerCapture = vi.fn();

describe("ProviderSelect", () => {
  it("offers only providers whose type is in `categories` (text+llm pair)", () => {
    render(<ProviderSelect value="" onChange={vi.fn()} categories={["text", "llm"]} />);
    fireEvent.pointerDown(screen.getByRole("combobox"));
    expect(screen.getByRole("option", { name: /my-openai/ })).toBeInTheDocument();
    expect(screen.getByRole("option", { name: /legacy-llm/ })).toBeInTheDocument();
    expect(screen.queryByRole("option", { name: /whisper/ })).not.toBeInTheDocument();
  });

  it("category filter for a media capability", () => {
    render(<ProviderSelect value="" onChange={vi.fn()} categories={["stt"]} />);
    fireEvent.pointerDown(screen.getByRole("combobox"));
    expect(screen.getByRole("option", { name: /whisper/ })).toBeInTheDocument();
    expect(screen.queryByRole("option", { name: /my-openai/ })).not.toBeInTheDocument();
  });

  it("shows the provider's default_model as a secondary label", () => {
    render(<ProviderSelect value="" onChange={vi.fn()} categories={["text"]} />);
    fireEvent.pointerDown(screen.getByRole("combobox"));
    expect(screen.getByRole("option", { name: /my-openai/ })).toHaveTextContent("gpt-4.1");
  });

  it("allowNone renders the dash item and maps it to empty string", () => {
    const onChange = vi.fn();
    render(<ProviderSelect value="my-openai" onChange={onChange} categories={["text", "llm"]} allowNone />);
    fireEvent.pointerDown(screen.getByRole("combobox"));
    fireEvent.click(screen.getByRole("option", { name: "—" }));
    expect(onChange).toHaveBeenCalledWith("");
  });

  it("selecting a provider calls onChange with its name", () => {
    const onChange = vi.fn();
    render(<ProviderSelect value="" onChange={onChange} categories={["text", "llm"]} />);
    fireEvent.pointerDown(screen.getByRole("combobox"));
    fireEvent.click(screen.getByRole("option", { name: /legacy-llm/ }));
    expect(onChange).toHaveBeenCalledWith("legacy-llm");
  });
});
```

- [ ] **Step 2: Запустить тест — убедиться, что падает**

```powershell
cd ui; npx vitest run src/components/provider-fields/__tests__/ProviderSelect.test.tsx
```

Expected: FAIL — `Cannot find module '../ProviderSelect'`.

- [ ] **Step 3: Реализация**

`ui/src/components/provider-fields/ProviderSelect.tsx`:

```tsx
"use client";

import { Link2 } from "lucide-react";
import { useTranslation } from "@/hooks/use-translation";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { useProviders } from "@/lib/queries";

const NONE = "__none__";

export interface ProviderSelectProps {
  value: string;
  onChange: (name: string) => void;
  /** Provider `type` categories to offer (e.g. ["text","llm"] for LLM slots —
   *  `llm` is the legacy alias for `text`). */
  categories: string[];
  /** Adds a "—" item that maps to "" (routing rules use it to unset the rule). */
  allowNone?: boolean;
  placeholder?: string;
  disabled?: boolean;
  size?: "sm" | "default";
  className?: string;
  id?: string;
}

/** Unified provider picker: name + default_model secondary label, filtered by
 *  capability categories. Data comes from useProviders (React Query). */
export function ProviderSelect({
  value,
  onChange,
  categories,
  allowNone = false,
  placeholder,
  disabled,
  size = "default",
  className,
  id,
}: ProviderSelectProps) {
  const { t } = useTranslation();
  const { data: providers = [] } = useProviders();
  const options = providers.filter((p) => categories.includes(p.type));

  return (
    <Select
      value={value === "" ? (allowNone ? NONE : "") : value}
      onValueChange={(v) => onChange(v === NONE ? "" : v)}
      disabled={disabled}
    >
      <SelectTrigger id={id} size={size} className={className}>
        <SelectValue placeholder={placeholder ?? t("profiles.provider_placeholder")} />
      </SelectTrigger>
      <SelectContent>
        {allowNone && (
          <SelectItem value={NONE} className="text-xs text-muted-foreground">
            <span className="text-muted-foreground">&mdash;</span>
          </SelectItem>
        )}
        {options.map((p) => (
          <SelectItem key={p.name} value={p.name} className="text-xs">
            <span className="flex min-w-0 items-center gap-2">
              <Link2 className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
              <span className="truncate">{p.name}</span>
              {p.default_model && (
                <span className="truncate text-2xs text-muted-foreground-subtle">{p.default_model}</span>
              )}
            </span>
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  );
}
```

`ui/src/components/provider-fields/index.ts` — добавить строку:

```ts
export { ProviderSelect, type ProviderSelectProps } from "./ProviderSelect";
```

- [ ] **Step 4: Запустить тест — убедиться, что проходит**

```powershell
cd ui; npx vitest run src/components/provider-fields/__tests__/ProviderSelect.test.tsx
```

Expected: PASS (5 тестов). Если Radix Select в jsdom требует иных шимов — смотреть, как это решают существующие тесты селектов (`ui/src/app/(authenticated)/agents/__tests__/agent-form.test.tsx`), и повторить их подход.

- [ ] **Step 5: Commit**

```powershell
git add ui/src/components/provider-fields
git commit -m "feat(ui): ProviderSelect — unified provider picker with category filter"
```

---

### Task 3: `useTtsVoices` + `VoiceSelect`

**Files:**
- Modify: `ui/src/lib/queries.ts` (qk + hook, рядом с `useProviderModelsDetailed`, ~строка 248)
- Create: `ui/src/components/provider-fields/VoiceSelect.tsx`
- Modify: `ui/src/components/provider-fields/index.ts`
- Create: `ui/src/components/provider-fields/__tests__/VoiceSelect.test.tsx`
- Modify (при необходимости): `ui/src/__tests__/api-coverage.test.ts`

**Interfaces:**
- Consumes: `apiGet` из `@/lib/api`; `Select*`, `Input`.
- Produces (используется задачами 4 и 6):

```ts
// queries.ts
export interface TtsVoice { id: string; name: string; description?: string; language?: string }
export function useTtsVoices(provider: string | null): UseQueryResult<TtsVoice[]>
// qk.ttsVoices = (provider: string) => ["tts-voices", provider] as const

// VoiceSelect.tsx
export interface VoiceSelectProps {
  value: string;                 // id голоса, "" = не задан / серверный дефолт
  onChange: (voiceId: string) => void;
  providerName: string;
  allowServerDefault?: boolean;  // пункт «— серверный дефолт» → onChange("")
  disabled?: boolean;
  size?: "sm" | "default";
  className?: string;
  id?: string;
}
export function VoiceSelect(props: VoiceSelectProps): JSX.Element
```

- [ ] **Step 1: Написать падающий тест**

`ui/src/components/provider-fields/__tests__/VoiceSelect.test.tsx`:

```tsx
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import "@testing-library/jest-dom/vitest";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";

vi.mock("@/hooks/use-translation", () => ({
  useTranslation: () => ({ t: (key: string) => key, locale: "en" }),
}));

const { apiGet } = vi.hoisted(() => ({ apiGet: vi.fn() }));
vi.mock("@/lib/api", () => ({ apiGet }));

import { VoiceSelect } from "../VoiceSelect";

window.HTMLElement.prototype.scrollIntoView = vi.fn();
window.HTMLElement.prototype.hasPointerCapture = vi.fn();
window.HTMLElement.prototype.releasePointerCapture = vi.fn();

function wrap(ui: React.ReactElement) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(<QueryClientProvider client={qc}>{ui}</QueryClientProvider>);
}

describe("VoiceSelect", () => {
  beforeEach(() => apiGet.mockReset());

  it("renders voices from /api/tts/voices for the provider", async () => {
    apiGet.mockResolvedValue({ voices: [{ id: "clone:Arty", name: "Arty" }, { id: "nova", name: "Nova", language: "en" }] });
    wrap(<VoiceSelect value="" onChange={vi.fn()} providerName="minimax" />);

    const trigger = await screen.findByRole("combobox");
    fireEvent.pointerDown(trigger);
    expect(await screen.findByRole("option", { name: /Arty/ })).toBeInTheDocument();
    expect(screen.getByRole("option", { name: /Nova/ })).toBeInTheDocument();
    expect(apiGet).toHaveBeenCalledWith("/api/tts/voices?provider=minimax");
  });

  it("selecting a voice calls onChange with its id", async () => {
    apiGet.mockResolvedValue({ voices: [{ id: "clone:Arty", name: "Arty" }] });
    const onChange = vi.fn();
    wrap(<VoiceSelect value="" onChange={onChange} providerName="minimax" />);
    fireEvent.pointerDown(await screen.findByRole("combobox"));
    fireEvent.click(await screen.findByRole("option", { name: /Arty/ }));
    expect(onChange).toHaveBeenCalledWith("clone:Arty");
  });

  it("allowServerDefault adds the dash item mapping to empty string", async () => {
    apiGet.mockResolvedValue({ voices: [{ id: "nova", name: "Nova" }] });
    const onChange = vi.fn();
    wrap(<VoiceSelect value="nova" onChange={onChange} providerName="minimax" allowServerDefault />);
    fireEvent.pointerDown(await screen.findByRole("combobox"));
    fireEvent.click(await screen.findByRole("option", { name: /voice_server_default/ }));
    expect(onChange).toHaveBeenCalledWith("");
  });

  it("empty voice list degrades to a free-text input", async () => {
    apiGet.mockResolvedValue({ voices: [] });
    const onChange = vi.fn();
    wrap(<VoiceSelect value="" onChange={onChange} providerName="broken-tts" />);
    const input = await screen.findByRole("textbox");
    fireEvent.change(input, { target: { value: "custom-voice" } });
    expect(onChange).toHaveBeenCalledWith("custom-voice");
  });

  it("fetch error degrades to a free-text input", async () => {
    apiGet.mockRejectedValue(new Error("toolgate down"));
    wrap(<VoiceSelect value="v1" onChange={vi.fn()} providerName="broken-tts" />);
    expect(await screen.findByRole("textbox")).toHaveValue("v1");
  });

  it("no provider → disabled, no fetch", () => {
    wrap(<VoiceSelect value="" onChange={vi.fn()} providerName="" />);
    expect(apiGet).not.toHaveBeenCalled();
  });
});
```

- [ ] **Step 2: Запустить тест — убедиться, что падает**

```powershell
cd ui; npx vitest run src/components/provider-fields/__tests__/VoiceSelect.test.tsx
```

Expected: FAIL — `Cannot find module '../VoiceSelect'`.

- [ ] **Step 3: Реализация — хук**

В `ui/src/lib/queries.ts`, в объект `qk` (после `providerModels`, ~строка 72):

```ts
  ttsVoices: (provider: string) => ["tts-voices", provider] as const,
```

После `useProviderModelsDetailed` (~строка 248):

```ts
export interface TtsVoice {
  id: string
  name: string
  description?: string
  language?: string
}

/** Voice list of a TTS provider (GET /api/tts/voices?provider=). Feeds the
 *  shared VoiceSelect field. */
export function useTtsVoices(provider: string | null) {
  return useQuery({
    queryKey: qk.ttsVoices(provider ?? ""),
    queryFn: () => apiGet<{ voices: TtsVoice[] }>(`/api/tts/voices?provider=${encodeURIComponent(provider ?? "")}`),
    select: (d) => d.voices ?? [],
    enabled: !!provider,
    retry: false,
    staleTime: 60_000,
  })
}
```

- [ ] **Step 4: Реализация — компонент**

`ui/src/components/provider-fields/VoiceSelect.tsx`:

```tsx
"use client";

import { useTranslation } from "@/hooks/use-translation";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { useTtsVoices } from "@/lib/queries";

const SERVER_DEFAULT = "__default__";

export interface VoiceSelectProps {
  value: string;
  onChange: (voiceId: string) => void;
  providerName: string;
  /** Adds a "— server default" item mapping to "" (provider dialog semantics:
   *  unset voice = the TTS server's own default). */
  allowServerDefault?: boolean;
  disabled?: boolean;
  size?: "sm" | "default";
  className?: string;
  id?: string;
}

/** Unified TTS voice picker over GET /api/tts/voices. Degrades to a free-text
 *  input when the list is unavailable (toolgate down, provider without a voice
 *  listing) so the field always stays fillable. */
export function VoiceSelect({
  value,
  onChange,
  providerName,
  allowServerDefault = false,
  disabled,
  size = "default",
  className,
  id,
}: VoiceSelectProps) {
  const { t } = useTranslation();
  const { data: voices = [], isLoading, isError } = useTtsVoices(providerName || null);

  if (!isLoading && (isError || voices.length === 0)) {
    return (
      <Input
        id={id}
        value={value}
        disabled={disabled || !providerName}
        placeholder={t("profiles.voice_placeholder")}
        onChange={(e) => onChange(e.target.value)}
        className={`font-mono text-sm ${className ?? ""}`}
      />
    );
  }

  return (
    <Select
      value={value === "" ? (allowServerDefault ? SERVER_DEFAULT : "") : value}
      onValueChange={(v) => onChange(v === SERVER_DEFAULT ? "" : v)}
      disabled={disabled || !providerName}
    >
      <SelectTrigger id={id} size={size} className={className}>
        <SelectValue placeholder={isLoading ? t("fields.voice_loading") : t("profiles.voice_placeholder")} />
      </SelectTrigger>
      <SelectContent>
        {allowServerDefault && (
          <SelectItem value={SERVER_DEFAULT} className="text-sm text-muted-foreground">
            <span className="text-muted-foreground">&mdash; {t("providers.voice_server_default")}</span>
          </SelectItem>
        )}
        {voices.map((v) => (
          <SelectItem key={v.id} value={v.id} className="text-sm font-mono">
            <span className="flex flex-col">
              <span>{v.name || v.id}</span>
              {(v.language || v.description) && (
                <span className="text-3xs text-muted-foreground-subtle">
                  {[v.language, v.description].filter(Boolean).join(" · ")}
                </span>
              )}
            </span>
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  );
}
```

`ui/src/components/provider-fields/index.ts` — добавить:

```ts
export { VoiceSelect, type VoiceSelectProps } from "./VoiceSelect";
```

- [ ] **Step 5: Запустить тест — убедиться, что проходит**

```powershell
cd ui; npx vitest run src/components/provider-fields/__tests__/VoiceSelect.test.tsx
```

Expected: PASS (6 тестов).

- [ ] **Step 6: api-coverage**

Открыть `ui/src/__tests__/api-coverage.test.ts`, найти таблицу endpoint→hook (формат строк: `["GET", "/api/providers/{id}/models", "useProviderModels"]`). Если `/api/tts/voices` в таблице отсутствует или не привязан к хуку — добавить строку:

```ts
  ["GET",    "/api/tts/voices",            "useTtsVoices"],
```

Затем прогнать: `cd ui; npx vitest run src/__tests__/api-coverage.test.ts` — Expected: PASS. Если тест устроен иначе (например, сканирует apiGet-литералы), привести к его фактическим правилам — цель: тест зелёный без ослабления его проверок.

- [ ] **Step 7: Commit**

```powershell
git add ui/src/components/provider-fields ui/src/lib/queries.ts ui/src/__tests__/api-coverage.test.ts
git commit -m "feat(ui): VoiceSelect + useTtsVoices — unified TTS voice picker with free-input degradation"
```

---

### Task 4: Внедрение в редактор профилей

**Files:**
- Modify: `ui/src/app/(authenticated)/profiles/_parts/ProfileEditor.tsx`
- Modify: `ui/src/app/(authenticated)/profiles/__tests__/profile-editor.test.tsx`

**Interfaces:**
- Consumes: `ProviderSelect`, `ModelCombobox`, `VoiceSelect` из `@/components/provider-fields` (сигнатуры — в задачах 1–3).
- Produces: ничего нового; поведение — смена провайдера очищает model (для text/compaction/vision) и voice (для tts).

- [ ] **Step 1: Обновить мок и добавить падающие тесты**

В `profile-editor.test.tsx` заменить мок `@/lib/queries` (сейчас только `useProviders`) на:

```ts
vi.mock("@/lib/queries", () => ({
  useProviders: () => ({
    data: [
      { id: "p1", name: "openai", type: "text", provider_type: "openai_compat", default_model: "gpt-4.1", enabled: true },
      { id: "p2", name: "minimax", type: "tts", provider_type: "minimax", default_model: null, enabled: true },
      { id: "p3", name: "other-llm", type: "text", provider_type: "openai_compat", default_model: "glm-5", enabled: true },
    ],
  }),
  useProviderModelsDetailed: () => ({ data: [], isLoading: false }),
  useTtsVoices: () => ({ data: [], isLoading: false, isError: false }),
}));
```

И добавить в конец `describe("ProfileEditor", ...)` два теста (Radix-шимы `scrollIntoView`/`hasPointerCapture`/`releasePointerCapture` добавить в топ файла, как в Task 2 Step 1):

```tsx
  it("changing the provider of a text row clears its model", async () => {
    render(<ProfileEditor profile={makeProfile()} open onClose={vi.fn()} />);

    const modelInput = screen.getByTestId("profile-model-text-0") as HTMLInputElement;
    expect(modelInput).toHaveValue("gpt-4");

    // ProviderSelect строки text — первый combobox в первой строке.
    // Выбираем ДРУГОЙ провайдер (не текущий "openai") — Radix не обязан
    // дёргать onValueChange при повторном выборе того же значения.
    const row = screen.getByTestId("profile-row-text-0");
    fireEvent.pointerDown(within(row).getAllByRole("combobox")[0]);
    fireEvent.click(await screen.findByRole("option", { name: /other-llm/ }));

    expect(screen.getByTestId("profile-model-text-0")).toHaveValue("");
  });

  it("model field is disabled until a provider is chosen", () => {
    render(<ProfileEditor profile={makeProfile()} open onClose={vi.fn()} />);
    fireEvent.click(screen.getAllByRole("button", { name: /profiles\.add_reserve/i })[0]);
    // новая строка: provider = "" → model input задизейблен
    expect(screen.getByTestId("profile-model-text-1")).toBeDisabled();
  });
```

(`within` импортировать из `@testing-library/react`.)

- [ ] **Step 2: Запустить — убедиться, что новые тесты падают**

```powershell
cd ui; npx vitest run "src/app/(authenticated)/profiles/__tests__/profile-editor.test.tsx"
```

Expected: 2 новых теста FAIL (провайдер не очищает модель; поле не задизейблено), старые 4 — PASS.

- [ ] **Step 3: Внедрить компоненты в ProfileEditor**

Изменения в `ProfileEditor.tsx`:

1. Импорты: удалить `apiGet` из `@/lib/api`, `Select/SelectContent/SelectItem/SelectTrigger/SelectValue`; добавить:

```tsx
import { ModelCombobox, ProviderSelect, VoiceSelect } from "@/components/provider-fields";
```

2. Удалить: `interface TtsVoice`, стейт `voicesByProvider`, `voiceFetchSeq`, `unmountedRef` (+ его useEffect), функцию `fetchVoices`, сброс `setVoicesByProvider({})` в эффекте пере-инициализации, функцию `providersFor` и переменную `options` в map (больше не нужны).

3. Добавить хелпер после `rowsFor`:

```tsx
  const providerIdByName = (name: string) =>
    providers.find((p) => p.name === name)?.id ?? null;
```

(`useProviders` остаётся — используется этим хелпером.)

4. Внутри `rows.map((row, idx) => ...)` заменить весь блок `<Select …провайдер…>`, `{hasModelField(cap) && <Input …/>}` и `{cap === "tts" && <Select …голос…>}` на:

```tsx
                        <ProviderSelect
                          value={row.provider}
                          categories={categoriesFor(cap)}
                          size="sm"
                          className="w-40"
                          onChange={(v) => {
                            // Провайдер сменился — прежние model/voice ему не принадлежат.
                            // Пустая model = default_model провайдера (семантика useAgentTextModel).
                            const patch: Partial<SlotEntry> = { provider: v };
                            if (hasModelField(cap)) patch.model = "";
                            if (cap === "tts") patch.voice = "";
                            updateRow(cap, idx, patch);
                          }}
                        />

                        {hasModelField(cap) && (
                          <ModelCombobox
                            value={row.model ?? ""}
                            onChange={(m) => updateRow(cap, idx, { model: m })}
                            providerId={providerIdByName(row.provider)}
                            disabled={!row.provider}
                            placeholder={row.provider ? t("profiles.model_default_placeholder") : t("fields.select_provider_first")}
                            className="w-40"
                            data-testid={`profile-model-${cap}-${idx}`}
                          />
                        )}

                        {cap === "tts" && (
                          <VoiceSelect
                            value={row.voice ?? ""}
                            onChange={(v) => updateRow(cap, idx, { voice: v })}
                            providerName={row.provider}
                            size="sm"
                            className="w-40"
                          />
                        )}
```

5. Новый i18n-ключ в `ru.json`:

```json
  "profiles.model_default_placeholder": "По умолчанию провайдера"
```

в `en.json`:

```json
  "profiles.model_default_placeholder": "Provider default"
```

- [ ] **Step 4: Запустить тесты профилей — все зелёные**

```powershell
cd ui; npx vitest run "src/app/(authenticated)/profiles/__tests__/profile-editor.test.tsx"
```

Expected: PASS (6 тестов). Внимание на тест «Save calls useUpdateProfile…»: он менял модель через `fireEvent.change` по testid — ModelCombobox рендерит Input с тем же testid и пробрасывает onChange, поведение сохраняется.

- [ ] **Step 5: Commit**

```powershell
git add "ui/src/app/(authenticated)/profiles" ui/src/i18n/locales/ru.json ui/src/i18n/locales/en.json
git commit -m "refactor(ui): ProfileEditor on shared provider-fields; provider change clears model/voice"
```

---

### Task 5: Внедрение в роутинг-правила агента + снос discovery-механики

**Files:**
- Modify: `ui/src/app/(authenticated)/agents/RoutingRulesEditor.tsx`
- Modify: `ui/src/app/(authenticated)/agents/AgentEditDialog.tsx`
- Modify: `ui/src/app/(authenticated)/agents/page.tsx`
- Modify: `ui/src/app/(authenticated)/agents/__tests__/agent-form.test.tsx`
- Modify: `ui/src/app/(authenticated)/agents/__tests__/agent-tabs.test.tsx`
- Modify: `ui/src/app/(authenticated)/agents/__tests__/agents-page.test.tsx`

**Interfaces:**
- Consumes: `ProviderSelect` (allowNone), `ModelCombobox` из `@/components/provider-fields`.
- Produces: `RoutingRulesEditorProps` СУЖАЕТСЯ до `{ routing, llmProviders, onChange }`; `AgentEditDialogProps` теряет `discoveredModels`, `modelsLoading`, `fetchModels`.

- [ ] **Step 1: RoutingRulesEditor — переписать**

В `RoutingRulesEditor.tsx`:

1. Удалить константы `PROVIDERS` (строки 13–27) и `FALLBACK_MODELS` (строки 40–54) целиком.
2. Импорты: удалить `Link2` и `Select*` (после правок не используются); `Input` ОСТАВИТЬ — он нужен полю temperature в expanded-секции; добавить:

```tsx
import { ModelCombobox, ProviderSelect } from "@/components/provider-fields";
```

3. Из props `RoutingRuleRow` и `RoutingRulesEditorProps` удалить `discoveredModels` и `fetchModels` (в обоих местах). `llmProviders` ОСТАВИТЬ (нужен для подстановки default_model и для кнопки add-rule).
4. В `RoutingRuleRow` заменить блок `<Select …провайдер…>` (строки 82–108) и `<Input value={rule.model} …>` (строки 109–111) на:

```tsx
          <ProviderSelect
            value={rule.provider}
            allowNone
            categories={["text", "llm"]}
            className="w-full bg-background border-border text-xs h-9"
            onChange={(v) => {
              if (v === "") { onChange({ provider: "", model: "" }); return; }
              const conn = llmProviders.find((p) => p.name === v);
              onChange({ provider: v, model: conn?.default_model ?? "" });
            }}
          />
          <ModelCombobox
            value={rule.model}
            onChange={(m) => onChange({ model: m })}
            providerId={llmProviders.find((p) => p.name === rule.provider)?.id ?? null}
            disabled={!rule.provider}
            placeholder={t("agents.model_placeholder")}
            className="w-full"
          />
```

5. В `RoutingRulesEditor` убрать проброс `discoveredModels`/`fetchModels` в `RoutingRuleRow`.

- [ ] **Step 2: AgentEditDialog — сузить props**

В `AgentEditDialog.tsx`:

1. Из `AgentEditDialogProps` удалить поля `discoveredModels`, `modelsLoading`, `fetchModels` (строки 171–174) и комментарий `// Models (used by RoutingRulesEditor)`.
2. Из деструктуризации в `AgentEditDialog({...})` удалить `discoveredModels`, `fetchModels`.
3. Вызов заменить на:

```tsx
                <RoutingRulesEditor routing={form.routing} llmProviders={llmProviders} onChange={(routing) => upd({ routing })} />
```

- [ ] **Step 3: agents/page.tsx — снести discovery-механику**

1. Удалить импорт `FALLBACK_MODELS` (строка 32: `import { FALLBACK_MODELS } from "./RoutingRulesEditor";`).
2. Удалить блок «Dynamic model discovery» целиком (строки ~417–446): стейт `discoveredModels`/`modelsLoading`, `discoveredModelsRef`, функцию `fetchModels`.
3. Из вызова `<AgentEditDialog …>` удалить строки `discoveredModels={discoveredModels}`, `modelsLoading={modelsLoading}`, `fetchModels={fetchModels}`.
4. Если после этого импорт типа `Provider` (использовался в fetchModels) или `apiGet` становятся неиспользуемыми — удалить; `npx tsc --noEmit` покажет.

- [ ] **Step 4: Обновить тест-моки**

Во всех трёх файлах (`agent-form.test.tsx` ~строки 57–58, `agent-tabs.test.tsx` ~строки 61–62, `agents-page.test.tsx` ~строки 40–41) из `vi.mock("../RoutingRulesEditor", …)` удалить ключи `PROVIDERS` и `FALLBACK_MODELS`. Если тесты передают в `AgentEditDialog` пропсы `discoveredModels`/`modelsLoading`/`fetchModels` — удалить и их. Если моки `@/lib/queries` в этих файлах не содержат `useProviders` — компонент `ProviderSelect` внутри замоканного `RoutingRulesEditor` не рендерится (модуль замокан целиком), поэтому дополнительных моков не требуется; при падениях типа «useProviders is not a function» — добавить `useProviders: () => ({ data: [] })` в мок queries.

- [ ] **Step 5: Прогнать тесты агентов + tsc**

```powershell
cd ui; npx vitest run "src/app/(authenticated)/agents"; npx tsc --noEmit
```

Expected: PASS все; tsc без ошибок.

- [ ] **Step 6: Commit**

```powershell
git add "ui/src/app/(authenticated)/agents"
git commit -m "refactor(ui): routing rules on ProviderSelect+ModelCombobox; drop dead discovery plumbing and FALLBACK_MODELS"
```

---

### Task 6: Внедрение в диалог провайдеров (Text + Media) + чистка page.tsx

**Files:**
- Modify: `ui/src/app/(authenticated)/providers/_parts/TextFields.tsx`
- Modify: `ui/src/app/(authenticated)/providers/_parts/MediaFields.tsx`
- Modify: `ui/src/app/(authenticated)/providers/ProviderDialog.tsx`
- Modify: `ui/src/app/(authenticated)/providers/page.tsx`
- Modify: `ui/src/app/(authenticated)/providers/__tests__/providers-page.test.tsx`, `provider-form.test.ts` (по фактическим падениям)

**Interfaces:**
- Consumes: `ModelCombobox`, `VoiceSelect` из `@/components/provider-fields`.
- Produces: `TextFieldsProps` теряет `discoveredModels`, `modelsLoading`, `onDiscoverModels`; `MediaFieldsProps` теряет `ttsVoices`, `ttsVoicesLoading`; `ProviderDialogProps` теряет `discoveredModels`, `modelsLoading`, `onDiscoverModels`, `ttsVoices`, `ttsVoicesLoading`.

- [ ] **Step 1: TextFields**

1. Импорт: `import { ModelCombobox } from "@/components/provider-fields";`; удалить импорт `RefreshCw`? — НЕТ, `RefreshCw` ещё используется в test-connection секции; оставить.
2. Из `TextFieldsProps` и деструктуризации удалить `discoveredModels`, `modelsLoading`, `onDiscoverModels`.
3. Добавить локальный стейт пресетных моделей (после `const { t } = useTranslation();`):

```tsx
  // Model suggestions from the picked catalog preset — the create flow has no
  // saved provider id to discover from, but the catalog already ships a list.
  const [presetModels, setPresetModels] = React.useState<string[]>([]);
```

4. В `applyPreset` добавить первой строкой тела: `setPresetModels(p.models ?? []);`
5. Заменить весь блок `{/* Default Model */} <div className="space-y-1.5"> … </div>` (строки 135–183) на:

```tsx
      {/* Default Model */}
      <div className="space-y-1.5">
        <label htmlFor={modelId} className="text-xs font-medium text-muted-foreground">
          {t("providers.field_model")} <span className="text-destructive">*</span>
        </label>
        <ModelCombobox
          id={modelId}
          value={form.default_model ?? ""}
          onChange={(v) => setForm((f) => ({ ...f, default_model: v }))}
          providerId={isEditing ? editing?.id ?? null : null}
          staticOptions={!isEditing ? presetModels : undefined}
          placeholder="MiniMax-Text-01"
        />
        {selectedType?.supports_model_listing === false && (
          <p className="text-2xs text-warning">{t("providers.no_model_discovery")}</p>
        )}
      </div>
```

(Кнопка re-discover, ветка Select-vs-Input и хинт `providers.save_first_to_discover` уходят.)

- [ ] **Step 2: MediaFields**

1. Импорты: `import { ModelCombobox, VoiceSelect } from "@/components/provider-fields";`.
2. Удалить `interface TtsVoice`; из `MediaFieldsProps` и деструктуризации удалить `ttsVoices`, `ttsVoicesLoading`.
3. Блок `{/* Model */}` (строки 116–124) заменить на:

```tsx
      {/* Model */}
      <div className="space-y-1.5">
        <label htmlFor={mediaModelIdLabel} className="text-xs font-medium text-muted-foreground">
          {t("providers.field_model_short")}{" "}
          <span className="text-muted-foreground-subtle font-normal">({t("providers.optional")})</span>
        </label>
        <ModelCombobox
          id={mediaModelIdLabel}
          value={form.default_model ?? ""}
          onChange={(v) => setForm((f) => ({ ...f, default_model: v }))}
          providerId={isEditing ? editing?.id ?? null : null}
          placeholder="Systran/faster-whisper-large-v3"
        />
      </div>
```

где `mediaModelIdLabel` — новый `React.useId()` в начале компонента: `const mediaModelIdLabel = React.useId();` (раньше поле было `Field` без id).

4. Блок голоса (строки 127–180) заменить на:

```tsx
      {/* Voice (TTS only) */}
      {dialogCategory === "tts" && (
        <div className="space-y-1.5">
          <label htmlFor={voiceId} className="text-xs font-medium text-muted-foreground">
            {t("providers.field_voice")}{" "}
            <span className="text-muted-foreground-subtle font-normal">({t("providers.optional")})</span>
          </label>
          <VoiceSelect
            id={voiceId}
            value={(getOpts(form).voice as string | undefined) ?? ""}
            onChange={(v) =>
              setForm((f) => ({ ...f, options: { ...getOpts(f), voice: v || undefined } }))
            }
            providerName={form.name}
            allowServerDefault
            className="text-sm font-mono"
          />
          <p className="text-2xs text-muted-foreground-subtle">{t("providers.field_voice_hint")}</p>
        </div>
      )}
```

- [ ] **Step 3: ProviderDialog**

Удалить из `ProviderDialogProps`, деструктуризации и вызовов `TextFields`/`MediaFields`: `discoveredModels`, `modelsLoading`, `onDiscoverModels`, `ttsVoices`, `ttsVoicesLoading`; удалить `interface TtsVoice`.

- [ ] **Step 4: providers/page.tsx — чистка**

Удалить:
- стейт `discoveredModels`/`setDiscoveredModels`, `modelsLoading`/`setModelsLoading` (строки 86–87);
- блок autoModels: `editingId`, `useProviderModelsDetailed(editingId)`, `effectiveModels` (строки 88–93) и импорт `useProviderModelsDetailed`;
- стейт `ttsVoices`/`ttsVoicesLoading` (строки 94–95) и весь эффект «TTS voice list loader» (строки 172–193);
- функцию `discoverModels` (строки 151–170);
- вызовы `setDiscoveredModels([])` в `openCreate`, `openEdit`, `setCategory`, `onSetProviderType`;
- из JSX `<ProviderDialog …>` строки `discoveredModels={effectiveModels}`, `modelsLoading={modelsLoading || autoModelsLoading}`, `onDiscoverModels={discoverModels}`, `ttsVoices={ttsVoices}`, `ttsVoicesLoading={ttsVoicesLoading}`.

- [ ] **Step 5: Тесты + tsc**

```powershell
cd ui; npx vitest run "src/app/(authenticated)/providers"; npx tsc --noEmit
```

Expected: провалившиеся тесты чинить по месту: моки `@/lib/queries` в тестах провайдеров должны отдавать `useProviderModelsDetailed: () => ({ data: [], isLoading: false })` и `useTtsVoices: () => ({ data: [], isLoading: false, isError: false })`; ассерты на снесённые пропсы/кнопку discover — удалить. tsc без ошибок.

- [ ] **Step 6: Commit**

```powershell
git add "ui/src/app/(authenticated)/providers"
git commit -m "refactor(ui): provider dialog on ModelCombobox/VoiceSelect; drop triple discovery plumbing in providers page"
```

---

### Task 7: Setup-визард

**Files:**
- Modify: `ui/src/app/setup/page.tsx`
- Modify: `ui/src/app/setup/__tests__/setup-page.test.tsx` (по фактическим падениям)

**Interfaces:**
- Consumes: `ModelCombobox` (режим `staticOptions`) из `@/components/provider-fields`.
- Produces: ничего; `FALLBACK_MODELS` в setup ОСТАЁТСЯ (это источник staticOptions).

- [ ] **Step 1: Снести битый pre-create discovery и chips**

В `ui/src/app/setup/page.tsx`:

1. Удалить функцию `discoverModels` (строки ~247–263) и `discoverGenRef` (строка 246). `providerNameRef` оставить (используется в doStep1/шаге профиля).
2. Удалить стейт `discoveredModels`/`setDiscoveredModels` и `modelsLoading`/`setModelsLoading` (строка ~169); удалить строки `const modelOptions = …; void modelOptions;` (229–230).
3. В `handleProviderTypeChange` удалить строки `setDiscoveredModels([]);` и `discoverGenRef.current++;`.
4. Импортировать: `import { ModelCombobox } from "@/components/provider-fields";`
5. Заменить весь блок `{/* Model */} {providerType && ( … )}` (строки ~616–691, включая ветку Select-с-кнопкой, ветку Input-с-кнопкой и chips-кнопки fallback-моделей) на:

```tsx
              {/* Model */}
              {providerType && (
                <div className="space-y-1.5">
                  <label htmlFor={modelId} className="text-sm font-medium text-muted-foreground">
                    {t("setup.model")} <span className="text-destructive">*</span>
                  </label>
                  <ModelCombobox
                    id={modelId}
                    value={defaultModel}
                    onChange={setDefaultModel}
                    staticOptions={fallbackModels}
                    placeholder={fallbackModels.length > 0 ? fallbackModels[0] : t("setup.model_placeholder")}
                  />
                </div>
              )}
```

6. Неиспользуемые после правки импорты (`Select*`, `RefreshCw` — проверить остальные использования по файлу) удалить; `npx tsc --noEmit` покажет.

Примечание: post-create проверка ключа `apiGet(\`/api/providers/${created.id}/models\`)` в `doStep1` НЕ трогается — это валидация, не поле.

- [ ] **Step 2: Тесты + tsc**

```powershell
cd ui; npx vitest run src/app/setup; npx tsc --noEmit
```

Expected: падения `setup-page.test.tsx`, завязанные на кнопку discover / chips / `setup.select_model`, переписать под combobox (модель по-прежнему вводится в input с `id={modelId}` через `fireEvent.change`). tsc без ошибок.

- [ ] **Step 3: Commit**

```powershell
git add ui/src/app/setup
git commit -m "refactor(ui): setup wizard model field on ModelCombobox; remove silently-broken pre-create discovery"
```

---

### Task 8: Чистка локалей + финальная верификация

**Files:**
- Modify: `ui/src/i18n/locales/ru.json`, `ui/src/i18n/locales/en.json`

- [ ] **Step 1: Удалить осиротевшие ключи**

Для каждого из ключей: `providers.save_first_to_discover`, `providers.discover_failed`, `providers.discover`, `providers.select_model`, `setup.select_model`, `common.discover` — выполнить поиск использований по `ui/src` (инструментом Grep, паттерн — имя ключа в кавычках, например `"providers\.save_first_to_discover"`, исключая сами файлы локалей `src/i18n/locales/`). Ключи, у которых 0 использований вне локалей, удалить из ОБОИХ локалей (`ru.json` и `en.json`). Ключи, которые ещё используются (например, `providers.no_model_discovery` — остался в TextFields), НЕ трогать.

- [ ] **Step 2: Полный прогон**

```powershell
cd ui; npm test
```

Expected: все тесты PASS.

```powershell
cd ui; npm run build
```

Expected: сборка успешна, tsc-ошибок нет.

- [ ] **Step 3: Commit**

```powershell
git add ui/src/i18n/locales
git commit -m "chore(ui): drop locale keys orphaned by provider-fields unification"
```

---

## Вне плана (по запросу пользователя)

Деплой UI на прод — `scripts/deploy-ui.sh` (union `_next/static`, см. память `reference_deploy_gaps`) — выполнять только по явной команде.
