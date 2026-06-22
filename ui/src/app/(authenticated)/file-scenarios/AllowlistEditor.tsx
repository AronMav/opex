"use client";

import React from "react";
import { useTranslation } from "@/hooks/use-translation";
import { Switch } from "@/components/ui/switch";
import type { FileScenarioAllowlistRow } from "@/types/api";
import { FSE_ALLOWLIST_MEMBERS } from "./_parts/helpers";

interface AllowlistEditorProps {
  rows: FileScenarioAllowlistRow[];
  onToggle: (action_ref: string, enabled: boolean) => void;
}

export function AllowlistEditor({ rows, onToggle }: AllowlistEditorProps) {
  const { t } = useTranslation();
  const enabledOf = (name: string) =>
    rows.find((r) => r.action_ref === name)?.enabled ?? true;

  return (
    <div className="rounded-xl border bg-card p-4 flex flex-col gap-3">
      <div>
        <h3 className="text-sm font-semibold">{t("file_scenarios.allowlist_title")}</h3>
        <p className="text-xs text-muted-foreground">{t("file_scenarios.allowlist_hint")}</p>
      </div>
      <div className="flex flex-col gap-2">
        {FSE_ALLOWLIST_MEMBERS.map((name) => (
          <div key={name} className="flex items-center justify-between">
            <span className="font-mono text-sm">{name}</span>
            <Switch
              aria-label={name}
              checked={enabledOf(name)}
              onCheckedChange={(v) => onToggle(name, v)}
            />
          </div>
        ))}
      </div>
    </div>
  );
}
