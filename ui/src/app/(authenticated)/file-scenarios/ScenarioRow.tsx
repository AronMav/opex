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
    <div className="flex items-center gap-3 rounded-lg border bg-card px-3 py-2">
      <span className="text-[10px] font-mono text-muted-foreground w-10 text-right">
        {scenario.priority}
      </span>
      <div className="flex flex-col min-w-0 flex-1">
        <div className="flex items-center gap-2">
          <span className="font-medium truncate">{scenario.label}</span>
          {scenario.is_default && (
            <Badge variant="secondary" className="text-[10px]">
              {t("file_scenarios.default_badge")}
            </Badge>
          )}
          {!scenario.enabled && (
            <Badge variant="outline" className="text-[10px]">
              {t("file_scenarios.disabled_badge")}
            </Badge>
          )}
        </div>
        <span className="text-xs text-muted-foreground font-mono truncate">
          {scenario.action_ref}
        </span>
      </div>
      <Badge variant="outline" className="text-[10px]">{scenario.executor}</Badge>
      <Button
        variant={scenario.is_default ? "default" : "ghost"}
        size="sm"
        className="gap-1"
        onClick={onToggleDefault}
        aria-label={t("file_scenarios.set_default")}
      >
        <Star className={`h-3.5 w-3.5 ${scenario.is_default ? "fill-current" : ""}`} />
      </Button>
      <Switch
        checked={scenario.enabled}
        onCheckedChange={onToggleEnabled}
        aria-label={t("file_scenarios.toggle_enabled")}
      />
      <Button variant="ghost" size="icon" onClick={onEdit} aria-label={t("common.edit")}>
        <Pencil className="h-4 w-4" />
      </Button>
      <Button variant="ghost" size="icon" onClick={onDelete} aria-label={t("common.delete")}>
        <Trash2 className="h-4 w-4" />
      </Button>
    </div>
  );
}
