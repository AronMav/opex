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
  /** Matches the `size` prop of the sibling ProviderSelect/VoiceSelect so the
   *  three fields line up at the same height when placed in a row. */
  size?: "sm" | "default";
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
  size = "default",
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
    // Only move up when something is already highlighted — from the initial
    // "nothing highlighted" state (h = -1) ArrowUp is a no-op, not a jump to 0.
    if (e.key === "ArrowUp") { e.preventDefault(); setHighlight((h) => (h > 0 ? h - 1 : h)); return; }
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
        aria-activedescendant={open && highlight >= 0 ? `${listId}-opt-${highlight}` : undefined}
        autoComplete="off"
        value={value}
        placeholder={placeholder}
        disabled={disabled}
        data-testid={testId}
        className={`font-mono text-sm ${size === "sm" ? "h-8" : ""}`}
        onFocus={openList}
        onClick={openList}
        onChange={(e) => {
          onChange(e.target.value);
          setFilterActive(true);
          setHighlight(-1);
          if (!open) { setActivated(true); setOpen(true); }
        }}
        onKeyDown={onKeyDown}
      />
      {open && (
        <ul
          id={listId}
          role="listbox"
          // Grow past the (often narrow) input up to a cap so long model ids
          // stay readable; min-w-full keeps it at least as wide as the field.
          className="absolute left-0 top-full z-50 mt-1 max-h-64 w-max min-w-full max-w-[min(24rem,90vw)] overflow-y-auto overscroll-contain rounded-md border border-border bg-popover p-1 shadow-md"
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
                id={`${listId}-opt-${i}`}
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
