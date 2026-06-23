"use client";

import React from "react";
import { useTranslation } from "@/hooks/use-translation";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Switch } from "@/components/ui/switch";
import { Star, Pencil, Trash2 } from "lucide-react";
import type { FileScenario } from "@/types/api";

interface ScenarioRowProps {
  scenario: FileScenario;
  onToggleDefault: () => void;
  onToggleEnabled: (enabled: boolean) => void;
  onEdit: () => void;
  onDelete: () => void;
}

export function ScenarioRow({
  scenario,
  onToggleDefault,
  onToggleEnabled,
  onEdit,
  onDelete,
}: ScenarioRowProps) {
  const { t } = useTranslation();
  return (
    <div className="flex flex-col gap-2.5 rounded-lg border bg-card p-3 sm:flex-row sm:items-center sm:gap-3">
      {/* Info — fills remaining space, text truncates, badges wrap (never overlap) */}
      <div className="flex min-w-0 flex-1 items-start gap-2.5">
        <span className="mt-0.5 w-7 shrink-0 text-right font-mono text-[10px] leading-5 text-muted-foreground">
          {scenario.priority}
        </span>
        <div className="flex min-w-0 flex-col gap-0.5">
          <div className="flex flex-wrap items-center gap-1.5">
            <span className="truncate font-medium">{scenario.label}</span>
            <Badge variant="outline" className="shrink-0 text-[10px]">
              {scenario.executor}
            </Badge>
            {scenario.is_default && (
              <Badge variant="secondary" className="shrink-0 text-[10px]">
                {t("file_scenarios.default_badge")}
              </Badge>
            )}
            {!scenario.enabled && (
              <Badge variant="outline" className="shrink-0 text-[10px]">
                {t("file_scenarios.disabled_badge")}
              </Badge>
            )}
          </div>
          <span className="truncate font-mono text-xs text-muted-foreground">
            {scenario.action_ref}
          </span>
        </div>
      </div>

      {/* Controls — right-aligned row below the info on mobile, inline on sm+ */}
      <div className="flex shrink-0 items-center justify-end gap-1 self-stretch border-t pt-2 sm:self-auto sm:border-t-0 sm:pt-0">
        <Button
          variant={scenario.is_default ? "default" : "ghost"}
          size="icon"
          className="h-8 w-8"
          onClick={onToggleDefault}
          aria-label={t("file_scenarios.set_default")}
        >
          <Star className={`h-4 w-4 ${scenario.is_default ? "fill-current" : ""}`} />
        </Button>
        <Switch
          checked={scenario.enabled}
          onCheckedChange={onToggleEnabled}
          aria-label={t("file_scenarios.toggle_enabled")}
        />
        <Button
          variant="ghost"
          size="icon"
          className="h-8 w-8"
          onClick={onEdit}
          aria-label={t("common.edit")}
        >
          <Pencil className="h-4 w-4" />
        </Button>
        <Button
          variant="ghost"
          size="icon"
          className="h-8 w-8"
          onClick={onDelete}
          aria-label={t("common.delete")}
        >
          <Trash2 className="h-4 w-4" />
        </Button>
      </div>
    </div>
  );
}
