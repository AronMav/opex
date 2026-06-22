"use client";

import React from "react";
import { useTranslation } from "@/hooks/use-translation";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogFooter,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import type { CreateFileScenarioInput, UpdateFileScenarioInput } from "@/types/api";
import { isAllowlistViolation, buildScenarioBody } from "./_parts/helpers";

// ── Props ───────────────────────────────────────────────────────────────────

/**
 * onSave contract (for Task 8.6 wiring):
 *   - CREATE mode: called with `CreateFileScenarioInput` (full payload via buildScenarioBody)
 *   - EDIT mode:   called with `UpdateFileScenarioInput` (only label/priority/enabled)
 *
 * The dialog shapes the payload internally so callers never accidentally send
 * structural fields (match_type/executor/action_ref) on a PUT request.
 */
type OnSaveArg = CreateFileScenarioInput | UpdateFileScenarioInput;

interface ScenarioDialogProps {
  open: boolean;
  /** true = edit mode (structural fields are immutable). false = create mode. */
  editing: boolean;
  form: CreateFileScenarioInput;
  setForm: (f: CreateFileScenarioInput) => void;
  saving: boolean;
  /** Receives the shaped payload — CREATE: full body; EDIT: {label,priority,enabled} only. */
  onSave: (payload: OnSaveArg) => void;
  onClose: () => void;
}

// ── Component ───────────────────────────────────────────────────────────────

export function ScenarioDialog({
  open,
  editing,
  form,
  setForm,
  saving,
  onSave,
  onClose,
}: ScenarioDialogProps) {
  const { t } = useTranslation();

  // Skill bindings can never be default (backend rule: is_default requires executor="tool").
  // Tool bindings must also pass the allowlist check.
  const isSkill = form.executor === "skill";
  const defaultBlocked = isSkill || isAllowlistViolation(form.executor, true, form.action_ref);

  const valid =
    form.match_type.trim().length > 0 &&
    form.action_ref.trim().length > 0 &&
    form.label.trim().length > 0;

  // Shape the payload by mode so the caller never accidentally sends structural
  // fields on a PUT (edit) request.
  function handleSave() {
    if (editing) {
      const payload: UpdateFileScenarioInput = {
        label: form.label.trim(),
        priority: form.priority ?? 100,
        enabled: form.enabled ?? true,
      };
      onSave(payload);
    } else {
      onSave(buildScenarioBody(form));
    }
  }

  // IDs for label association
  const matchId = React.useId();
  const actionId = React.useId();
  const labelId = React.useId();
  const prioId = React.useId();
  const defaultId = React.useId();
  const enabledId = React.useId();
  const executorId = React.useId();

  return (
    <Dialog open={open} onOpenChange={(o) => { if (!o) onClose(); }}>
      <DialogContent className="max-w-md">
        <DialogHeader>
          <DialogTitle>
            {editing ? t("file_scenarios.edit_title") : t("file_scenarios.create_title")}
          </DialogTitle>
        </DialogHeader>

        <div className="flex flex-col gap-3">
          {/* match_type — structural, immutable in edit mode */}
          <div className="space-y-1.5">
            <label htmlFor={matchId} className="text-xs font-medium text-muted-foreground">
              {t("file_scenarios.match_type")}
            </label>
            <Input
              id={matchId}
              value={form.match_type}
              placeholder="image/* | application/pdf | .mp4"
              disabled={editing}
              onChange={(e) => setForm({ ...form, match_type: e.target.value })}
            />
          </div>

          {/* executor — structural, immutable in edit mode */}
          <div className="space-y-1.5">
            <label htmlFor={executorId} className="text-xs font-medium text-muted-foreground">
              {t("file_scenarios.executor")}
            </label>
            {editing ? (
              <Input id={executorId} value={form.executor} disabled />
            ) : (
              <Select
                value={form.executor}
                onValueChange={(v) =>
                  setForm({
                    ...form,
                    executor: v as "tool" | "skill",
                    // skill bindings cannot be default
                    is_default: v === "skill" ? false : form.is_default,
                  })
                }
              >
                <SelectTrigger id={executorId} className="text-sm w-full">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="tool">tool</SelectItem>
                  <SelectItem value="skill">skill</SelectItem>
                </SelectContent>
              </Select>
            )}
          </div>

          {/* action_ref — structural, immutable in edit mode */}
          <div className="space-y-1.5">
            <label htmlFor={actionId} className="text-xs font-medium text-muted-foreground">
              {t("file_scenarios.action_ref")}
            </label>
            <Input
              id={actionId}
              value={form.action_ref}
              disabled={editing}
              onChange={(e) => setForm({ ...form, action_ref: e.target.value })}
            />
          </div>

          {/* label — mutable in both modes */}
          <div className="space-y-1.5">
            <label htmlFor={labelId} className="text-xs font-medium text-muted-foreground">
              {t("file_scenarios.label")}
            </label>
            <Input
              id={labelId}
              value={form.label}
              onChange={(e) => setForm({ ...form, label: e.target.value })}
            />
          </div>

          {/* priority — mutable in both modes */}
          <div className="space-y-1.5">
            <label htmlFor={prioId} className="text-xs font-medium text-muted-foreground">
              {t("file_scenarios.priority")}
            </label>
            <Input
              id={prioId}
              type="number"
              value={form.priority ?? 100}
              onChange={(e) => setForm({ ...form, priority: Number(e.target.value) })}
            />
          </div>

          {/* is_default — gated by allowlist; only in create mode for now */}
          <div className="flex items-center justify-between">
            <label htmlFor={defaultId} className="text-xs font-medium text-muted-foreground">
              {t("file_scenarios.is_default")}
            </label>
            <Switch
              id={defaultId}
              aria-label={t("file_scenarios.is_default")}
              checked={!!form.is_default}
              disabled={defaultBlocked}
              onCheckedChange={(v) => setForm({ ...form, is_default: v })}
            />
          </div>
          {defaultBlocked && (
            <p className="text-xs text-destructive -mt-1">
              {isSkill
                ? t("file_scenarios.default_not_skill")
                : t("file_scenarios.default_not_allowlisted")}
            </p>
          )}

          {/* enabled — mutable in both modes */}
          <div className="flex items-center justify-between">
            <label htmlFor={enabledId} className="text-xs font-medium text-muted-foreground">
              {t("file_scenarios.enabled")}
            </label>
            <Switch
              id={enabledId}
              aria-label={t("file_scenarios.enabled")}
              checked={form.enabled ?? true}
              onCheckedChange={(v) => setForm({ ...form, enabled: v })}
            />
          </div>
        </div>

        <DialogFooter>
          <Button variant="ghost" onClick={onClose}>
            {t("common.cancel")}
          </Button>
          <Button onClick={handleSave} disabled={saving || !valid}>
            {editing ? t("common.save") : t("common.create")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
