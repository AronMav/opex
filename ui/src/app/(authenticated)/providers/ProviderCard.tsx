"use client";

import React from "react";
import { useTranslation } from "@/hooks/use-translation";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import { Globe, Key, Pencil, Trash2, Link2 } from "lucide-react";
import type { Provider } from "@/types/api";
import { CATEGORY_ICONS } from "./_parts/constants";
import type { ProviderCategory } from "./_parts/constants";

interface ProviderCardProps {
  provider: Provider;
  cap: ProviderCategory;
  isActive: boolean;
  draftPrio: number;
  typeLabel: string;
  isCapabilityGroup: boolean;
  onToggleActive: () => void;
  onApplyPriority: (n: number) => void;
  onDraftPriority: (n: number) => void;
  onEdit: () => void;
  onDelete: () => void;
}

export function ProviderCard({
  provider,
  cap,
  isActive,
  draftPrio,
  typeLabel,
  isCapabilityGroup,
  onToggleActive,
  onApplyPriority,
  onDraftPriority,
  onEdit,
  onDelete,
}: ProviderCardProps) {
  const { t } = useTranslation();

  return (
    <div
      className={`neu-card neu-hover p-5 space-y-4 overflow-hidden ${isActive ? "ring-1 ring-primary/30" : ""}`}
    >
      {/* Header */}
      <div className="flex items-start gap-3">
        <div className="flex items-center justify-center h-9 w-9 rounded-lg bg-muted/50 border border-border/60 text-muted-foreground shrink-0">
          {CATEGORY_ICONS[cap] ?? <Link2 className="h-4 w-4" />}
        </div>
        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-1.5 min-w-0">
            <p className="font-semibold text-sm font-mono truncate">
              {provider.name}
            </p>
            {isActive && (
              <span className="text-[9px] font-bold px-1 py-0 rounded bg-primary/10 text-primary border border-primary/20">
                {t("providers.active_badge")}
              </span>
            )}
          </div>
          <div className="flex items-center gap-1.5 mt-0.5 flex-wrap">
            <Badge variant="secondary" className="text-[10px] px-1.5 py-0 font-mono">
              {typeLabel}
            </Badge>
            {provider.default_model && (
              <span className="text-[11px] text-muted-foreground font-mono truncate">
                {provider.default_model}
              </span>
            )}
          </div>
        </div>
      </div>

      {/* Base URL */}
      {provider.base_url && (
        <div className="flex items-center gap-1.5 text-xs text-muted-foreground/60 font-mono truncate">
          <Globe className="h-3 w-3 shrink-0" />
          <span className="truncate">{provider.base_url}</span>
        </div>
      )}

      {/* API key status */}
      <div className="flex items-center gap-1.5 text-xs text-muted-foreground/70">
        <Key className="h-3 w-3 shrink-0" />
        <span className="font-mono truncate">
          {provider.api_key ?? (provider.has_api_key ? t("providers.api_key_configured") : t("providers.api_key_not_set"))}
        </span>
      </div>

      {/* Notes */}
      {provider.notes && (
        <p className="text-[11px] text-muted-foreground/60 truncate">
          {provider.notes}
        </p>
      )}

      {/* Enabled badge for non-text */}
      {cap !== "text" && (
        <div>
          <Badge variant="secondary" className={`text-[10px] px-1.5 py-0 ${provider.enabled ? "text-success" : "text-muted-foreground"}`}>
            {provider.enabled ? t("providers.status_enabled") : t("providers.status_disabled")}
          </Badge>
        </div>
      )}

      {/* Active + Priority controls (capabilities only, not text) */}
      {isCapabilityGroup && (
        <div className="flex items-center gap-2 pt-1 border-t border-border/30">
          <label className="flex items-center gap-1.5 cursor-pointer select-none">
            <Switch checked={isActive} onCheckedChange={onToggleActive} aria-label={t("providers.active_toggle")} />
            <span className="text-xs text-muted-foreground">{t("providers.active_toggle")}</span>
          </label>
          {isActive && (
            <label className="flex items-center gap-1.5 ml-auto">
              <span className="text-xs text-muted-foreground shrink-0">{t("providers.priority_label")}</span>
              <Input
                type="number"
                aria-label={t("providers.priority_label")}
                value={draftPrio}
                min={1}
                max={100}
                className="w-16 text-center font-mono h-7 text-xs"
                onChange={(e) => onDraftPriority(Number(e.target.value))}
                onBlur={(e) => onApplyPriority(Number(e.target.value))}
                onKeyDown={(e) => {
                  if (e.key === "Enter") onApplyPriority(Number(e.currentTarget.value));
                }}
              />
            </label>
          )}
        </div>
      )}

      {/* Edit / Delete actions */}
      <div className="flex items-center gap-2 border-t border-border/30 pt-1">
        <Button variant="outline" size="sm" className="flex-1 h-7 text-xs" onClick={onEdit}>
          <Pencil className="h-3 w-3" />
          {t("common.edit")}
        </Button>
        <Button
          variant="outline"
          size="sm"
          className="h-7 text-xs text-destructive hover:text-destructive"
          onClick={onDelete}
          aria-label={t("common.delete")}
        >
          <Trash2 className="h-3 w-3" />
        </Button>
      </div>
    </div>
  );
}