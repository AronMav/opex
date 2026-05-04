"use client";

import { useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { apiGet, apiDelete, apiPut, apiPost, apiPatch } from "@/lib/api";
import { useSkills, useCuratorStatus, useSkillVersions, useCuratorDecisions, useSkillCuratorDecisions, qk } from "@/lib/queries";
import { useTranslation } from "@/hooks/use-translation";
import { relativeTime } from "@/lib/format";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import { ErrorBanner } from "@/components/ui/error-banner";
import { Badge } from "@/components/ui/badge";
import { Skeleton } from "@/components/ui/skeleton";
import { EmptyState } from "@/components/ui/empty-state";
import {
  Sheet, SheetContent, SheetHeader, SheetTitle, SheetDescription,
} from "@/components/ui/sheet";
import {
  BookOpen, Wrench, Zap, Trash2, RefreshCw, Tag,
  Plus, Pencil, ArrowLeft, Save, FileText, History, Archive, ArchiveRestore,
  Lock, LockOpen,
} from "lucide-react";
import { toast } from "sonner";
import {
  AlertDialog, AlertDialogAction, AlertDialogCancel,
  AlertDialogContent, AlertDialogDescription, AlertDialogFooter,
  AlertDialogHeader, AlertDialogTitle,
} from "@/components/ui/alert-dialog";
import type { SkillEntry, CuratorDecision } from "@/types/api";

// ── Types ──────────────────────────────────────────────────────────────────

interface SkillForm {
  name: string;
  description: string;
  triggers: string;
  tools_required: string;
  priority: string;
  instructions: string;
}

const EMPTY_FORM: SkillForm = {
  name: "",
  description: "",
  triggers: "",
  tools_required: "",
  priority: "0",
  instructions: "",
};

type StateFilter = "all" | "active" | "stale" | "archived";

// ── State badge helper ─────────────────────────────────────────────────────

function StateBadge({ state }: { state: SkillEntry["state"] }) {
  if (state === "active") {
    return (
      <Badge className="text-[10px] px-1.5 py-0 bg-green-500/15 text-green-700 dark:text-green-400 border-green-500/30 shrink-0">
        active
      </Badge>
    );
  }
  if (state === "stale") {
    return (
      <Badge className="text-[10px] px-1.5 py-0 bg-amber-500/15 text-amber-700 dark:text-amber-400 border-amber-500/30 shrink-0">
        stale
      </Badge>
    );
  }
  return (
    <Badge className="text-[10px] px-1.5 py-0 bg-muted text-muted-foreground border-border/60 shrink-0">
      archived
    </Badge>
  );
}

// ── Curator decision badge ─────────────────────────────────────────────────

function CuratorDecisionBadge({ decision }: { decision: CuratorDecision | undefined }) {
  if (!decision || decision.action === "archive") return null;

  if (decision.action === "reject") {
    return (
      <Badge
        title={decision.reason ?? ""}
        className="text-[10px] px-1.5 py-0 bg-amber-500/10 text-amber-700 dark:text-amber-400 border-amber-500/20 shrink-0 cursor-help"
      >
        Curator: rejected
      </Badge>
    );
  }

  if (decision.action === "fix") {
    const date = decision.decided_at
      ? new Date(decision.decided_at).toLocaleDateString()
      : "";
    return (
      <Badge
        title={decision.reason ?? ""}
        className="text-[10px] px-1.5 py-0 bg-muted text-muted-foreground border-border/50 shrink-0 cursor-help"
      >
        Curator: fixed · {date}
      </Badge>
    );
  }

  return null;
}

// ── Pin badge ──────────────────────────────────────────────────────────────

function PinBadge() {
  return (
    <Badge className="text-[10px] px-1.5 py-0 bg-blue-500/10 text-blue-700 dark:text-blue-400 border-blue-500/20 shrink-0">
      pinned
    </Badge>
  );
}

// ── Skill history sheet ────────────────────────────────────────────────────

function SkillHistorySheet({ skillName, onClose }: { skillName: string; onClose: () => void }) {
  const qc = useQueryClient();
  const { data, isLoading } = useSkillVersions(skillName);
  const versions = data?.versions ?? [];
  const { data: curatorData } = useSkillCuratorDecisions(skillName);
  const curatorHistory = curatorData?.decisions ?? [];
  const [expandedId, setExpandedId] = useState<string | null>(null);
  const [restoringId, setRestoringId] = useState<string | null>(null);
  const [confirmRestore, setConfirmRestore] = useState<string | null>(null);

  const handleRestore = async (versionId: string) => {
    setRestoringId(versionId);
    try {
      await apiPost(`/api/skills/${encodeURIComponent(skillName)}/versions/${versionId}/restore`, {});
      qc.invalidateQueries({ queryKey: qk.skills });
      qc.invalidateQueries({ queryKey: [...qk.skills, skillName, "versions"] });
      toast.success(`Skill "${skillName}" restored to version`);
      onClose();
    } catch (e) {
      toast.error(String(e));
    } finally {
      setRestoringId(null);
      setConfirmRestore(null);
    }
  };

  return (
    <>
      <Sheet open onOpenChange={(open) => { if (!open) onClose(); }}>
        <SheetContent className="w-full sm:max-w-xl overflow-y-auto">
          <SheetHeader className="mb-4">
            <SheetTitle className="font-mono text-sm">{skillName}</SheetTitle>
            <SheetDescription>Version history — click a version to view content</SheetDescription>
          </SheetHeader>

          {isLoading ? (
            <div className="space-y-3">
              {[1, 2, 3].map((i) => (
                <Skeleton key={i} className="h-20 rounded-lg" />
              ))}
            </div>
          ) : versions.length === 0 ? (
            <p className="text-sm text-muted-foreground py-8 text-center">No version history yet.</p>
          ) : (
            <div className="space-y-2">
              {versions.map((v) => {
                const isExpanded = expandedId === v.id;
                return (
                  <div key={v.id} className="rounded-lg border border-border/60 bg-card/50 overflow-hidden">
                    {/* Header row — click to expand */}
                    <button
                      className="w-full text-left p-3 hover:bg-muted/40 transition-colors"
                      onClick={() => setExpandedId(isExpanded ? null : v.id)}
                    >
                      <div className="flex items-center gap-2 flex-wrap">
                        <Badge variant="secondary" className="font-mono text-[10px] px-1.5 py-0 shrink-0">
                          gen {v.generation}
                        </Badge>
                        {v.evolution_type && (
                          <span className="text-xs font-medium text-foreground/80 truncate">
                            {v.evolution_type}
                          </span>
                        )}
                        <span className="text-xs text-muted-foreground ml-auto shrink-0">
                          {relativeTime(v.created_at)}
                        </span>
                      </div>
                      {v.trigger_reason && (
                        <p className="text-xs text-muted-foreground mt-1 text-left">
                          {v.trigger_reason}
                        </p>
                      )}
                    </button>

                    {/* Expanded content */}
                    {isExpanded && (
                      <div className="border-t border-border/40">
                        <div className="flex items-center justify-between px-3 py-2 bg-muted/20">
                          <span className="text-[10px] font-mono text-muted-foreground/60 truncate">
                            {v.content_hash}
                          </span>
                          <Button
                            size="sm"
                            variant="outline"
                            className="h-6 text-xs px-2 shrink-0"
                            disabled={restoringId === v.id}
                            onClick={() => setConfirmRestore(v.id)}
                          >
                            <ArchiveRestore className="h-3 w-3 mr-1" />
                            Restore
                          </Button>
                        </div>
                        <pre className="p-3 text-[11px] font-mono text-foreground/80 overflow-x-auto max-h-80 overflow-y-auto bg-muted/10 whitespace-pre-wrap break-words">
                          {v.content}
                        </pre>
                      </div>
                    )}
                  </div>
                );
              })}
            </div>
          )}

          {curatorHistory.length > 0 && (
            <div className="mt-6">
              <h3 className="text-xs font-semibold text-muted-foreground uppercase tracking-wider mb-3">
                Curator History
              </h3>
              <div className="space-y-1.5">
                {curatorHistory.map((d) => (
                  <div
                    key={d.id}
                    className="flex items-start gap-2 rounded-md border border-border/40 bg-card/30 px-3 py-2"
                  >
                    <Badge
                      className={
                        d.action === "reject"
                          ? "text-[10px] px-1.5 py-0 bg-amber-500/10 text-amber-700 dark:text-amber-400 border-amber-500/20 shrink-0 mt-0.5"
                          : "text-[10px] px-1.5 py-0 bg-muted text-muted-foreground border-border/50 shrink-0 mt-0.5"
                      }
                    >
                      {d.action}
                    </Badge>
                    <div className="flex-1 min-w-0">
                      {d.reason && (
                        <p className="text-xs text-foreground/70 truncate">{d.reason}</p>
                      )}
                      <p className="text-[10px] text-muted-foreground/60 mt-0.5">
                        {relativeTime(d.decided_at)}
                      </p>
                    </div>
                  </div>
                ))}
              </div>
            </div>
          )}
        </SheetContent>
      </Sheet>

      <AlertDialog open={!!confirmRestore} onOpenChange={(o) => { if (!o) setConfirmRestore(null); }}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Restore this version?</AlertDialogTitle>
            <AlertDialogDescription>
              The current skill content will be saved as a snapshot before being replaced.
              You can undo by restoring from history again.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction
              onClick={() => confirmRestore && handleRestore(confirmRestore)}
            >
              Restore
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </>
  );
}

// ── Curator widget ─────────────────────────────────────────────────────────

function CuratorWidget() {
  const { data: status } = useCuratorStatus();
  const qc = useQueryClient();
  const [running, setRunning] = useState(false);

  if (!status?.enabled) return null;

  const lastRun = status.last_run_at ? relativeTime(status.last_run_at) : "never";

  const runNow = async () => {
    setRunning(true);
    try {
      await apiPost("/api/curator/run");
      toast.success("Curator run started");
      qc.invalidateQueries({ queryKey: qk.curatorStatus });
      qc.invalidateQueries({ queryKey: qk.curatorRuns });
    } catch (e) {
      toast.error(String(e));
    } finally {
      setRunning(false);
    }
  };

  return (
    <div className="rounded-lg border border-border/60 bg-muted/20 px-4 py-3 flex flex-col sm:flex-row sm:items-center justify-between gap-2 sm:gap-4">
      <div className="flex flex-col xs:flex-row xs:items-center gap-1 xs:gap-3 text-sm min-w-0">
        <span className="font-medium shrink-0">Curator</span>
        <span className="text-muted-foreground text-xs truncate">
          Last run: {lastRun} &middot; {status.last_phase1} transitions &middot; {status.last_phase2} repairs &middot; {status.last_phase3} LLM
        </span>
      </div>
      <Button size="sm" variant="outline" onClick={runNow} disabled={running} className="shrink-0 self-end sm:self-auto">
        <RefreshCw className={`h-3 w-3 ${running ? "animate-spin" : ""}`} />
        Run now
      </Button>
    </div>
  );
}

// ── Main page ──────────────────────────────────────────────────────────────

export default function SkillsPage() {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const { data, isLoading: loading, error } = useSkills();
  const allSkills: SkillEntry[] = Array.isArray(data) ? data : [];
  const { data: curatorDecisions } = useCuratorDecisions();

  const [stateFilter, setStateFilter] = useState<StateFilter>("all");
  const [deletePending, setDeletePending] = useState<string | null>(null);
  const [deleteConfirm, setDeleteConfirm] = useState<string | null>(null);
  const [archivePending, setArchivePending] = useState<string | null>(null);
  const [pinPending, setPinPending] = useState<string | null>(null);
  const [historySkill, setHistorySkill] = useState<string | null>(null);

  const [showForm, setShowForm] = useState(false);
  const [editingKey, setEditingKey] = useState<string | null>(null);
  const [form, setForm] = useState<SkillForm>(EMPTY_FORM);
  const [saving, setSaving] = useState(false);

  const skills = stateFilter === "all"
    ? allSkills
    : allSkills.filter((s) => s.state === stateFilter);

  const handleDelete = async (skillName: string) => {
    setDeletePending(skillName);
    try {
      await apiDelete(`/api/skills/${encodeURIComponent(skillName)}`);
      qc.invalidateQueries({ queryKey: qk.skills });
      toast.success(t("skills.skill_deleted", { name: skillName }));
    } catch (e) {
      toast.error(t("skills.skill_delete_error", { error: String(e) }));
    } finally {
      setDeletePending(null);
    }
  };

  const handleToggleArchive = async (skill: SkillEntry) => {
    const newState = skill.state === "archived" ? "active" : "archived";
    setArchivePending(skill.name);
    try {
      await apiPut(`/api/skills/${encodeURIComponent(skill.name)}`, { state: newState });
      qc.invalidateQueries({ queryKey: qk.skills });
      toast.success(newState === "archived"
        ? `Skill "${skill.name}" archived`
        : `Skill "${skill.name}" restored`
      );
    } catch (e) {
      toast.error(String(e));
    } finally {
      setArchivePending(null);
    }
  };

  const handleTogglePin = async (skill: SkillEntry) => {
    const newPinned = !skill.pinned;
    setPinPending(skill.name);
    try {
      await apiPatch(`/api/skills/${encodeURIComponent(skill.name)}/pin`, { pinned: newPinned });
      qc.invalidateQueries({ queryKey: qk.skills });
      toast.success(newPinned
        ? `Skill "${skill.name}" protected from Curator`
        : `Curator protection removed from "${skill.name}"`
      );
    } catch (e) {
      toast.error(String(e));
    } finally {
      setPinPending(null);
    }
  };

  const openNew = () => {
    setForm(EMPTY_FORM);
    setEditingKey(null);
    setShowForm(true);
  };

  const openEdit = async (skill: SkillEntry) => {
    try {
      const data = await apiGet<{
        name: string;
        content: string;
        description?: string;
        triggers?: string[];
        tools_required?: string[];
        priority?: number;
        instructions?: string;
      }>(`/api/skills/${encodeURIComponent(skill.name)}`);
      setForm({
        name: skill.name,
        description: data.description ?? skill.description,
        triggers: (data.triggers ?? skill.triggers).join("\n"),
        tools_required: (data.tools_required ?? skill.tools_required).join("\n"),
        priority: String(data.priority ?? skill.priority ?? 0),
        instructions: data.instructions ?? "",
      });
      setEditingKey(skill.name);
      setShowForm(true);
    } catch (e) {
      toast.error(t("skills.skill_load_error", { error: String(e) }));
    }
  };

  const handleSave = async () => {
    if (!form.name.trim()) { toast.error(t("skills.field_name_required")); return; }
    setSaving(true);
    try {
      await apiPut(`/api/skills/${encodeURIComponent(form.name.trim())}`, {
        description: form.description.trim(),
        triggers: form.triggers.split("\n").map((t) => t.trim()).filter(Boolean),
        tools_required: form.tools_required.split("\n").map((t) => t.trim()).filter(Boolean),
        priority: parseInt(form.priority || "0", 10),
        instructions: form.instructions,
      });
      toast.success(editingKey ? t("skills.skill_updated", { name: form.name }) : t("skills.skill_created", { name: form.name }));
      setShowForm(false);
      qc.invalidateQueries({ queryKey: qk.skills });
    } catch (e) {
      toast.error(t("skills.skill_save_error", { error: String(e) }));
    } finally {
      setSaving(false);
    }
  };

  // ── Form view ──────────────────────────────────────────────────────────────

  if (showForm) {
    return (
      <div className="flex-1 overflow-y-auto p-4 md:p-6 lg:p-8 selection:bg-primary/20">
        <div className="mx-auto max-w-3xl">
          <div className="mb-8 flex items-center gap-3">
            <Button variant="outline" size="sm" onClick={() => setShowForm(false)}>
              <ArrowLeft className="h-3.5 w-3.5" />
              {t("common.back")}
            </Button>
            <div>
              <h2 className="font-display text-lg font-bold tracking-tight text-foreground">
                {editingKey ? t("skills.editing", { name: form.name }) : t("skills.new_skill_title")}
              </h2>
              <span className="text-sm text-muted-foreground">
                {editingKey ? t("skills.editing_subtitle") : t("skills.new_skill_subtitle")}
              </span>
            </div>
          </div>

          <div className="rounded-xl border border-border/60 bg-card/50 p-6 space-y-5">
            <div className="flex flex-col gap-1.5">
              <label className="text-xs font-medium text-muted-foreground">
                {t("skills.field_name")} <span className="text-destructive">*</span>
              </label>
              <Input
                type="text"
                value={form.name}
                onChange={(e) => setForm((f) => ({ ...f, name: e.target.value }))}
                disabled={!!editingKey}
                placeholder="e.g. research-task"
                className="font-mono max-w-md"
              />
            </div>

            <div className="flex flex-col gap-1.5">
              <label className="text-xs font-medium text-muted-foreground">{t("skills.field_description")}</label>
              <Input
                type="text"
                value={form.description}
                onChange={(e) => setForm((f) => ({ ...f, description: e.target.value }))}
                placeholder={t("skills.description_placeholder")}
              />
            </div>

            <div className="grid grid-cols-1 sm:grid-cols-2 gap-4">
              <div className="flex flex-col gap-1.5">
                <label className="text-xs font-medium text-muted-foreground">
                  {t("skills.field_triggers")} <span className="text-muted-foreground/50 font-normal">({t("skills.triggers_hint")})</span>
                </label>
                <Textarea
                  value={form.triggers}
                  onChange={(e) => setForm((f) => ({ ...f, triggers: e.target.value }))}
                  placeholder={"research\ninvestigate\nfind information"}
                  rows={4}
                  className="resize-none font-mono"
                />
              </div>
              <div className="flex flex-col gap-1.5">
                <label className="text-xs font-medium text-muted-foreground">
                  {t("skills.field_tools_required")} <span className="text-muted-foreground/50 font-normal">({t("skills.tools_hint")})</span>
                </label>
                <Textarea
                  value={form.tools_required}
                  onChange={(e) => setForm((f) => ({ ...f, tools_required: e.target.value }))}
                  placeholder={"web_search\nmemory\nworkspace_write"}
                  rows={4}
                  className="resize-none font-mono"
                />
              </div>
            </div>

            <div className="flex flex-col gap-1.5 max-w-48">
              <label className="text-xs font-medium text-muted-foreground">{t("skills.field_priority")}</label>
              <Input
                type="number"
                value={form.priority}
                onChange={(e) => setForm((f) => ({ ...f, priority: e.target.value }))}
                min={0}
              />
              <p className="text-xs text-muted-foreground/60">{t("skills.priority_hint")}</p>
            </div>

            <div className="flex flex-col gap-1.5">
              <label className="text-xs font-medium text-muted-foreground">
                {t("skills.field_instructions")} <span className="text-muted-foreground/50 font-normal">({t("skills.instructions_hint")})</span>
              </label>
              <Textarea
                value={form.instructions}
                onChange={(e) => setForm((f) => ({ ...f, instructions: e.target.value }))}
                placeholder={"## Step 1\nDo this first...\n\n## Step 2\nThen do this..."}
                rows={14}
                className="resize-y font-mono"
              />
            </div>
          </div>

          <div className="mt-4 flex flex-col-reverse sm:flex-row sm:items-center sm:justify-end gap-2 sm:gap-3">
            <Button variant="ghost" onClick={() => setShowForm(false)} className="w-full sm:w-auto">
              {t("common.cancel")}
            </Button>
            <Button onClick={handleSave} disabled={saving} className="w-full sm:w-auto">
              <Save className="h-4 w-4" />
              {saving ? t("skills.saving") : t("skills.save_skill")}
            </Button>
          </div>
        </div>
      </div>
    );
  }

  // ── List view ──────────────────────────────────────────────────────────────

  const STATE_FILTERS: { value: StateFilter; label: string }[] = [
    { value: "all", label: "All" },
    { value: "active", label: "Active" },
    { value: "stale", label: "Stale" },
    { value: "archived", label: "Archived" },
  ];

  return (
    <div className="flex flex-col gap-6 p-4 md:p-6 lg:p-8 selection:bg-primary/20">
      {/* Header */}
      <div className="flex flex-col md:flex-row md:items-start justify-between gap-4">
        <div className="flex flex-col gap-1">
          <h2 className="font-display text-lg font-bold tracking-tight">{t("skills.title")}</h2>
          <span className="text-sm text-muted-foreground">{t("skills.subtitle")}</span>
        </div>
        <div className="flex items-center gap-2">
          <Button
            variant="outline"
            size="sm"
            onClick={() => qc.invalidateQueries({ queryKey: qk.skills })}
            disabled={loading}
          >
            <RefreshCw className={`h-3.5 w-3.5 ${loading ? "animate-spin" : ""}`} />
            {t("common.refresh")}
          </Button>
          <Button size="sm" onClick={openNew}>
            <Plus className="h-3.5 w-3.5" />
            {t("skills.new_skill")}
          </Button>
        </div>
      </div>

      {/* State filter */}
      <div className="flex items-center gap-1.5">
        {STATE_FILTERS.map((f) => (
          <Button
            key={f.value}
            variant={stateFilter === f.value ? "secondary" : "ghost"}
            size="sm"
            className="h-7 text-xs"
            onClick={() => setStateFilter(f.value)}
          >
            {f.label}
            {f.value !== "all" && (
              <span className="ml-1.5 text-[10px] tabular-nums text-muted-foreground">
                {allSkills.filter((s) => s.state === f.value).length}
              </span>
            )}
          </Button>
        ))}
      </div>

      {/* Curator widget */}
      <CuratorWidget />

      {error && <ErrorBanner error={String(error)} />}

      {loading ? (
        <div className="space-y-4">
          {[1, 2, 3].map((i) => (
            <Skeleton key={i} className="h-32 rounded-xl border border-border bg-muted/20" />
          ))}
        </div>
      ) : skills.length === 0 ? (
        <EmptyState icon={BookOpen} text={t("skills.no_skills")} hint={
          <p className="text-xs text-muted-foreground/60 mt-1">
            {t("skills.no_skills_hint_prefix")}<span className="font-mono">skill(action=&quot;create&quot;)</span>{t("skills.no_skills_hint_middle")}
            <Button variant="link" onClick={openNew} className="p-0 h-auto">{t("skills.no_skills_hint_link")}</Button>
          </p>
        } />
      ) : (
        <div className="space-y-3">
          {skills.map((skill) => {
            const isPending = deletePending === skill.name;
            const isArchivePending = archivePending === skill.name;
            const isArchived = skill.state === "archived";

            return (
              <div key={skill.name} className={`rounded-xl border border-border/60 bg-card/50 p-5 space-y-4 ${isArchived ? "opacity-60" : ""}`}>
                {/* Header */}
                <div className="flex items-start gap-3">
                  <div className="flex items-center justify-center h-10 w-10 rounded-lg bg-primary/10 border border-primary/20 shrink-0">
                    <BookOpen className="h-4.5 w-4.5 text-primary" />
                  </div>
                  <div className="flex-1 min-w-0">
                    <div className="flex items-center gap-2 flex-wrap">
                      <span className="font-mono text-sm font-semibold text-foreground truncate">
                        {skill.name}
                      </span>
                      <StateBadge state={skill.state} />
                      <CuratorDecisionBadge decision={curatorDecisions?.[skill.name]} />
                      {skill.pinned && <PinBadge />}
                      {skill.priority > 0 && (
                        <Badge variant="secondary" className="text-[10px] px-1.5 py-0 font-mono shrink-0">
                          p:{skill.priority}
                        </Badge>
                      )}
                    </div>
                    {skill.description && (
                      <p className="text-xs text-muted-foreground mt-0.5 line-clamp-2">{skill.description}</p>
                    )}
                  </div>
                </div>

                {/* Triggers */}
                {skill.triggers.length > 0 && (
                  <div className="flex flex-wrap items-start gap-2">
                    <div className="flex items-center gap-1.5 shrink-0 pt-0.5">
                      <Zap className="h-3 w-3 text-warning" />
                      <span className="text-xs text-muted-foreground font-medium">{t("skills.triggers_label")}</span>
                    </div>
                    <div className="flex flex-wrap gap-1.5">
                      {skill.triggers.map((tr) => (
                        <span key={tr} className="inline-flex items-center gap-1 rounded-md border border-border/60 bg-muted/30 px-2 py-0.5 text-xs text-foreground/80">
                          <Tag className="h-2.5 w-2.5 text-muted-foreground" />
                          {tr}
                        </span>
                      ))}
                    </div>
                  </div>
                )}

                {/* Tools required */}
                {skill.tools_required.length > 0 && (
                  <div className="flex flex-wrap items-start gap-2">
                    <div className="flex items-center gap-1.5 shrink-0 pt-0.5">
                      <Wrench className="h-3 w-3 text-primary" />
                      <span className="text-xs text-muted-foreground font-medium">{t("skills.tools_label")}</span>
                    </div>
                    <div className="flex flex-wrap gap-1.5">
                      {skill.tools_required.map((tr) => (
                        <span key={tr} className="inline-flex items-center rounded-md border border-primary/20 bg-primary/5 px-2 py-0.5 text-xs font-mono text-primary/80">
                          {tr}
                        </span>
                      ))}
                    </div>
                  </div>
                )}

                {/* Footer: instructions size + actions */}
                <div className="flex flex-col xs:flex-row xs:items-center justify-between gap-2 pt-1 border-t border-border/30">
                  <div className="flex items-center gap-1.5 min-w-0">
                    <FileText className="h-3 w-3 text-muted-foreground/50 shrink-0" />
                    <span className="text-xs text-muted-foreground/60 truncate">
                      {t("skills.instructions_size")} {t("skills.instructions_chars", { count: skill.instructions_len.toLocaleString() })}
                    </span>
                    {skill.last_used_at && (
                      <span className="text-xs text-muted-foreground/50 shrink-0">
                        &middot; used {relativeTime(skill.last_used_at)}
                      </span>
                    )}
                  </div>
                  <div className="flex items-center gap-1 shrink-0 self-end xs:self-auto">
                    <Button
                      variant="outline"
                      size="sm"
                      onClick={() => setHistorySkill(skill.name)}
                      className="h-7 text-xs"
                      title="Version history"
                    >
                      <History className="h-3 w-3" />
                    </Button>
                    <Button
                      variant="outline"
                      size="sm"
                      disabled={pinPending === skill.name}
                      onClick={() => handleTogglePin(skill)}
                      className="h-7 text-xs"
                      title={skill.pinned ? "Remove Curator protection" : "Protect from Curator"}
                    >
                      {skill.pinned
                        ? <Lock className="h-3 w-3 text-blue-500" />
                        : <LockOpen className="h-3 w-3" />
                      }
                    </Button>
                    <Button
                      variant="outline"
                      size="sm"
                      onClick={() => openEdit(skill)}
                      className="h-7 text-xs"
                    >
                      <Pencil className="h-3 w-3" />
                      <span className="hidden sm:inline">{t("common.edit")}</span>
                    </Button>
                    <Button
                      variant="outline"
                      size="sm"
                      disabled={isArchivePending}
                      onClick={() => handleToggleArchive(skill)}
                      className="h-7 text-xs"
                      title={isArchived ? "Restore" : "Archive"}
                    >
                      {isArchived
                        ? <ArchiveRestore className="h-3 w-3" />
                        : <Archive className="h-3 w-3" />
                      }
                    </Button>
                    <Button
                      variant="outline"
                      size="sm"
                      disabled={isPending}
                      onClick={() => setDeleteConfirm(skill.name)}
                      className="h-7 text-xs text-destructive hover:text-destructive"
                    >
                      <Trash2 className="h-3 w-3" />
                      <span className="hidden sm:inline">{t("common.delete")}</span>
                    </Button>
                  </div>
                </div>
              </div>
            );
          })}
        </div>
      )}

      <AlertDialog open={!!deleteConfirm} onOpenChange={(o) => !o && setDeleteConfirm(null)}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>{t("skills.delete_skill_confirm_title")}</AlertDialogTitle>
            <AlertDialogDescription>
              {t("skills.delete_skill_confirm_description", { name: deleteConfirm ?? "" })}
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>{t("common.cancel")}</AlertDialogCancel>
            <AlertDialogAction
              variant="destructive"
              onClick={() => {
                if (deleteConfirm) {
                  handleDelete(deleteConfirm);
                  setDeleteConfirm(null);
                }
              }}
            >
              {t("common.delete")}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>

      {historySkill && (
        <SkillHistorySheet skillName={historySkill} onClose={() => setHistorySkill(null)} />
      )}
    </div>
  );
}
