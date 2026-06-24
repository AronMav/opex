"use client";

import React, { forwardRef } from "react";
import { useTranslation } from "@/hooks/use-translation";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Switch } from "@/components/ui/switch";
import { Globe, Key, Pencil, Trash2, Link2, GripVertical } from "lucide-react";
import type { DraggableAttributes } from "@dnd-kit/core";
import type { DraggableSyntheticListeners } from "@dnd-kit/core";
import type { Provider } from "@/types/api";
import { CATEGORY_ICONS } from "./_parts/constants";
import type { ProviderCategory } from "./_parts/constants";

export interface ProviderRowProps {
  provider: Provider;
  cap: ProviderCategory;
  isActive: boolean;
  typeLabel: string;
  isCapabilityGroup: boolean;
  draggable?: boolean;
  isDragging?: boolean;
  dragHandleAttributes?: DraggableAttributes;
  dragHandleListeners?: DraggableSyntheticListeners;
  style?: React.CSSProperties;
  onToggleActive: () => void;
  onEdit: () => void;
  onDelete: () => void;
}

export const ProviderRow = forwardRef<HTMLDivElement, ProviderRowProps>(function ProviderRow(
  {
    provider, cap, isActive, typeLabel, isCapabilityGroup,
    draggable = false, isDragging = false,
    dragHandleAttributes, dragHandleListeners, style,
    onToggleActive, onEdit, onDelete,
  },
  ref,
) {
  const { t } = useTranslation();

  return (
    <div
      ref={ref}
      style={style}
      className={`group relative flex flex-col md:flex-row md:items-center gap-3 neu-flat p-4 transition-all hover:border-primary/20 ${isActive ? "ring-1 ring-primary/30" : ""} ${isDragging ? "opacity-80 shadow-lg" : ""} ${!isActive && isCapabilityGroup ? "opacity-60" : ""}`}
    >
      {/* Drag handle — active capability rows only */}
      {draggable && (
        <button
          type="button"
          className="shrink-0 cursor-grab active:cursor-grabbing touch-none text-muted-foreground/50 hover:text-muted-foreground"
          aria-label={t("providers.drag_handle_aria")}
          {...dragHandleAttributes}
          {...dragHandleListeners}
        >
          <GripVertical className="h-4 w-4" />
        </button>
      )}

      {/* Identity */}
      <div className="flex items-center gap-3 md:min-w-[240px]">
        <div className="flex items-center justify-center h-9 w-9 rounded-lg bg-muted/50 border border-border/60 text-muted-foreground shrink-0">
          {CATEGORY_ICONS[cap] ?? <Link2 className="h-4 w-4" />}
        </div>
        <div className="flex flex-col min-w-0">
          <div className="flex items-center gap-1.5 min-w-0">
            <span className="font-semibold text-sm font-mono truncate">{provider.name}</span>
            {isActive && (
              <span className="text-[9px] font-bold px-1 py-0 rounded bg-primary/10 text-primary border border-primary/20">
                {t("providers.active_badge")}
              </span>
            )}
          </div>
          <div className="flex items-center gap-1.5 mt-0.5 flex-wrap">
            <Badge variant="secondary" className="text-[10px] px-1.5 py-0 font-mono">{typeLabel}</Badge>
            {provider.default_model && (
              <span className="text-[11px] text-muted-foreground font-mono truncate">{provider.default_model}</span>
            )}
          </div>
        </div>
      </div>

      {/* Meta: base_url + api key + enabled */}
      <div className="flex flex-1 flex-wrap items-center gap-x-3 gap-y-1 min-w-0">
        {provider.base_url && (
          <span className="flex items-center gap-1.5 text-xs text-muted-foreground/60 font-mono truncate min-w-0">
            <Globe className="h-3 w-3 shrink-0" />
            <span className="truncate">{provider.base_url}</span>
          </span>
        )}
        <span className="flex items-center gap-1.5 text-xs text-muted-foreground/70">
          <Key className="h-3 w-3 shrink-0" />
          <span className="font-mono truncate">
            {provider.api_key ?? (provider.has_api_key ? t("providers.api_key_configured") : t("providers.api_key_not_set"))}
          </span>
        </span>
        {cap !== "text" && (
          <Badge variant="secondary" className={`text-[10px] px-1.5 py-0 ${provider.enabled ? "text-success" : "text-muted-foreground"}`}>
            {provider.enabled ? t("providers.status_enabled") : t("providers.status_disabled")}
          </Badge>
        )}
      </div>

      {/* Active toggle — capabilities only */}
      {isCapabilityGroup && (
        <label className="flex items-center gap-1.5 cursor-pointer select-none shrink-0">
          <Switch checked={isActive} onCheckedChange={onToggleActive} aria-label={t("providers.active_toggle")} />
          <span className="text-xs text-muted-foreground">{t("providers.active_toggle")}</span>
        </label>
      )}

      {/* Actions */}
      <div className="flex items-center gap-2 shrink-0">
        <Button variant="outline" size="sm" className="h-7 text-xs" onClick={onEdit}>
          <Pencil className="h-3 w-3" />
          {t("common.edit")}
        </Button>
        <Button variant="outline" size="sm" className="h-7 text-xs text-destructive hover:text-destructive" onClick={onDelete} aria-label={t("common.delete")}>
          <Trash2 className="h-3 w-3" />
        </Button>
      </div>
    </div>
  );
});
