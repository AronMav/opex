"use client";

import React, { useState } from "react";
import { useTranslation } from "@/hooks/use-translation";
import { Button } from "@/components/ui/button";
import { PageHeader } from "@/components/ui/page-header";
import { ErrorBanner } from "@/components/ui/error-banner";
import { Skeleton } from "@/components/ui/skeleton";
import { EmptyState } from "@/components/ui/empty-state";
import { ConfirmDialog } from "@/components/ui/confirm-dialog";
import { Plus, RefreshCw, FileCog } from "lucide-react";
import { toast } from "sonner";
import type { FileScenario, CreateFileScenarioInput, UpdateFileScenarioInput } from "@/types/api";
import {
  useFileScenarios,
  useCreateFileScenario,
  useUpdateFileScenario,
  useDeleteFileScenario,
  useSetFileScenarioDefault,
  useFileScenarioAllowlist,
  useSetFileScenarioAllowlist,
} from "@/lib/queries";
import {
  groupByMatchType,
  sortBindings,
  buildScenarioBody,
  isAllowlistViolation,
  isDefaultIneligible,
} from "./_parts/helpers";
import { ScenarioRow } from "./ScenarioRow";
import { ScenarioDialog } from "./ScenarioDialog";
import { AllowlistEditor } from "./AllowlistEditor";

// ── Re-exports for tests ───────────────────────────────────────────────────────
export { groupByMatchType, sortBindings, buildScenarioBody, isAllowlistViolation, isDefaultIneligible };

// ── Constants ─────────────────────────────────────────────────────────────────

const EMPTY_FORM: CreateFileScenarioInput = {
  match_type: "",
  executor: "tool",
  action_ref: "",
  label: "",
  is_default: false,
  priority: 100,
  enabled: true,
};

// ── Page ───────────────────────────────────────────────────────────────────────

export default function FileScenariosPage() {
  const { t } = useTranslation();

  // ── Data ────────────────────────────────────────────────────────────────────
  const { data: scenarios = [], isLoading, error, refetch } = useFileScenarios();
  const { data: allowlist = [] } = useFileScenarioAllowlist();

  // ── Mutations ────────────────────────────────────────────────────────────────
  const createScenario = useCreateFileScenario();
  const updateScenario = useUpdateFileScenario();
  const deleteScenario = useDeleteFileScenario();
  const setDefault = useSetFileScenarioDefault();
  const setAllowlist = useSetFileScenarioAllowlist();

  // ── Dialog state ─────────────────────────────────────────────────────────────
  const [dialogOpen, setDialogOpen] = useState(false);
  const [editingId, setEditingId] = useState<string | null>(null);
  const [form, setForm] = useState<CreateFileScenarioInput>(EMPTY_FORM);
  const [saving, setSaving] = useState(false);
  const [deleteTarget, setDeleteTarget] = useState<FileScenario | null>(null);

  // ── Derived: live enabled-allowlist Set ──────────────────────────────────────
  // Used by the authoritative set-default gate (carry-forward from task brief).
  const enabledAllowlistSet: ReadonlySet<string> = new Set(
    allowlist.filter((r) => r.enabled).map((r) => r.action_ref),
  );

  // ── Grouped scenarios ────────────────────────────────────────────────────────
  const groups = groupByMatchType(scenarios);

  // ── Dialog handlers ──────────────────────────────────────────────────────────

  const openCreate = () => {
    setEditingId(null);
    setForm(EMPTY_FORM);
    setDialogOpen(true);
  };

  const openEdit = (s: FileScenario) => {
    setEditingId(s.id);
    setForm({
      match_type: s.match_type,
      executor: s.executor,
      action_ref: s.action_ref,
      label: s.label,
      is_default: s.is_default,
      priority: s.priority,
      enabled: s.enabled,
    });
    setDialogOpen(true);
  };

  // onSave handles both create and edit branches (carry-forward §2)
  const onSave = async (payload: CreateFileScenarioInput | UpdateFileScenarioInput) => {
    setSaving(true);
    try {
      if (editingId) {
        // EDIT: payload is UpdateFileScenarioInput (label/priority/enabled only)
        await updateScenario.mutateAsync({ id: editingId, ...(payload as UpdateFileScenarioInput) });
      } else {
        // CREATE: payload is full CreateFileScenarioInput via buildScenarioBody
        await createScenario.mutateAsync(payload as CreateFileScenarioInput);
      }
      setDialogOpen(false);
    } catch (e) {
      toast.error(t("file_scenarios.save_error", { error: String(e) }));
    }
    setSaving(false);
  };

  // ── Toggle default with LIVE allowlist gate (carry-forward §1) ────────────────
  // ScenarioRow fires onToggleDefault unconditionally. This page is the
  // authoritative gate: if the action would SET is_default=true, check the
  // live enabled-allowlist set before calling setDefault.
  const onToggleDefault = (s: FileScenario) => {
    const nextDefault = !s.is_default;
    if (nextDefault && isDefaultIneligible(s.executor, s.action_ref, enabledAllowlistSet)) {
      // Gate: skill binding or tool action not in the ENABLED allowlist → no-op.
      return;
    }
    setDefault.mutate({ id: s.id, is_default: nextDefault });
  };

  // ── Delete ────────────────────────────────────────────────────────────────────

  const confirmDelete = () => {
    if (!deleteTarget) return;
    const target = deleteTarget;
    setDeleteTarget(null);
    deleteScenario.mutate(target.id);
  };

  // ── Allowlist toggle (carry-forward §4) ─────────────────────────────────────
  // AllowlistEditor fires onToggle(action_ref, enabled).
  // useSetFileScenarioAllowlist takes { action_ref, enabled } (single-item PUT).
  const onToggleAllowlist = (action_ref: string, enabled: boolean) => {
    setAllowlist.mutate({ action_ref, enabled });
  };

  // ── Render ────────────────────────────────────────────────────────────────────

  return (
    <div className="flex flex-col gap-8 p-4 md:p-6 lg:p-8">
      <PageHeader
        title={t("file_scenarios.title")}
        description={t("file_scenarios.subtitle")}
        actions={
          <div className="flex items-center gap-2">
            <Button variant="outline" size="sm" onClick={() => refetch()} className="gap-1.5">
              <RefreshCw className="h-4 w-4" />
              {t("common.refresh")}
            </Button>
            <Button size="lg" onClick={openCreate} className="w-full md:w-auto gap-2">
              <Plus className="h-4 w-4" />
              {t("file_scenarios.add")}
            </Button>
          </div>
        }
      />

      {error && <ErrorBanner error={String(error)} />}

      {isLoading ? (
        <div className="flex flex-col gap-3">
          {[1, 2, 3].map((i) => <Skeleton key={i} className="h-16 rounded-lg" />)}
        </div>
      ) : groups.length === 0 ? (
        <EmptyState icon={FileCog} text={t("file_scenarios.empty")} height="h-48" />
      ) : (
        <div className="flex flex-col gap-6">
          {groups.map(({ matchType, bindings }) => (
            <div key={matchType} className="flex flex-col gap-2">
              <h2 className="text-sm font-semibold font-mono text-muted-foreground">
                {matchType}
              </h2>
              <div className="flex flex-col gap-2">
                {bindings.map((s) => (
                  <ScenarioRow
                    key={s.id}
                    scenario={s}
                    onToggleDefault={() => onToggleDefault(s)}
                    onToggleEnabled={(enabled) => updateScenario.mutate({ id: s.id, enabled })}
                    onEdit={() => openEdit(s)}
                    onDelete={() => setDeleteTarget(s)}
                  />
                ))}
              </div>
            </div>
          ))}
        </div>
      )}

      <AllowlistEditor rows={allowlist} onToggle={onToggleAllowlist} />

      <ScenarioDialog
        open={dialogOpen}
        editing={editingId !== null}
        form={form}
        setForm={setForm}
        saving={saving}
        onSave={onSave}
        onClose={() => setDialogOpen(false)}
      />

      <ConfirmDialog
        open={!!deleteTarget}
        onClose={() => setDeleteTarget(null)}
        onConfirm={confirmDelete}
        title={t("file_scenarios.delete_title")}
        description={t("file_scenarios.delete_description")}
      />
    </div>
  );
}
