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
import { PageHeader } from "@/components/ui/page-header";
import { SectionHeader } from "@/components/ui/section-header";
import { Badge } from "@/components/ui/badge";
import { StatusBadge } from "@/components/ui/status-badge";
import { Card } from "@/components/ui/card";
import { PageContainer } from "@/components/ui/page-container";
import { IconTile } from "@/components/ui/icon-tile";
import { Chip } from "@/components/ui/chip";
import { SearchInput } from "@/components/ui/search-input";
import { Tabs } from "@/components/ui/tabs";
import { FilterTabsList, type FilterTabItem } from "@/components/ui/filter-tabs";
import { Skeleton } from "@/components/ui/skeleton";
import { EmptyState } from "@/components/ui/empty-state";
import { Field } from "@/components/ui/field";
import {
  Sheet, SheetContent, SheetHeader, SheetBody, SheetTitle, SheetDescription,
} from "@/components/ui/sheet";
import {
  BookOpen, Wrench, Zap, Trash2, RefreshCw, Tag,
  Plus, Pencil, ArrowLeft, Save, FileText, History, Archive, ArchiveRestore,
  Lock, LockOpen, Search, LayoutList, CircleCheck,
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
  const { t } = useTranslation();
  return (
    <StatusBadge status={state} size="sm">
      {t(`skills.state_${state}`)}
    </StatusBadge>
  );
}

// ── Curator decision badge ─────────────────────────────────────────────────

function CuratorDecisionBadge({ decision }: { decision: CuratorDecision | undefined }) {
  const { t } = useTranslation();
  if (!decision || decision.action === "archive") return null;

  if (decision.action === "reject") {
    return (
      <Badge
        variant="warning"
        size="sm"
        title={decision.reason ?? ""}
        className="cursor-help"
      >
        {t("skills.curator_rejected")}
      </Badge>
    );
  }

  if (decision.action === "fix") {
    const date = decision.decided_at
      ? new Date(decision.decided_at).toLocaleDateString()
      : "";
    return (
      <Badge
        variant="secondary"
        size="sm"
        title={decision.reason ?? ""}
        className="cursor-help"
      >
        {t("skills.curator_fixed")} · {date}
      </Badge>
    );
  }

  return null;
}

// ── Pin badge ──────────────────────────────────────────────────────────────

function PinBadge() {
  const { t } = useTranslation();
  return (
    <Badge variant="default" size="sm">
      {t("skills.badge_pinned")}
    </Badge>
  );
}

// ── Skill history sheet ────────────────────────────────────────────────────

function SkillHistorySheet({ skillName, onClose }: { skillName: string; onClose: () => void }) {
  const { t, locale } = useTranslation();
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
        <SheetContent className="w-full sm:max-w-xl">
          <SheetHeader className="mb-4">
            <SheetTitle className="font-mono text-sm">{skillName}</SheetTitle>
            <SheetDescription>{t("skills.version_description")}</SheetDescription>
          </SheetHeader>

          <SheetBody>
          {isLoading ? (
            <div className="space-y-3 max-w-4xl mx-auto w-full">
              {[1, 2, 3].map((i) => (
                <Skeleton key={i} className="h-20 rounded-lg" />
              ))}
            </div>
          ) : versions.length === 0 ? (
            <p className="text-sm text-muted-foreground py-8 text-center">{t("skills.no_versions")}</p>
          ) : (
            <div className="space-y-2">
              {versions.map((v) => {
                const isExpanded = expandedId === v.id;
                return (
                  <Card key={v.id} className="overflow-hidden min-w-0">
                    {/* Header row — click to expand */}
                    <button
                      className="w-full text-left p-3 hover:bg-muted/30 transition-colors"
                      onClick={() => setExpandedId(isExpanded ? null : v.id)}
                    >
                      <div className="flex items-center gap-2 flex-wrap">
                        <Badge variant="secondary" size="sm" className="font-mono">
                          gen {v.generation}
                        </Badge>
                        {v.evolution_type && (
                          <span className="text-xs font-medium text-foreground/80 truncate">
                            {v.evolution_type}
                          </span>
                        )}
                        <span className="text-xs text-muted-foreground ml-auto shrink-0">
                          {relativeTime(v.created_at, locale)}
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
                      <div className="border-t border-border/30">
                        <div className="flex items-center justify-between px-3 py-2 bg-muted/20">
                          <span className="text-3xs font-mono text-muted-foreground-subtle truncate min-w-0">
                            {v.content_hash}
                          </span>
                          <Button
                            size="sm"
                            variant="outline"
                            className="h-6 text-xs px-2 shrink-0"
                            disabled={restoringId === v.id}
                            onClick={() => setConfirmRestore(v.id)}
                          >
                            <ArchiveRestore className="h-4 w-4 mr-1" />
                            {t("skills.restore")}
                          </Button>
                        </div>
                        <pre className="p-3 text-2xs font-mono text-foreground/80 overflow-x-auto max-h-80 overflow-y-auto bg-muted/10 whitespace-pre-wrap break-words">
                          {v.content}
                        </pre>
                      </div>
                    )}
                  </Card>
                );
              })}
            </div>
          )}

          {curatorHistory.length > 0 && (
            <div className="mt-6">
              <h3 className="text-xs font-semibold text-muted-foreground uppercase tracking-wider mb-3">
                {t("skills.curator_history")}
              </h3>
              <div className="space-y-1.5">
                {curatorHistory.map((d) => (
                  <Card
                    key={d.id}
                    className="flex items-start gap-2 px-3 py-2"
                  >
                    <Badge
                      variant={d.action === "reject" ? "warning" : "secondary"}
                      size="sm"
                      className="mt-0.5"
                    >
                      {d.action}
                    </Badge>
                    <div className="flex-1 min-w-0">
                      {d.reason && (
                        <p className="text-xs text-foreground/80 truncate">{d.reason}</p>
                      )}
                      <p className="text-3xs text-muted-foreground-subtle mt-0.5">
                        {relativeTime(d.decided_at, locale)}
                      </p>
                    </div>
                  </Card>
                ))}
              </div>
            </div>
          )}
          </SheetBody>
        </SheetContent>
      </Sheet>

      <AlertDialog open={!!confirmRestore} onOpenChange={(o) => { if (!o) setConfirmRestore(null); }}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>{t("skills.restore_confirm_title")}</AlertDialogTitle>
            <AlertDialogDescription>
              {t("skills.restore_confirm_desc")}
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>{t("common.cancel")}</AlertDialogCancel>
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
  const { t, locale } = useTranslation();
  const { data: status } = useCuratorStatus();
  const qc = useQueryClient();
  const [running, setRunning] = useState(false);

  if (!status?.enabled) return null;

  const lastRun = status.last_run_at ? relativeTime(status.last_run_at, locale) : "never";

  const runNow = async () => {
    setRunning(true);
    try {
      await apiPost("/api/curator/run");
      toast.success(t("skills.curator_run_started"));
      qc.invalidateQueries({ queryKey: qk.curatorStatus });
      qc.invalidateQueries({ queryKey: qk.curatorRuns });
    } catch (e) {
      toast.error(String(e));
    } finally {
      setRunning(false);
    }
  };

  return (
    <Card className="px-4 py-3 flex flex-col sm:flex-row sm:items-center justify-between gap-2 sm:gap-4">
      <div className="flex flex-col sm:flex-row sm:items-center gap-1 sm:gap-3 text-sm min-w-0">
        <span className="font-medium shrink-0">{t("skills.curator_label")}</span>
        <span className="text-muted-foreground text-xs truncate">
          {t("skills.curator_last_run", { time: lastRun })} &middot; {status.last_phase1} transitions &middot; {status.last_phase2} repairs &middot; {status.last_phase3} LLM
        </span>
      </div>
      <Button size="sm" variant="outline" onClick={runNow} disabled={running} className="shrink-0 self-end sm:self-auto">
        <RefreshCw className={`h-4 w-4 ${running ? "animate-spin" : ""}`} />
        {t("skills.curator_run_now")}
      </Button>
    </Card>
  );
}

// ── Main page ──────────────────────────────────────────────────────────────

export default function SkillsPage() {
  const { t, locale } = useTranslation();
  const qc = useQueryClient();
  const { data, isLoading: loading, error } = useSkills();
  const allSkills: SkillEntry[] = Array.isArray(data) ? data : [];
  const { data: curatorDecisions } = useCuratorDecisions();

  const [stateFilter, setStateFilter] = useState<StateFilter>("all");
  const [skillSearch, setSkillSearch] = useState("");
  const [deletePending, setDeletePending] = useState<string | null>(null);
  const [deleteConfirm, setDeleteConfirm] = useState<string | null>(null);
  const [archivePending, setArchivePending] = useState<string | null>(null);
  const [pinPending, setPinPending] = useState<string | null>(null);
  const [historySkill, setHistorySkill] = useState<string | null>(null);

  const [showForm, setShowForm] = useState(false);
  const [editingKey, setEditingKey] = useState<string | null>(null);
  const [form, setForm] = useState<SkillForm>(EMPTY_FORM);
  const [saving, setSaving] = useState(false);

  const skills = (stateFilter === "all"
    ? allSkills
    : allSkills.filter((s) => s.state === stateFilter)
  ).filter((s) => !skillSearch || s.name.toLowerCase().includes(skillSearch.toLowerCase()));

  // A filter/search is active — an empty result here is "no matches", not onboarding.
  const isFiltered = stateFilter !== "all" || skillSearch.trim() !== "";

  const resetFilters = () => {
    setStateFilter("all");
    setSkillSearch("");
  };

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
      <PageContainer>
        <div className="mx-auto max-w-3xl">
          <div className="mb-8 flex items-center gap-3">
            <Button variant="outline" size="sm" onClick={() => setShowForm(false)}>
              <ArrowLeft className="h-3.5 w-3.5" />
              {t("common.back")}
            </Button>
            <SectionHeader
              className="mb-0 flex-1"
              title={editingKey ? t("skills.editing", { name: form.name }) : t("skills.new_skill_title")}
              description={editingKey ? t("skills.editing_subtitle") : t("skills.new_skill_subtitle")}
            />
          </div>

          <Card className="p-6 space-y-5">
            <Field label={`${t("skills.field_name")} *`} labelClassName="text-xs">
              <Input
                type="text"
                value={form.name}
                onChange={(e) => setForm((f) => ({ ...f, name: e.target.value }))}
                disabled={!!editingKey}
                placeholder="e.g. research-task"
                className="font-mono max-w-md"
              />
            </Field>

            <Field label={t("skills.field_description")} labelClassName="text-xs">
              <Input
                type="text"
                value={form.description}
                onChange={(e) => setForm((f) => ({ ...f, description: e.target.value }))}
                placeholder={t("skills.description_placeholder")}
              />
            </Field>

            <div className="grid grid-cols-1 sm:grid-cols-2 gap-4">
              <Field label={`${t("skills.field_triggers")} (${t("skills.triggers_hint")})`} labelClassName="text-xs">
                <Textarea
                  value={form.triggers}
                  onChange={(e) => setForm((f) => ({ ...f, triggers: e.target.value }))}
                  placeholder={"research\ninvestigate\nfind information"}
                  rows={4}
                  className="resize-none font-mono"
                />
              </Field>
              <Field label={`${t("skills.field_tools_required")} (${t("skills.tools_hint")})`} labelClassName="text-xs">
                <Textarea
                  value={form.tools_required}
                  onChange={(e) => setForm((f) => ({ ...f, tools_required: e.target.value }))}
                  placeholder={"web_search\nmemory\nworkspace_write"}
                  rows={4}
                  className="resize-none font-mono"
                />
              </Field>
            </div>

            <Field label={t("skills.field_priority")} hint={t("skills.priority_hint")} labelClassName="text-xs" className="max-w-48">
              <Input
                type="number"
                value={form.priority}
                onChange={(e) => setForm((f) => ({ ...f, priority: e.target.value }))}
                min={0}
              />
            </Field>

            <Field label={`${t("skills.field_instructions")} (${t("skills.instructions_hint")})`} labelClassName="text-xs">
              <Textarea
                value={form.instructions}
                onChange={(e) => setForm((f) => ({ ...f, instructions: e.target.value }))}
                placeholder={"## Step 1\nDo this first...\n\n## Step 2\nThen do this..."}
                rows={14}
                className="resize-y font-mono"
              />
            </Field>
          </Card>

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
      </PageContainer>
    );
  }

  // ── List view ──────────────────────────────────────────────────────────────

  const STATE_FILTERS: (FilterTabItem & { value: StateFilter })[] = [
    { value: "all", label: t("skills.filter_all"), icon: <LayoutList /> },
    {
      value: "active",
      label: t("skills.filter_active"),
      icon: <CircleCheck />,
      count: allSkills.filter((s) => s.state === "active").length,
    },
    {
      value: "stale",
      label: t("skills.filter_stale"),
      icon: <History />,
      count: allSkills.filter((s) => s.state === "stale").length,
    },
    {
      value: "archived",
      label: t("skills.filter_archived"),
      icon: <Archive />,
      count: allSkills.filter((s) => s.state === "archived").length,
    },
  ];

  return (
    <PageContainer className="flex flex-col gap-6">
      {/* Header */}
      <PageHeader
        title={t("skills.title")}
        description={t("skills.subtitle")}
        actions={
          <div className="flex flex-wrap items-center gap-2 w-full md:w-auto">
            <Button
              variant="outline"
              size="sm"
              onClick={() => qc.invalidateQueries({ queryKey: qk.skills })}
              disabled={loading}
            >
              <RefreshCw className={`h-3.5 w-3.5 ${loading ? "animate-spin" : ""}`} />
              {t("common.refresh")}
            </Button>
            <Button size="lg" onClick={openNew} className="w-full md:w-auto gap-2">
              <Plus className="h-4 w-4" />
              {t("skills.new_skill")}
            </Button>
          </div>
        }
      />

      {/* State filter */}
      <Tabs value={stateFilter} onValueChange={(v) => setStateFilter(v as StateFilter)}>
        <FilterTabsList items={STATE_FILTERS} />
      </Tabs>

      {/* Search */}
      <SearchInput
        value={skillSearch}
        onChange={setSkillSearch}
        placeholder={t("skills.search_placeholder")}
      />

      {/* Curator widget */}
      <CuratorWidget />

      {error && <ErrorBanner error={String(error)} />}

      {loading ? (
        <div className="space-y-4">
          {[1, 2, 3].map((i) => (
            <Skeleton key={i} className="h-32 rounded-xl border border-border bg-muted/20" />
          ))}
        </div>
      ) : skills.length === 0 && isFiltered ? (
        <EmptyState
          icon={Search}
          text={t("skills.no_matches")}
          hint={
            <Button variant="link" onClick={resetFilters} className="p-0 h-auto mt-1">
              {t("skills.reset_filters")}
            </Button>
          }
        />
      ) : skills.length === 0 ? (
        <EmptyState icon={BookOpen} text={t("skills.no_skills")} hint={
          <p className="text-xs text-muted-foreground-subtle mt-1">
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
              <Card key={skill.name} className={`p-5 space-y-4 min-w-0 overflow-hidden ${isArchived ? "opacity-60" : ""}`}>
                {/* Header */}
                <div className="flex items-start gap-3">
                  <IconTile tone="primary" size="md">
                    <BookOpen />
                  </IconTile>
                  <div className="flex-1 min-w-0">
                    <div className="flex items-center gap-2 flex-wrap">
                      <span className="font-mono text-sm font-semibold text-foreground truncate">
                        {skill.name}
                      </span>
                      <StateBadge state={skill.state} />
                      <CuratorDecisionBadge decision={curatorDecisions?.[skill.name]} />
                      {skill.pinned && <PinBadge />}
                      {skill.priority > 0 && (
                        <Badge variant="secondary" size="sm" className="font-mono">
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
                      <Zap className="h-4 w-4 text-warning" />
                      <span className="text-xs text-muted-foreground font-medium">{t("skills.triggers_label")}</span>
                    </div>
                    <div className="flex flex-wrap gap-1.5">
                      {skill.triggers.map((tr) => (
                        <Chip key={tr} tone="default">
                          <Tag className="text-muted-foreground" />
                          {tr}
                        </Chip>
                      ))}
                    </div>
                  </div>
                )}

                {/* Tools required */}
                {skill.tools_required.length > 0 && (
                  <div className="flex flex-wrap items-start gap-2">
                    <div className="flex items-center gap-1.5 shrink-0 pt-0.5">
                      <Wrench className="h-4 w-4 text-primary" />
                      <span className="text-xs text-muted-foreground font-medium">{t("skills.tools_label")}</span>
                    </div>
                    <div className="flex flex-wrap gap-1.5">
                      {skill.tools_required.map((tr) => (
                        <Chip key={tr} tone="primary" className="font-mono">
                          {tr}
                        </Chip>
                      ))}
                    </div>
                  </div>
                )}

                {/* Footer: instructions size + actions */}
                <div className="flex flex-col sm:flex-row sm:items-center justify-between gap-2 pt-1 border-t border-border/30">
                  <div className="flex items-center gap-1.5 min-w-0">
                    <FileText className="h-4 w-4 text-muted-foreground-subtle shrink-0" />
                    <span className="text-xs text-muted-foreground-subtle truncate">
                      {t("skills.instructions_size")} {t("skills.instructions_chars", { count: skill.instructions_len.toLocaleString() })}
                    </span>
                    {skill.last_used_at && (
                      <span className="text-xs text-muted-foreground-subtle shrink-0">
                        &middot; used {relativeTime(skill.last_used_at, locale)}
                      </span>
                    )}
                  </div>
                  <div className="flex items-center gap-1 shrink-0 self-end sm:self-auto">
                    <Button
                      variant="outline"
                      size="sm"
                      onClick={() => setHistorySkill(skill.name)}
                      className="h-7 text-xs"
                      title="Version history"
                    >
                      <History className="h-4 w-4" />
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
                        ? <Lock className="h-4 w-4 text-primary" />
                        : <LockOpen className="h-4 w-4" />
                      }
                    </Button>
                    <Button
                      variant="outline"
                      size="sm"
                      onClick={() => openEdit(skill)}
                      className="h-7 text-xs"
                    >
                      <Pencil className="h-4 w-4" />
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
                        ? <ArchiveRestore className="h-4 w-4" />
                        : <Archive className="h-4 w-4" />
                      }
                    </Button>
                    <Button
                      variant="outline"
                      size="sm"
                      disabled={isPending}
                      onClick={() => setDeleteConfirm(skill.name)}
                      className="h-7 text-xs text-destructive hover:text-destructive"
                    >
                      <Trash2 className="h-4 w-4" />
                      <span className="hidden sm:inline">{t("common.delete")}</span>
                    </Button>
                  </div>
                </div>
              </Card>
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
    </PageContainer>
  );
}
