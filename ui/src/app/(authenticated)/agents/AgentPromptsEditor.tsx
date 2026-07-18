"use client";

import { useEffect, useRef, useState } from "react";
import { toast } from "sonner";
import { Plus, Trash2 } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { useTranslation } from "@/hooks/use-translation";
import { apiPut } from "@/lib/api";
import { queryClient } from "@/lib/query-client";
import { encodeWorkspacePath } from "@/app/(authenticated)/workspace/file-ops";
import {
  agentPromptsKey,
  agentPromptsPath,
  serializePrompts,
  useAgentPrompts,
  type PromptEntry,
} from "@/lib/prompts";

/**
 * Editor for an agent's starter-prompt suggestions — the buttons shown on the
 * chat welcome screen. Persists to `workspace/agents/{agent}/prompts.md` (the
 * same `## Title` + body format the welcome screen parses) via the workspace
 * file API, decoupled from the main agent-config PUT: it has its own load +
 * Save button. Only the first 3 entries appear as chips on the welcome screen.
 */
export function AgentPromptsEditor({ agentName }: { agentName: string | null }) {
  const { t } = useTranslation();
  const { prompts, isLoading } = useAgentPrompts(agentName);
  const [rows, setRows] = useState<PromptEntry[]>([]);
  const [saving, setSaving] = useState(false);
  // Seed local editable rows once per agent, after the fetch settles — so a
  // background refetch never clobbers in-progress edits.
  const seededRef = useRef<string | null>(null);

  useEffect(() => {
    if (!agentName) {
      setRows([]);
      seededRef.current = null;
      return;
    }
    if (isLoading || seededRef.current === agentName) return;
    setRows(prompts.map((p) => ({ ...p })));
    seededRef.current = agentName;
  }, [agentName, isLoading, prompts]);

  if (!agentName) {
    return <p className="text-xs text-muted-foreground-subtle py-2">{t("agents.prompts_save_agent_first")}</p>;
  }

  const updateRow = (i: number, patch: Partial<PromptEntry>) =>
    setRows((r) => r.map((row, idx) => (idx === i ? { ...row, ...patch } : row)));
  const removeRow = (i: number) => setRows((r) => r.filter((_, idx) => idx !== i));
  const addRow = () => setRows((r) => [...r, { title: "", body: "" }]);

  const save = async () => {
    setSaving(true);
    try {
      const content = serializePrompts(rows);
      await apiPut(`/api/workspace/${encodeWorkspacePath(agentPromptsPath(agentName))}`, { content });
      await queryClient.invalidateQueries({ queryKey: agentPromptsKey(agentName) });
      toast.success(t("agents.prompts_saved"));
    } catch {
      toast.error(t("agents.prompts_save_error"));
    }
    setSaving(false);
  };

  return (
    <div className="space-y-3">
      <div className="space-y-1">
        <h3 className="text-xs font-semibold uppercase tracking-wide text-foreground">{t("agents.prompts_title")}</h3>
        <p className="text-xs text-muted-foreground-subtle">{t("agents.prompts_hint")}</p>
      </div>

      {rows.length === 0 ? (
        <p className="text-xs text-muted-foreground-subtle py-2">{t("agents.prompts_empty")}</p>
      ) : (
        <div className="space-y-3">
          {rows.map((row, i) => (
            <div key={i} className="space-y-2 rounded-lg border border-border bg-muted/20 p-3">
              <div className="flex items-center gap-2">
                <Input
                  value={row.title}
                  onChange={(e) => updateRow(i, { title: e.target.value })}
                  placeholder={t("agents.prompt_title_placeholder")}
                  className="h-8 bg-background border-border text-sm"
                />
                <Button
                  variant="ghost"
                  size="icon"
                  onClick={() => removeRow(i)}
                  aria-label={t("agents.prompt_remove_aria")}
                  className="h-8 w-8 shrink-0 text-muted-foreground hover:text-destructive"
                >
                  <Trash2 className="h-4 w-4" />
                </Button>
              </div>
              <textarea
                className="w-full rounded-md border border-input bg-background px-3 py-2 text-sm"
                rows={2}
                value={row.body}
                onChange={(e) => updateRow(i, { body: e.target.value })}
                placeholder={t("agents.prompt_body_placeholder")}
              />
            </div>
          ))}
        </div>
      )}

      <div className="flex items-center justify-between gap-2">
        <Button variant="outline" size="sm" onClick={addRow} className="gap-1.5">
          <Plus className="h-4 w-4" />
          {t("agents.prompt_add")}
        </Button>
        <Button size="sm" onClick={save} disabled={saving} className="font-semibold">
          {saving ? t("common.saving") : t("agents.prompts_save")}
        </Button>
      </div>
    </div>
  );
}
