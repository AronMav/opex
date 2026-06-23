"use client";

import { useCallback, useState, type FormEvent } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { apiGet, apiPost, apiPut, apiDelete } from "@/lib/api";
import { useYamlTools, useMcpServers, qk } from "@/lib/queries";
import { useTranslation } from "@/hooks/use-translation";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import { ErrorBanner } from "@/components/ui/error-banner";
import { PageHeader } from "@/components/ui/page-header";
import { Badge } from "@/components/ui/badge";
import { Skeleton } from "@/components/ui/skeleton";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Tabs, TabsList, TabsTrigger, TabsContent } from "@/components/ui/tabs";
import { EmptyState } from "@/components/ui/empty-state";
import { Button } from "@/components/ui/button";
import {
  Activity, FileCode2, CheckCircle2,
  RefreshCw, Plus, Pencil, Trash2, RotateCcw,
  ArrowLeft, Save, Square, Play,
  ExternalLink,
} from "lucide-react";
import { toast } from "sonner";
import {
  AlertDialog, AlertDialogAction, AlertDialogCancel,
  AlertDialogContent, AlertDialogDescription, AlertDialogFooter,
  AlertDialogHeader, AlertDialogTitle,
} from "@/components/ui/alert-dialog";
import type { McpEntry, YamlToolEntry } from "@/types/api";
import { Field, Row, TypeBadge, StatusBadge } from "./ToolHelpers";

/* ── MCP Form helpers ───────────────────────────────────────────── */

interface McpFormData {
  name: string;
  url: string;
  container: string;
  port: string;
  mode: string;
  protocol: string;
}

const emptyMcpForm = (): McpFormData => ({
  name: "", url: "", container: "", port: "", mode: "on-demand", protocol: "http",
});

function mcpToForm(e: McpEntry): McpFormData {
  return {
    name: e.name, url: e.url ?? "", container: e.container ?? "",
    port: e.port != null ? String(e.port) : "", mode: e.mode,
    protocol: e.protocol,
  };
}

function formToPayload(f: McpFormData) {
  return {
    name: f.name.trim(), url: f.url.trim() || null,
    container: f.container.trim() || null,
    port: f.port.trim() ? Number(f.port) : null,
    mode: f.mode, protocol: f.protocol,
  };
}

type EditView =
  | { kind: "mcp"; id: string }
  | { kind: "yaml"; id: string };

/* ── Page ────────────────────────────────────────────────────────── */

export default function ToolsPage() {
  const { t } = useTranslation();
  const qc = useQueryClient();

  const { data: yamlTools = [], isLoading: yamlLoading2, error: yamlError } = useYamlTools();
  const { data: mcpServers = [], isLoading: mcpLoading, error: mcpError } = useMcpServers();

  const loading = yamlLoading2 || mcpLoading;
  const errorMsg = yamlError ? String(yamlError) : mcpError ? String(mcpError) : "";

  const [actionPending, setActionPending] = useState<string | null>(null);

  // Edit forms (full-page)
  const [editView, setEditView] = useState<EditView | null>(null);
  const [mcpForm, setMcpForm] = useState<McpFormData>(emptyMcpForm());
  const [yamlContent, setYamlContent] = useState("");
  const [yamlLoading, setYamlLoading] = useState(false);
  const [formBusy, setFormBusy] = useState(false);
  const [deleteConfirm, setDeleteConfirm] = useState<{ kind: "mcp" | "yaml"; name: string } | null>(null);

  const invalidateAll = useCallback(() => {
    qc.invalidateQueries({ queryKey: qk.yamlTools });
    qc.invalidateQueries({ queryKey: qk.mcpServers });
  }, [qc]);

  const handleConfirmDelete = async () => {
    if (!deleteConfirm) return;
    const { kind, name } = deleteConfirm;
    setDeleteConfirm(null);
    if (kind === "mcp") await deleteMcp(name);
    else await deleteYamlTool(name);
  };

  /* ── MCP CRUD ─────────────────────────────────────────────────── */
  const startCreateMcp = () => { setMcpForm(emptyMcpForm()); setEditView({ kind: "mcp", id: "new" }); };
  const startEditMcp = (s: McpEntry) => { setMcpForm(mcpToForm(s)); setEditView({ kind: "mcp", id: s.name }); };
  const cancelEdit = () => { setEditView(null); setMcpForm(emptyMcpForm()); setYamlContent(""); };

  const saveMcp = async (e: FormEvent) => {
    e.preventDefault();
    if (!editView || editView.kind !== "mcp") return;
    const payload = formToPayload(mcpForm);
    if (!payload.name) { toast.error(t("tools.mcp_field_name")); return; }
    setFormBusy(true);
    try {
      if (editView.id === "new") {
        await apiPost("/api/mcp", payload);
        toast.success(t("tools.created_toast", { name: payload.name }));
      } else {
        await apiPut(`/api/mcp/${encodeURIComponent(editView.id)}`, payload);
        toast.success(t("tools.updated_toast", { name: payload.name }));
      }
      cancelEdit();
      invalidateAll();
    } catch (e) {
      toast.error(`${e}`);
    } finally {
      setFormBusy(false);
    }
  };

  const deleteMcp = async (name: string) => {
    setActionPending(name);
    try {
      await apiDelete(`/api/mcp/${encodeURIComponent(name)}`);
      qc.invalidateQueries({ queryKey: qk.mcpServers });
      toast.success(t("tools.deleted_toast", { name }));
    } catch (e) {
      toast.error(`${e}`);
    } finally {
      setActionPending(null);
    }
  };

  const toggleMcp = async (name: string) => {
    setActionPending("toggle:" + name);
    try {
      const res = await apiPost<{ enabled: boolean }>(`/api/mcp/${encodeURIComponent(name)}/toggle`, {});
      qc.invalidateQueries({ queryKey: qk.mcpServers });
      toast.success(res.enabled ? t("tools.enabled_toast", { name }) : t("tools.disabled_toast", { name }));
    } catch (e) {
      toast.error(`${e}`);
    } finally {
      setActionPending(null);
    }
  };

  const reloadMcp = async (name: string) => {
    setActionPending("reload:" + name);
    try {
      await apiPost(`/api/mcp/${encodeURIComponent(name)}/reload`, {});
      toast.success(t("tools.updated_toast", { name }));
      invalidateAll();
    } catch (e) {
      toast.error(`${e}`);
    } finally {
      setActionPending(null);
    }
  };

  /* ── YAML actions ──────────────────────────────────────────────── */
  const handleVerify = async (name: string) => {
    setActionPending(name);
    try {
      await apiPost(`/api/yaml-tools/${encodeURIComponent(name)}/verify`, {});
      qc.invalidateQueries({ queryKey: qk.yamlTools });
      toast.success(t("tools.verified_toast", { name }));
    } catch (e) { toast.error(`${e}`); }
    finally { setActionPending(null); }
  };

  const handleDisable = async (name: string) => {
    setActionPending(name);
    try {
      await apiPost(`/api/yaml-tools/${encodeURIComponent(name)}/disable`, {});
      qc.invalidateQueries({ queryKey: qk.yamlTools });
      toast.success(t("tools.disabled_toast", { name }));
    } catch (e) { toast.error(`${e}`); }
    finally { setActionPending(null); }
  };

  const handleEnable = async (name: string) => {
    setActionPending(name);
    try {
      await apiPost(`/api/yaml-tools/${encodeURIComponent(name)}/enable`, {});
      qc.invalidateQueries({ queryKey: qk.yamlTools });
      toast.success(t("tools.enabled_toast", { name }));
    } catch (e) { toast.error(`${e}`); }
    finally { setActionPending(null); }
  };

  const startCreateYaml = () => {
    setYamlContent(`name: my-tool
description: "${t("tools.yaml_description_placeholder")}"
endpoint: "http://example.com/api"
method: GET
parameters:
  q:
    type: string
    required: true
    location: query
    description: "${t("tools.yaml_param_description_placeholder")}"
`);
    setEditView({ kind: "yaml", id: "new" });
  };

  const startEditYaml = async (name: string) => {
    setYamlLoading(true);
    setEditView({ kind: "yaml", id: name });
    try {
      const res = await apiGet<{ content: string }>(`/api/yaml-tools/${encodeURIComponent(name)}`);
      setYamlContent(res.content);
    } catch (e) {
      toast.error(t("tools.load_error", { error: String(e) }));
      setEditView(null);
    } finally {
      setYamlLoading(false);
    }
  };

  const saveYaml = async (e: FormEvent) => {
    e.preventDefault();
    if (!editView || editView.kind !== "yaml") return;
    setFormBusy(true);
    try {
      if (editView.id === "new") {
        const res = await apiPost<{ name: string }>("/api/yaml-tools", { content: yamlContent });
        toast.success(t("tools.created_toast", { name: res.name }));
      } else {
        await apiPut(`/api/yaml-tools/${encodeURIComponent(editView.id)}`, { content: yamlContent });
        toast.success(t("tools.updated_toast", { name: editView.id }));
      }
      cancelEdit();
      qc.invalidateQueries({ queryKey: qk.yamlTools });
    } catch (e) {
      toast.error(`${e}`);
    } finally {
      setFormBusy(false);
    }
  };

  const deleteYamlTool = async (name: string) => {
    setActionPending(name);
    try {
      await apiDelete(`/api/yaml-tools/${encodeURIComponent(name)}`);
      qc.invalidateQueries({ queryKey: qk.yamlTools });
      toast.success(t("tools.deleted_toast", { name }));
    } catch (e) { toast.error(`${e}`); }
    finally { setActionPending(null); }
  };

  /* ── MCP Edit Form (full-page) ─────────────────────────────────── */

  if (editView?.kind === "mcp") {
    const isNew = editView.id === "new";
    return (
      <div className="flex-1 overflow-y-auto p-4 md:p-6 lg:p-8 selection:bg-primary/20">
        <div className="mx-auto max-w-2xl">
          <div className="mb-8 flex items-center gap-3">
            <Button variant="ghost" size="sm" onClick={cancelEdit}>
              <ArrowLeft className="h-3.5 w-3.5" /> {t("common.back")}
            </Button>
            <div>
              <h2 className="font-display text-lg font-bold tracking-tight text-foreground">
                {isNew ? t("tools.new_mcp_server") : t("tools.editing_mcp", { name: editView.id })}
              </h2>
              <span className="text-sm text-muted-foreground">
                {isNew ? t("tools.add_new_mcp") : t("tools.update_mcp")}
              </span>
            </div>
          </div>

          <form onSubmit={saveMcp} className="neu-flat p-6 space-y-5">
            <Field label={t("tools.mcp_field_name")} hint={t("tools.mcp_hint_name")}>
              <Input type="text" required disabled={!isNew} value={mcpForm.name}
                onChange={(e) => setMcpForm((f) => ({ ...f, name: e.target.value }))}
                placeholder="my-mcp" />
            </Field>
            <div className="grid grid-cols-1 sm:grid-cols-2 gap-4">
              <Field label={t("tools.mcp_field_mode")} hint={t("tools.mcp_hint_mode")}>
                <Select value={mcpForm.mode} onValueChange={(v) => setMcpForm((f) => ({ ...f, mode: v }))}>
                  <SelectTrigger className="font-mono text-sm w-full">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="on-demand">on-demand</SelectItem>
                    <SelectItem value="persistent">persistent</SelectItem>
                  </SelectContent>
                </Select>
              </Field>
              <Field label={t("tools.mcp_field_protocol")}>
                <Select value={mcpForm.protocol} onValueChange={(v) => setMcpForm((f) => ({ ...f, protocol: v }))}>
                  <SelectTrigger className="font-mono text-sm w-full">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="http">http</SelectItem>
                    <SelectItem value="mcp">mcp</SelectItem>
                  </SelectContent>
                </Select>
              </Field>
            </div>
            <div className="rounded-lg border border-border/60 bg-muted/10 p-4 space-y-4">
              <p className="text-xs font-medium text-muted-foreground uppercase tracking-wider">{t("tools.mcp_connection")}</p>
              <Field label={t("tools.mcp_field_url")}>
                <Input type="text" value={mcpForm.url} onChange={(e) => setMcpForm((f) => ({ ...f, url: e.target.value }))}
                  placeholder="http://host:9000" />
              </Field>
              <div className="grid grid-cols-1 sm:grid-cols-2 gap-4">
                <Field label={t("tools.mcp_field_container")}>
                  <Input type="text" value={mcpForm.container} onChange={(e) => setMcpForm((f) => ({ ...f, container: e.target.value }))}
                    placeholder="mcp-summarize" />
                </Field>
                <Field label={t("tools.mcp_field_port")}>
                  <Input type="number" value={mcpForm.port} onChange={(e) => setMcpForm((f) => ({ ...f, port: e.target.value }))}
                    placeholder="9002" />
                </Field>
              </div>
            </div>
            <div className="flex justify-end gap-3 pt-3 border-t border-border/50">
              <Button type="button" variant="ghost" size="sm" onClick={cancelEdit}>
                {t("common.cancel")}
              </Button>
              <Button type="submit" size="sm" disabled={formBusy}>
                <Save className="h-4 w-4" /> {formBusy ? t("common.saving") : isNew ? t("common.create") : t("common.save")}
              </Button>
            </div>
          </form>
        </div>
      </div>
    );
  }

  /* ── YAML Tool Edit Form (full-page) ─────────────────────────────── */

  if (editView?.kind === "yaml") {
    const isNewYaml = editView.id === "new";
    return (
      <div className="flex-1 overflow-y-auto p-4 md:p-6 lg:p-8 selection:bg-primary/20">
        <div className="mx-auto max-w-3xl">
          <div className="mb-8 flex items-center gap-3">
            <Button variant="ghost" size="sm" onClick={cancelEdit}>
              <ArrowLeft className="h-3.5 w-3.5" /> {t("common.back")}
            </Button>
            <div>
              <h2 className="font-display text-lg font-bold tracking-tight text-foreground">
                {isNewYaml ? t("tools.new_yaml_tool") : t("tools.editing_yaml", { name: editView.id })}
              </h2>
              <span className="text-sm text-muted-foreground">
                {isNewYaml ? t("tools.create_yaml_tool") : t("tools.edit_yaml_tool")}
              </span>
            </div>
          </div>

          {yamlLoading ? (
            <Skeleton className="h-96 rounded-xl border border-border bg-muted/20" />
          ) : (
            <form onSubmit={saveYaml} className="neu-flat p-6 space-y-5">
              <div className="flex flex-col gap-1.5">
                <label className="text-xs font-medium text-foreground">{t("tools.yaml_config")}</label>
                <span className="text-[11px] text-muted-foreground">{t("tools.yaml_edit_hint")}</span>
                <Textarea
                  value={yamlContent}
                  onChange={(e) => setYamlContent(e.target.value)}
                  rows={24}
                  spellCheck={false}
                  className="font-mono leading-relaxed resize-y"
                />
              </div>
              <div className="flex justify-end gap-3 pt-3 border-t border-border/50">
                <Button type="button" variant="ghost" size="sm" onClick={cancelEdit}>
                  {t("common.cancel")}
                </Button>
                <Button type="submit" size="sm" disabled={formBusy}>
                  <Save className="h-4 w-4" /> {formBusy ? t("common.saving") : isNewYaml ? t("common.create") : t("common.save")}
                </Button>
              </div>
            </form>
          )}
        </div>
      </div>
    );
  }

  /* ── Card renderers ──────────────────────────────────────────────── */

  const renderYamlCard = (tool: YamlToolEntry, idx: number) => {
    const pending = actionPending === tool.name;
    return (
      <div key={`yaml-${tool.name}-${idx}`}
        className={`flex flex-col gap-3 neu-flat p-5 min-w-0 overflow-hidden ${tool.status === "disabled" ? "opacity-50" : ""}`}>
        <div className="flex items-start justify-between gap-2">
          <div className="flex items-center gap-3 min-w-0">
            <div className={`flex h-9 w-9 shrink-0 items-center justify-center rounded-lg border ${
              tool.status === "verified" ? "bg-success/10 border-success/20"
              : tool.status === "draft" ? "bg-warning/10 border-warning/20"
              : "bg-muted/30 border-border"
            }`}>
              <FileCode2 className={`h-4 w-4 ${
                tool.status === "verified" ? "text-success"
                : tool.status === "draft" ? "text-warning"
                : "text-muted-foreground"
              }`} />
            </div>
            <span className="font-mono text-sm font-bold text-foreground break-words leading-snug min-w-0" title={tool.name}>{tool.name}</span>
          </div>
          <div className="flex flex-col items-end gap-1 shrink-0">
            <TypeBadge type={tool.method.toUpperCase()} />
            <StatusBadge status={tool.status} />
          </div>
        </div>
        {tool.description && (
          <p className="text-xs text-muted-foreground line-clamp-2">{tool.description}</p>
        )}
        <div className="space-y-1.5 mt-auto text-xs">
          <div className="flex flex-col gap-0.5 bg-muted/20 rounded px-2.5 py-1.5 border border-border/50 overflow-hidden">
            <span className="text-muted-foreground">{t("tools.endpoint")}</span>
            <span className="font-mono text-primary/70 truncate" title={tool.endpoint}>{tool.endpoint}</span>
          </div>
        </div>
        <div className="flex gap-1.5 pt-1">
          <Button variant="outline" size="sm" disabled={pending} onClick={() => startEditYaml(tool.name)} className="flex-1">
            <Pencil className="h-3 w-3" /> {t("common.edit")}
          </Button>
          {tool.status === "draft" && (
            <Button variant="outline-success" size="sm" disabled={pending} aria-label={t("tools.verify")}
              onClick={() => handleVerify(tool.name)}>
              <CheckCircle2 className="h-3 w-3" />
            </Button>
          )}
          {tool.status === "disabled" ? (
            <Button variant="outline-success" size="sm" disabled={pending} aria-label={t("tools.enable")}
              onClick={() => handleEnable(tool.name)}>
              <Play className="h-3 w-3" />
            </Button>
          ) : (
            <Button variant="outline-warning" size="sm" disabled={pending} aria-label={t("tools.disable")}
              onClick={() => handleDisable(tool.name)}>
              <Square className="h-3 w-3" />
            </Button>
          )}
          <Button variant="outline-destructive" size="sm" disabled={pending} aria-label={t("common.delete")}
            onClick={() => setDeleteConfirm({ kind: "yaml", name: tool.name })}>
            <Trash2 className="h-3 w-3" />
          </Button>
        </div>
      </div>
    );
  };

  const renderMcpCard = (s: McpEntry) => {
    const isRunning = s.status === "running";
    const active = s.enabled && isRunning;
    const endpoint = s.url ?? (s.container && s.port ? `${s.container}:${s.port}` : s.container ?? "—");
    const pending = actionPending === s.name || actionPending === "reload:" + s.name || actionPending === "toggle:" + s.name;

    return (
      <div key={`mcp-${s.name}`}
        className={`flex flex-col gap-3 neu-flat p-5 min-w-0 overflow-hidden ${!s.enabled ? "opacity-50" : ""}`}>
        <div className="flex items-start justify-between gap-2">
          <div className="flex items-center gap-3 min-w-0">
            <div className={`flex h-9 w-9 shrink-0 items-center justify-center rounded-lg border ${
              s.enabled ? "bg-accent/50 border-border" : "bg-muted/30 border-border"
            }`}>
              <Activity className={`h-4 w-4 ${s.enabled ? "text-foreground/70" : "text-muted-foreground/40"}`} />
            </div>
            <span className="font-mono text-sm font-bold text-foreground break-words leading-snug min-w-0" title={s.name}>{s.name}</span>
          </div>
        </div>
        <div className="space-y-1.5 mt-auto text-xs">
          <Row label={t("tools.mode")} value={s.mode} />
          <div className="flex flex-col gap-0.5 bg-muted/20 rounded px-2.5 py-1.5 border border-border/50 overflow-hidden">
            <span className="text-muted-foreground">{s.url ? t("tools.url") : t("tools.container")}</span>
            <span className="font-mono text-primary/70 truncate" title={endpoint}>{endpoint}</span>
          </div>
          <div className={`flex items-center gap-1.5 rounded px-2.5 py-1.5 border ${
            active ? "border-success/30 bg-success/5" : "border-border bg-muted/10"
          }`}>
            <div className={`h-1.5 w-1.5 rounded-full shrink-0 ${active ? "bg-success" : "bg-muted-foreground/40"}`} />
            <span className={`font-medium min-w-0 truncate ${active ? "text-success" : "text-muted-foreground"}`}>
              {!s.enabled ? t("tools.disabled") : s.status === "running" ? t("tools.running") : s.status ?? t("tools.idle")}
            </span>
            {s.enabled && s.tool_count != null && (
              <span className="ml-auto shrink-0 font-mono font-bold text-primary">{t("tools.tools_count", { count: s.tool_count ?? 0 })}</span>
            )}
          </div>
        </div>
        <div className="flex gap-1.5 pt-1">
          <Button variant="outline" size="sm" disabled={pending} onClick={() => startEditMcp(s)} className="flex-1">
            <Pencil className="h-3 w-3" /> {t("common.edit")}
          </Button>
          {s.enabled && s.status === "running" && (
            <Button variant="outline" size="sm" disabled={pending} aria-label={t("tools.reload")}
              onClick={() => reloadMcp(s.name)}>
              <RotateCcw className={`h-3 w-3 ${actionPending === "reload:" + s.name ? "animate-spin" : ""}`} />
            </Button>
          )}
          <Button
            variant={s.enabled ? "outline-warning" : "outline-success"}
            size="sm"
            disabled={pending}
            aria-label={s.enabled ? t("tools.disable") : t("tools.enable")}
            onClick={() => toggleMcp(s.name)}
          >
            {s.enabled ? <Square className="h-3 w-3" /> : <Play className="h-3 w-3" />}
          </Button>
          <Button variant="outline-destructive" size="sm" disabled={pending} aria-label={t("common.delete")}
            onClick={() => setDeleteConfirm({ kind: "mcp", name: s.name })}>
            <Trash2 className="h-3 w-3" />
          </Button>
        </div>
      </div>
    );
  };

  /* ── Tabbed view ─────────────────────────────────────────────────── */

  return (
    <div className="flex-1 overflow-y-auto p-4 md:p-6 lg:p-8 selection:bg-primary/20">
      <div>
        {/* Header */}
        <PageHeader
          title={t("tools.title")}
          description={t("tools.subtitle")}
          actions={
            <div className="flex flex-wrap items-center gap-2">
              <Button variant="outline" size="sm" onClick={startCreateYaml}>
                <Plus className="h-3.5 w-3.5" /> {t("tools.external_apis")}
              </Button>
              <Button variant="outline" size="sm" onClick={startCreateMcp}>
                <Plus className="h-3.5 w-3.5" /> {t("tools.add_mcp")}
              </Button>
              <Button variant="outline" size="sm" onClick={invalidateAll} disabled={loading} aria-label={t("common.refresh")}>
                <RefreshCw className={`h-3.5 w-3.5 ${loading ? "animate-spin" : ""}`} />
              </Button>
            </div>
          }
        />

        {errorMsg && <ErrorBanner error={errorMsg} />}

        {loading ? (
          <div className="grid gap-4 md:grid-cols-2 lg:grid-cols-3 2xl:grid-cols-4">
            {[1, 2, 3, 4, 5, 6].map((i) => (
              <Skeleton key={i} className="h-40 rounded-xl border border-border bg-muted/20" />
            ))}
          </div>
        ) : (
          <Tabs defaultValue="external" className="mt-2">
            <TabsList>
              <TabsTrigger value="external">
                <ExternalLink className="h-3.5 w-3.5" />
                {t("tools.external_apis")}
                <Badge variant="secondary" className="ml-1.5 text-[10px]">{yamlTools.length}</Badge>
              </TabsTrigger>
              <TabsTrigger value="mcp">
                <Activity className="h-3.5 w-3.5" />
                {t("tools.mcp_servers")}
                <Badge variant="secondary" className="ml-1.5 text-[10px]">{mcpServers.length}</Badge>
              </TabsTrigger>
            </TabsList>

            {/* ── External API tools (YAML) ── */}
            <TabsContent value="external" className="mt-6">
              {yamlTools.length === 0 ? (
                <EmptyState icon={ExternalLink} text={t("tools.no_external_apis")} />
              ) : (
                <div className="grid gap-4 md:grid-cols-2 lg:grid-cols-3 2xl:grid-cols-4">
                  {yamlTools.map((tool, idx) => renderYamlCard(tool, idx))}
                </div>
              )}
            </TabsContent>

            {/* ── MCP servers ── */}
            <TabsContent value="mcp" className="mt-6">
              {mcpServers.length === 0 ? (
                <EmptyState icon={Activity} text={t("tools.no_mcp_servers")} />
              ) : (
                <div className="grid gap-4 md:grid-cols-2 lg:grid-cols-3 2xl:grid-cols-4">
                  {mcpServers.map((s) => renderMcpCard(s))}
                </div>
              )}
            </TabsContent>
          </Tabs>
        )}
      </div>

      <AlertDialog open={!!deleteConfirm} onOpenChange={(open) => { if (!open) setDeleteConfirm(null); }}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>{t("tools.delete_confirm_title", { kind: deleteConfirm?.kind === "mcp" ? "MCP" : "YAML", name: deleteConfirm?.name ?? "" })}</AlertDialogTitle>
            <AlertDialogDescription>
              {t("tools.delete_confirm_description")}
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>{t("common.cancel")}</AlertDialogCancel>
            <AlertDialogAction variant="destructive" onClick={handleConfirmDelete}>{t("common.delete")}</AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  );
}
