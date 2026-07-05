"use client";

import React from "react";
import { useQuery } from "@tanstack/react-query";
import { apiGet } from "@/lib/api";
import { Input } from "@/components/ui/input";
import { useTranslation } from "@/hooks/use-translation";

export interface CatalogProvider {
  id: string;
  name: string;
  api?: string;
  env: string[];
  openai_compatible: boolean;
  /** OPEX provider_type to create (native when known, else openai_compat). */
  provider_type: string;
  models: string[];
}

/** Searchable picker over the model-catalog provider presets (models.dev/…).
 *  Renders nothing until the catalog has loaded. Dumb — the parent maps the
 *  picked provider onto the form (provider_type/base_url/model). */
export function ProviderPresetPicker({ onPick }: { onPick: (p: CatalogProvider) => void }) {
  const { t } = useTranslation();
  const { data: providers = [] } = useQuery({
    queryKey: ["catalog-providers"],
    queryFn: () => apiGet<{ providers: CatalogProvider[] }>("/api/catalog/providers"),
    select: (d) => d.providers,
    staleTime: 5 * 60_000,
    retry: false,
  });

  const [q, setQ] = React.useState("");
  const [open, setOpen] = React.useState(false);

  const filtered = React.useMemo(() => {
    const s = q.trim().toLowerCase();
    const base = s
      ? providers.filter((p) => p.name.toLowerCase().includes(s) || p.id.toLowerCase().includes(s))
      : providers;
    return base.slice(0, 60);
  }, [q, providers]);

  if (providers.length === 0) return null;

  return (
    <div className="space-y-1.5">
      <label className="text-xs font-medium text-muted-foreground">{t("providers.preset_label")}</label>
      <div className="relative">
        <Input
          value={q}
          placeholder={t("providers.preset_placeholder")}
          onChange={(e) => { setQ(e.target.value); setOpen(true); }}
          onFocus={() => setOpen(true)}
          onBlur={() => setTimeout(() => setOpen(false), 150)}
          className="text-sm"
        />
        {open && filtered.length > 0 && (
          <div className="absolute z-50 mt-1 w-full max-h-60 overflow-y-auto rounded-md border border-border bg-popover shadow-lg">
            {filtered.map((p) => (
              <button
                key={p.id}
                type="button"
                className="flex w-full items-center justify-between gap-2 px-3 py-1.5 text-left text-sm hover:bg-muted/50"
                onMouseDown={(e) => { e.preventDefault(); onPick(p); setQ(p.name); setOpen(false); }}
              >
                <span className="truncate min-w-0">{p.name}</span>
                <span className="text-2xs text-muted-foreground-subtle shrink-0">
                  {p.models.length} {t("providers.preset_models")}
                </span>
              </button>
            ))}
          </div>
        )}
      </div>
      <p className="text-2xs text-muted-foreground-subtle">{t("providers.preset_hint")}</p>
    </div>
  );
}
