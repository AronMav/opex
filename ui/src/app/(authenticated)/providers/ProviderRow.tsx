"use client";

import React, { forwardRef } from "react";
import { useTranslation } from "@/hooks/use-translation";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Card } from "@/components/ui/card";
import { IconTile } from "@/components/ui/icon-tile";
import { StatusBadge } from "@/components/ui/status-badge";
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

// Built on <Card> (not <DataRow>) so the dnd node ref + drag transform style
// thread through to the DOM element — DataRow does not forward a ref. The
// markup mirrors DataRow's leading/title/meta/actions structure.
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
    <Card
      ref={ref}
      interactive
      style={style}
      className={`group relative flex flex-col md:flex-row md:items-center gap-3 p-4 ${isActive ? "ring-1 ring-primary/30" : ""} ${isDragging ? "opacity-80 shadow-lg" : ""} ${!isActive && isCapabilityGroup ? "opacity-60" : ""}`}
    >
      {/* Drag handle — active capability rows only */}
      {draggable && (
        <Button
          type="button"
          variant="ghost"
          size="icon-sm"
          className="tap-target shrink-0 cursor-grab active:cursor-grabbing touch-none text-muted-foreground/50 hover:text-muted-foreground"
          aria-label={t("providers.drag_handle_aria")}
          {...dragHandleAttributes}
          {...dragHandleListeners}
        >
          <GripVertical className="h-4 w-4" />
        </Button>
      )}

      {/* Identity */}
      <div className="flex items-center gap-3 md:min-w-60">
        <IconTile tone="muted" size="sm">
          {CATEGORY_ICONS[cap] ?? <Link2 className="h-4 w-4" />}
        </IconTile>
        <div className="flex flex-col min-w-0">
          <div className="flex items-center gap-1.5 min-w-0">
            <span className="font-semibold text-sm font-mono truncate">{provider.name}</span>
            {isActive && (
              <Badge variant="outline-primary" size="xs">
                {t("providers.active_badge")}
              </Badge>
            )}
          </div>
          <div className="flex items-center gap-1.5 mt-0.5 flex-wrap">
            <Badge variant="secondary" size="sm" className="font-mono">{typeLabel}</Badge>
            {provider.default_model && (
              <span className="text-2xs text-muted-foreground font-mono truncate">{provider.default_model}</span>
            )}
          </div>
        </div>
      </div>

      {/* Meta: base_url + api key + enabled */}
      <div className="flex flex-1 flex-wrap items-center gap-x-3 gap-y-1 min-w-0">
        {provider.base_url && (
          <span className="flex max-w-full items-center gap-1.5 text-xs text-muted-foreground-subtle font-mono truncate min-w-0">
            <Globe className="h-4 w-4 shrink-0" />
            <span className="truncate">{provider.base_url}</span>
          </span>
        )}
        <span className="flex max-w-full items-center gap-1.5 text-xs text-muted-foreground min-w-0">
          <Key className="h-4 w-4 shrink-0" />
          <span className="font-mono truncate">
            {provider.api_key ?? (provider.has_api_key ? t("providers.api_key_configured") : t("providers.api_key_not_set"))}
          </span>
        </span>
        {cap !== "text" && (
          <StatusBadge status={provider.enabled ? "enabled" : "disabled"} size="sm">
            {provider.enabled ? t("providers.status_enabled") : t("providers.status_disabled")}
          </StatusBadge>
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
        <Button variant="outline" size="sm" className="text-xs" onClick={onEdit}>
          <Pencil className="h-4 w-4" />
          {t("common.edit")}
        </Button>
        <Button variant="outline" size="sm" className="text-xs text-destructive hover:text-destructive" onClick={onDelete} aria-label={t("common.delete")}>
          <Trash2 className="h-4 w-4" />
        </Button>
      </div>
    </Card>
  );
});
