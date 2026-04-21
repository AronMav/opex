"use client";

import { useCallback, useState, type FormEvent } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { apiGet, apiPost, apiPut, apiDelete } from "@/lib/api";
import { useTools, useYamlTools, useMcpServers, useRestartService, useRebuildService, qk } from "@/lib/queries";
import { useTranslation } from "@/hooks/use-translation";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import { ErrorBanner } from "@/components/ui/error-banner";
import { Badge } from "@/components/ui/badge";
import { Skeleton } from "@/components/ui/skeleton";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { EmptyState } from "@/components/ui/empty-state";
import { Button } from "@/components/ui/button";
import {
  Network, Activity, FileCode2, CheckCircle2,
  RefreshCw, Plus, Pencil, Trash2, RotateCcw, Hammer,
  ArrowLeft, Save, Wifi, WifiOff, Square, Play,
  ExternalLink,
} from "lucide-react";
import { toast } from "sonner";
import {
  AlertDialog, AlertDialogAction, AlertDialogCancel,
  AlertDialogContent, AlertDialogDescription, AlertDialogFooter,
  AlertDialogHeader, AlertDialogTitle,
} from "@/components/ui/alert-dialog";
import type { McpEntry, YamlToolEntry, ToolEntry } from "@/types/api";
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

/* ── Service Form helpers ────────────────────────────────────────── */

interface ServiceFormData {
  name: string;
  type: string;
  url: string;
  max_concurrent: string;
  healthcheck: string;
  depends_on: string;
  ui_path: string;
}

const emptyServiceForm = (): ServiceFormData => ({
  name: "", type: "external", url: "", max_concurrent: "1", healthcheck: "", depends_on: "", ui_path: "",
});

function serviceToForm(e: ToolEntry): ServiceFormData {
  return {
    name: e.name,
    type: e.tool_type,
    url: e.url,
    max_concurrent: String(e.concurrency_limit ?? 1),
    healthcheck: e.healthcheck ?? "",
    depends_on: (e.depends_on ?? []).join(", "),
    ui_path: e.ui_path ?? "",
  };
}

function serviceFormToPayload(f: ServiceFormData) {
  const deps = f.depends_on.trim()
    ? f.depends_on.split(",").map(s => s.trim()).filter(Boolean)
    : [];
  return {
    name: f.name.trim(),
    type: f.type,
    url: f.url.trim(),
    max_concurrent: Number(f.max_concurrent) || 1,
    healthcheck: f.healthcheck.trim() || null,
    depends_on: deps,
    ui_path: f.ui_path.trim() || null,
  };
}

/** Resolve UI URL: if it starts with http, use as-is; otherwise append to service URL.
 *  Replace localhost/127.0.0.1 with browser hostname so LAN access works. */
function resolveUiUrl(serviceUrl: string, uiUrl: string): string {
  const raw = uiUrl.startsWith("http") ? uiUrl : `${serviceUrl}${uiUrl}`;
  try {
    const parsed = new URL(raw);
    if (parsed.hostname === "localhost" || parsed.hostname === "127.0.0.1") {
      parsed.hostname = window.location.hostname;
    }
    return parsed.toString();
  } catch {
    return raw;
  }
}

type EditView =
  | { kind: "mcp"; id: string }
  | { kind: "service"; id: string }
  | { kind: "yaml"; id: string };

/* ── Page ────────────────────────────────────────────────────────── */

export default function ToolsPage() {
  const { t, locale } = useTranslation();
  const qc = useQueryClient();

  const { data: services = [], isLoading: servicesLoading, error: servicesError } = useTools();
  const { data: yamlTools = [], isLoading: yamlLoading2, error: yamlError } = useYamlTools();
  const { data: mcpServers = [], isLoading: mcpLoading, error: mcpError } = useMcpServers();
  const restartService = useRestartService();
  const rebuildService = useRebuildService();

  const loading = servicesLoading || yamlLoading2 || mcpLoading;
  const errorMsg = servicesError ? String(servicesError) : yamlError ? String(yamlError) : mcpError ? String(mcpError) : "";

  const [actionPending, setActionPending] = useState<string | null>(null);
  // viewMode removed — only cards view now (tools grouped under services)

  // Edit forms (full-page)
  const [editView, setEditView] = useState<EditView | null>(null);
  const [mcpForm, setMcpForm] = useState<McpFormData>(emptyMcpForm());
  const [serviceForm, setServiceForm] = useState<ServiceFormData>(emptyServiceForm());
  const [yamlContent, setYamlContent] = useState("");
  const [yamlLoading, setYamlLoading] = useState(false);
  const [formBusy, setFormBusy] = useState(false);
  const [deleteConfirm, setDeleteConfirm] = useState<{ kind: "mcp" | "service" | "yaml"; name: string } | null>(null);
  const [restartConfirm, setRestartConfirm] = useState<{ name: string; action: "restart" | "rebuild" } | null>(null);

  const invalidateAll = useCallback(() => {
    qc.invalidateQueries({ queryKey: qk.tools });
    qc.invalidateQueries({ queryKey: qk.yamlTools });
    qc.invalidateQueries({ queryKey: qk.mcpServers });
  }, [qc]);

  const runServiceAction = async (name: string, action: "restart" | "rebuild") => {
    try {
      if (action === "restart") {
        await restartService.mutateAsync(name);
      } else {
        await rebuildService.mutateAsync(name);
      }
      toast.success(action === "restart" ? t("tools.restarted", { name }) : t("tools.rebuilt", { name }));
    } catch (e) {
      toast.error(t("tools.action_failed", { action, error: String(e) }));
    }
  };

  const handleConfirmDelete = async () => {
    if (!deleteConfirm) return;
    const { kind, name } = deleteConfirm;
    setDeleteConfirm(null);
    if (kind === "mcp") await deleteMcp(name);
    else if (kind === "service") await deleteService(name);
    else await deleteYamlTool(name);
  };

  /* ── MCP CRUD ─────────────────────────────────────────────────── */
  const startCreateMcp = () => { setMcpForm(emptyMcpForm()); setEditView({ kind: "mcp", id: "new" }); };
  const startEditMcp = (s: McpEntry) => { setMcpForm(mcpToForm(s)); setEditView({ kind: "mcp", id: s.name }); };
  const cancelEdit = () => { setEditView(null); setMcpForm(emptyMcpForm()); setServiceForm(emptyServiceForm()); setYamlContent(""); };

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

  /* ── Service CRUD ──────────────────────────────────────────────── */
  const startCreateService = () => { setServiceForm(emptyServiceForm()); setEditView({ kind: "service", id: "new" }); };
  const startEditService = (s: ToolEntry) => { setServiceForm(serviceToForm(s)); setEditView({ kind: "service", id: s.name }); };

  const saveService = async (e: FormEvent) => {
    e.preventDefault();
    if (!editView || editView.kind !== "service") return;
    const payload = serviceFormToPayload(serviceForm);
    if (!payload.name) { toast.error(t("tools.service_field_name")); return; }
    if (!payload.url) { toast.error(t("tools.service_field_url")); return; }
    setFormBusy(true);
    try {
      if (editView.id === "new") {
        await apiPost("/api/tools", payload);
        toast.success(t("tools.created_toast", { name: payload.name }));
      } else {
        await apiPut(`/api/tools/${encodeURIComponent(editView.id)}`, payload);
        toast.success(t("tools.updated_toast", { name: payload.name }));
      }
      cancelEdit();
      qc.invalidateQueries({ queryKey: qk.tools });
    } catch (e) {
      toast.error(`${e}`);
    } finally {
      setFormBusy(false);
    }
  };

  const deleteService = async (name: string) => {
    setActionPending(name);
    try {
      await apiDelete(`/api/tools/${encodeURIComponent(name)}`);
      qc.invalidateQueries({ queryKey: qk.tools });
      toast.success(t("tools.deleted_toast", { name }));
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

  /* ── Service Edit Form (full-page) ─────────────────────────────── */

  if (editView?.kind === "service") {
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
                {isNew ? t("tools.new_service") : t("tools.editing_service", { name: editView.id })}
              </h2>
              <span className="text-sm text-muted-foreground">
                {isNew ? t("tools.add_infra_service") : t("tools.update_service_config")}
              </span>
            </div>
          </div>

          <form onSubmit={saveService} className="neu-flat p-6 space-y-5">
            <Field label={t("tools.service_field_name")} hint={t("tools.service_hint_name")}>
              <Input type="text" required disabled={!isNew} value={serviceForm.name}
                onChange={(e) => setServiceForm((f) => ({ ...f, name: e.target.value }))}
                placeholder="my-service" />
            </Field>
            <div className="grid grid-cols-1 sm:grid-cols-2 gap-4">
              <Field label={t("tools.service_field_type")} hint={t("tools.service_hint_type")}>
                <Select value={serviceForm.type} onValueChange={(v) => setServiceForm((f) => ({ ...f, type: v }))}>
                  <SelectTrigger className="font-mono text-sm w-full">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="external">external</SelectItem>
                    <SelectItem value="internal">internal</SelectItem>
                  </SelectContent>
                </Select>
              </Field>
              <Field label={t("tools.service_field_max_concurrent")} hint={t("tools.service_hint_max_concurrent")}>
                <Input type="number" min={1} max={100} required value={serviceForm.max_concurrent}
                  onChange={(e) => setServiceForm((f) => ({ ...f, max_concurrent: e.target.value }))} />
              </Field>
            </div>
            <Field label={t("tools.service_field_url")} hint={t("tools.service_hint_url")}>
              <Input type="text" required value={serviceForm.url}
                onChange={(e) => setServiceForm((f) => ({ ...f, url: e.target.value }))}
                placeholder="http://192.168.1.132:11434/v1" />
            </Field>
            <Field label={t("tools.service_field_healthcheck")} hint={t("tools.service_hint_healthcheck")}>
              <Input type="text" value={serviceForm.healthcheck}
                onChange={(e) => setServiceForm((f) => ({ ...f, healthcheck: e.target.value }))}
                placeholder="/health" />
            </Field>
            <Field label={t("tools.service_field_depends_on")} hint={t("tools.service_hint_depends_on")}>
              <Input type="text" value={serviceForm.depends_on}
                onChange={(e) => setServiceForm((f) => ({ ...f, depends_on: e.target.value }))}
                placeholder="ollama-vision, whisper" />
            </Field>
            <Field label={t("tools.service_field_ui_path")} hint={t("tools.service_hint_ui_path")}>
              <Input type="text" value={serviceForm.ui_path}
                onChange={(e) => setServiceForm((f) => ({ ...f, ui_path: e.target.value }))}
                placeholder="http://localhost:9011/ui/ or /ui/" />
            </Field>
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

  /* ── Card grid view ──────────────────────────────────────────────── */

  const total = services.length + mcpServers.length + yamlTools.length;

  return (
    <div className="flex-1 overflow-y-auto p-4 md:p-6 lg:p-8 selection:bg-primary/20">
      <div>
        {/* Header */}
        <div className="mb-8 md:mb-10 flex flex-col sm:flex-row sm:items-start justify-between gap-4">
          <div className="flex flex-col gap-1">
            <h2 className="font-display text-lg font-bold tracking-tight text-foreground">{t("tools.title")}</h2>
            <span className="text-sm text-muted-foreground">{t("tools.subtitle")}</span>
          </div>
          <div className="flex flex-wrap items-center gap-2">
            <Button variant="outline" size="sm" onClick={startCreateService}>
              <Plus className="h-3.5 w-3.5" /> {t("tools.add_service")}
            </Button>
            <Button variant="outline" size="sm" onClick={startCreateMcp}>
              <Plus className="h-3.5 w-3.5" /> {t("tools.add_mcp")}
            </Button>
            <Button variant="outline" size="sm" onClick={startCreateYaml}>
              <Plus className="h-3.5 w-3.5" /> {t("tools.add_yaml")}
            </Button>
            <Button variant="outline" size="sm" onClick={invalidateAll} disabled={loading} aria-label={t("common.refresh")}>
              <RefreshCw className={`h-3.5 w-3.5 ${loading ? "animate-spin" : ""}`} />
            </Button>
            {/* Graph view toggle removed — tools are now grouped under services */}
          </div>
        </div>

        {errorMsg && <ErrorBanner error={errorMsg} />}

        {loading ? (
          <div className="grid gap-4 md:grid-cols-2 lg:grid-cols-3">
            {[1, 2, 3, 4, 5, 6].map((i) => (
              <Skeleton key={i} className="h-40 rounded-xl border border-border bg-muted/20" />
            ))}
          </div>
        ) : total === 0 ? (
          <EmptyState icon={Network} text={t("tools.no_tools_or_services")} />
        ) : (
          <div className="space-y-10">
            {/* ── Services + linked YAML tools ─────────────── */}
            {services.length > 0 && (() => {
              // Map YAML tools to services by matching endpoint host:port to service URL
              const getHostPort = (url: string): string | null => {
                try {
                  const u = new URL(url);
                  return `${u.hostname}:${u.port || (u.protocol === "https:" ? "443" : "80")}`;
                } catch { return null; }
              };
              const svcHostMap = new Map<string, string>();
              for (const svc of services) {
                const hp = getHostPort(svc.url);
                if (hp) svcHostMap.set(hp, svc.name);
              }
              const toolsByService = new Map<string, YamlToolEntry[]>();
              const unmatchedTools: YamlToolEntry[] = [];
              for (const tool of yamlTools) {
                const hp = getHostPort(tool.endpoint);
                const svcName = hp ? svcHostMap.get(hp) : undefined;
                if (svcName) {
                  const arr = toolsByService.get(svcName) ?? [];
                  arr.push(tool);
                  toolsByService.set(svcName, arr);
                } else {
                  unmatchedTools.push(tool);
                }
              };
              const HIDDEN_INFRA = new Set(["toolgate", "browser-renderer"]);
              // Tools linked to hidden services should appear as "unmatched" (visible without the service card)
              for (const hiddenName of HIDDEN_INFRA) {
                const hiddenTools = toolsByService.get(hiddenName) ?? [];
                unmatchedTools.push(...hiddenTools);
                toolsByService.delete(hiddenName);
              }
              const visibleServices = services.filter((s) => !HIDDEN_INFRA.has(s.name));
              const servicesWithTools = visibleServices.filter((s) => (toolsByService.get(s.name) ?? []).length > 0);
              const standaloneServices = visibleServices.filter((s) => (toolsByService.get(s.name) ?? []).length === 0);

              return <>
              {/* Standalone services (no linked tools) — compact grid */}
              {standaloneServices.length > 0 && (
                <section>
                  <div className="mb-4 flex items-center gap-3">
                    <Wifi className="h-4 w-4 text-amber-400" />
                    <h3 className="text-sm font-semibold text-foreground">{t("tools.infrastructure_services")}</h3>
                    <Badge variant="secondary" className="text-xs">{standaloneServices.length}</Badge>
                  </div>
                  <div className="grid gap-4 md:grid-cols-2 lg:grid-cols-3">
                    {standaloneServices.map((svc) => {
                      const pending = actionPending === svc.name;
                      return (
                        <div key={`svc-${svc.name}`} className="flex flex-col gap-3 neu-flat p-5 min-w-0 overflow-hidden">
                          <div className="flex items-start justify-between gap-2">
                            <div className="flex items-center gap-3 min-w-0">
                              <div className={`flex h-9 w-9 shrink-0 items-center justify-center rounded-lg border ${svc.healthy ? "bg-success/10 border-success/20" : "bg-destructive/10 border-destructive/20"}`}>
                                {svc.healthy ? <Wifi className="h-4 w-4 text-success" /> : <WifiOff className="h-4 w-4 text-destructive" />}
                              </div>
                              <span className="font-mono text-sm font-bold text-foreground break-words leading-snug min-w-0">{svc.name}</span>
                            </div>
                            <TypeBadge type={svc.tool_type === "internal" ? "INT" : "EXT"} />
                          </div>
                          <div className="space-y-1.5 mt-auto text-xs">
                            <div className="flex flex-col gap-0.5 bg-muted/20 rounded px-2.5 py-1.5 border border-border/50 overflow-hidden">
                              <span className="text-muted-foreground">{t("tools.url")}</span>
                              <span className="font-mono text-primary/70 truncate" title={svc.url}>{svc.url}</span>
                            </div>
                            <div className={`flex items-center gap-1.5 rounded px-2.5 py-1.5 border ${svc.healthy ? "border-success/30 bg-success/5" : "border-destructive/30 bg-destructive/5"}`}>
                              <div className={`h-1.5 w-1.5 rounded-full shrink-0 ${svc.healthy ? "bg-success" : "bg-destructive"}`} />
                              <span className={`font-medium ${svc.healthy ? "text-success" : "text-destructive"}`}>
                                {svc.healthy ? t("tools.healthy") : t("tools.unhealthy")}
                              </span>
                            </div>
                          </div>
                          <div className="flex flex-col gap-1.5 pt-1">
                            <div className="flex gap-1.5">
                              <button disabled={pending} onClick={() => startEditService(svc)}
                                className="flex flex-1 items-center justify-center gap-1 rounded-md border border-border bg-muted/20 px-2.5 py-1.5 text-xs font-medium text-muted-foreground hover:bg-accent hover:text-foreground disabled:opacity-50 disabled:cursor-not-allowed">
                                <Pencil className="h-3 w-3" /> {t("common.edit")}
                              </button>
                              <button disabled={pending} onClick={() => setDeleteConfirm({ kind: "service", name: svc.name })}
                                className="flex items-center justify-center rounded-md border border-destructive/30 bg-destructive/10 px-2.5 py-1.5 text-xs font-medium text-destructive hover:bg-destructive/20 disabled:opacity-50 disabled:cursor-not-allowed">
                                <Trash2 className="h-3 w-3" />
                              </button>
                            </div>
                            {svc.ui_path && (
                              <a href={resolveUiUrl(svc.url, svc.ui_path)} target="_blank" rel="noopener noreferrer"
                                className="flex w-full items-center justify-center gap-1 rounded-md border border-primary/40 bg-primary/10 px-2.5 py-1.5 text-xs font-medium text-primary transition-colors hover:bg-primary/20">
                                <ExternalLink className="h-3 w-3" /> {t("tools.open_ui")}
                              </a>
                            )}
                          </div>
                        </div>
                      );
                    })}
                  </div>
                </section>
              )}

              {/* Services WITH linked tools — each gets its own section */}
              {servicesWithTools.map((svc) => {
                const linkedTools = toolsByService.get(svc.name) ?? [];
                return (
              <section key={`svc-section-${svc.name}`}>
                <div className="mb-4 flex items-center gap-3">
                  {svc.healthy
                    ? <Wifi className="h-4 w-4 text-success" />
                    : <WifiOff className="h-4 w-4 text-destructive" />}
                  <h3 className="text-sm font-semibold text-foreground">{svc.name}</h3>
                  <span className="text-xs text-muted-foreground font-mono">{svc.url}</span>
                  <Badge variant="secondary" className="text-xs">{linkedTools.length} {t("tools.yaml_tools_short")}</Badge>
                  {svc.managed && (
                    <Badge variant="outline" className="text-[10px]">{t("tools.managed")}</Badge>
                  )}
                </div>
                <div className="grid gap-4 md:grid-cols-2 lg:grid-cols-3">
                  {(() => {
                    const pending = actionPending === svc.name;
                    return (
                      <div key={`svc-${svc.name}`}
                        className="flex flex-col gap-3 neu-flat p-5 min-w-0 overflow-hidden">
                        <div className="flex items-start justify-between gap-2">
                          <div className="flex items-center gap-3 min-w-0">
                            <div className={`flex h-9 w-9 shrink-0 items-center justify-center rounded-lg border ${
                              svc.healthy ? "bg-success/10 border-success/20" : "bg-destructive/10 border-destructive/20"
                            }`}>
                              {svc.healthy
                                ? <Wifi className="h-4 w-4 text-success" />
                                : <WifiOff className="h-4 w-4 text-destructive" />}
                            </div>
                            <span className="font-mono text-sm font-bold text-foreground break-words leading-snug min-w-0" title={svc.name}>{svc.name}</span>
                          </div>
                          <TypeBadge type={svc.tool_type === "internal" ? "INT" : "EXT"} />
                        </div>

                        <div className="space-y-1.5 mt-auto text-xs">
                          <div className="flex flex-col gap-0.5 bg-muted/20 rounded px-2.5 py-1.5 border border-border/50 overflow-hidden">
                            <span className="text-muted-foreground">{t("tools.url")}</span>
                            <span className="font-mono text-primary/70 truncate" title={svc.url}>{svc.url}</span>
                          </div>
                          <div className="flex gap-2">
                            <div className="flex-1 flex justify-between items-center bg-muted/20 rounded px-2.5 py-1.5 border border-border/50 min-w-0">
                              <span className="text-muted-foreground shrink-0">{t("tools.concurrency")}</span>
                              <span className="font-mono text-foreground/80 ml-2">{svc.concurrency_limit ?? 1}</span>
                            </div>
                            <div className={`flex items-center gap-1.5 rounded px-2.5 py-1.5 border shrink-0 ${svc.healthy ? "border-success/30 bg-success/5" : "border-destructive/30 bg-destructive/5"}`}>
                              <div className={`h-1.5 w-1.5 rounded-full shrink-0 ${svc.healthy ? "bg-success" : "bg-destructive"}`} />
                              <span className={`font-medium whitespace-nowrap ${svc.healthy ? "text-success" : "text-destructive"}`}>
                                {svc.healthy ? t("tools.healthy") : t("tools.unhealthy")}
                              </span>
                            </div>
                          </div>
                          {svc.depends_on && svc.depends_on.length > 0 && (
                            <div className="flex flex-col gap-0.5 bg-muted/20 rounded px-2.5 py-1.5 border border-border/50">
                              <span className="text-muted-foreground">{t("tools.depends_on")}</span>
                              <span className="font-mono text-foreground/70 break-all leading-snug">
                                {svc.depends_on.join(", ")}
                              </span>
                            </div>
                          )}
                        </div>

                        <div className="flex flex-col gap-1.5 pt-1">
                          <div className="flex gap-1.5">
                            <button disabled={pending} onClick={() => startEditService(svc)}
                              className="flex flex-1 items-center justify-center gap-1 rounded-md border border-border bg-muted/20 px-2.5 py-1.5 text-xs font-medium text-muted-foreground transition-colors hover:bg-accent hover:text-foreground disabled:opacity-50 disabled:cursor-not-allowed">
                              <Pencil className="h-3 w-3" /> {t("common.edit")}
                            </button>
                            <button disabled={pending} onClick={() => setDeleteConfirm({ kind: "service", name: svc.name })}
                              title={t("common.delete")}
                              className="flex items-center justify-center gap-1 rounded-md border border-destructive/30 bg-destructive/10 px-2.5 py-1.5 text-xs font-medium text-destructive hover:bg-destructive/20 disabled:opacity-50 disabled:cursor-not-allowed">
                              <Trash2 className="h-3 w-3" />
                            </button>
                          </div>
                          {svc.ui_path && (
                            <a href={resolveUiUrl(svc.url, svc.ui_path)} target="_blank" rel="noopener noreferrer"
                              className="flex w-full items-center justify-center gap-1 rounded-md border border-primary/40 bg-primary/10 px-2.5 py-1.5 text-xs font-medium text-primary transition-colors hover:bg-primary/20">
                              <ExternalLink className="h-3 w-3" /> {t("tools.open_ui")}
                            </a>
                          )}
                          {svc.managed && (
                            <button onClick={() => setRestartConfirm({ name: svc.name, action: "restart" })}
                              className="flex w-full items-center justify-center gap-1 rounded-md border border-border bg-muted/20 px-2.5 py-1.5 text-xs font-medium text-muted-foreground transition-colors hover:bg-accent hover:text-foreground">
                              <RotateCcw className="h-3 w-3" /> {t("services.restart")}
                            </button>
                          )}
                        </div>
                      </div>
                    );
                  })()}
                  {/* Linked YAML tools in same grid */}
                  {linkedTools.map((tool, idx) => {
                    const pending = actionPending === tool.name;
                    return (
                      <div key={`yaml-${tool.name}-${idx}`}
                        className={`flex flex-col gap-3 rounded-xl border-2 border-border/60 p-5 min-w-0 overflow-hidden ${tool.status === "disabled" ? "opacity-50" : ""}`}>
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
                        <div className="flex gap-1.5 pt-1 mt-auto">
                          <button disabled={pending} onClick={() => startEditYaml(tool.name)}
                            className="flex flex-1 items-center justify-center gap-1 rounded-md border border-border bg-muted/20 px-2.5 py-1.5 text-xs font-medium text-muted-foreground hover:bg-accent hover:text-foreground disabled:opacity-50 disabled:cursor-not-allowed">
                            <Pencil className="h-3 w-3" /> {t("common.edit")}
                          </button>
                          {tool.status === "draft" && (
                            <button disabled={pending} onClick={() => handleVerify(tool.name)}
                              title={t("tools.verify")}
                              className="flex items-center justify-center gap-1 rounded-md border border-success/30 bg-success/10 px-2.5 py-1.5 text-xs font-medium text-success hover:bg-success/20 disabled:opacity-50 disabled:cursor-not-allowed">
                              <CheckCircle2 className="h-3 w-3" />
                            </button>
                          )}
                          {tool.status === "disabled" ? (
                            <button disabled={pending} onClick={() => handleEnable(tool.name)}
                              title={t("tools.enable")}
                              className="flex items-center justify-center gap-1 rounded-md border border-success/30 bg-success/10 px-2.5 py-1.5 text-xs font-medium text-success hover:bg-success/20 disabled:opacity-50 disabled:cursor-not-allowed">
                              <Play className="h-3 w-3" />
                            </button>
                          ) : (
                            <button disabled={pending} onClick={() => handleDisable(tool.name)}
                              title={t("tools.disable")}
                              className="flex items-center justify-center gap-1 rounded-md border border-warning/30 bg-warning/10 px-2.5 py-1.5 text-xs font-medium text-warning hover:bg-warning/20 disabled:opacity-50 disabled:cursor-not-allowed">
                              <Square className="h-3 w-3" />
                            </button>
                          )}
                          <button disabled={pending} onClick={() => setDeleteConfirm({ kind: "yaml", name: tool.name })}
                            title={t("common.delete")}
                            className="flex items-center justify-center gap-1 rounded-md border border-destructive/30 bg-destructive/10 px-2.5 py-1.5 text-xs font-medium text-destructive hover:bg-destructive/20 disabled:opacity-50 disabled:cursor-not-allowed">
                            <Trash2 className="h-3 w-3" />
                          </button>
                        </div>
                      </div>
                    );
                  })}
                </div>
              </section>
                );
              })}

              {/* ── External API Tools (not linked to any service) ── */}
              {unmatchedTools.length > 0 && (
                <section>
                  <div className="mb-4 flex items-center gap-3">
                    <ExternalLink className="h-4 w-4 text-blue-400" />
                    <h3 className="text-sm font-semibold text-foreground">{t("tools.external_apis")}</h3>
                    <Badge variant="secondary" className="text-xs">{unmatchedTools.length}</Badge>
                  </div>
                  <div className="grid gap-4 md:grid-cols-2 lg:grid-cols-3">
                    {unmatchedTools.map((tool, idx) => {
                      const pending = actionPending === tool.name;
                      return (
                        <div key={`yaml-ext-${tool.name}-${idx}`}
                          className={`flex flex-col gap-3 rounded-xl border-2 border-border/60 p-5 min-w-0 overflow-hidden ${tool.status === "disabled" ? "opacity-50" : ""}`}>
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
                            <button disabled={pending} onClick={() => startEditYaml(tool.name)}
                              className="flex flex-1 items-center justify-center gap-1 rounded-md border border-border bg-muted/20 px-2.5 py-1.5 text-xs font-medium text-muted-foreground hover:bg-accent hover:text-foreground disabled:opacity-50 disabled:cursor-not-allowed">
                              <Pencil className="h-3 w-3" /> {t("common.edit")}
                            </button>
                            {tool.status === "draft" && (
                              <button disabled={pending} onClick={() => handleVerify(tool.name)}
                                title={t("tools.verify")}
                                className="flex items-center justify-center gap-1 rounded-md border border-success/30 bg-success/10 px-2.5 py-1.5 text-xs font-medium text-success hover:bg-success/20 disabled:opacity-50 disabled:cursor-not-allowed">
                                <CheckCircle2 className="h-3 w-3" />
                              </button>
                            )}
                            {tool.status === "disabled" ? (
                              <button disabled={pending} onClick={() => handleEnable(tool.name)}
                                title={t("tools.enable")}
                                className="flex items-center justify-center gap-1 rounded-md border border-success/30 bg-success/10 px-2.5 py-1.5 text-xs font-medium text-success hover:bg-success/20 disabled:opacity-50 disabled:cursor-not-allowed">
                                <Play className="h-3 w-3" />
                              </button>
                            ) : (
                              <button disabled={pending} onClick={() => handleDisable(tool.name)}
                                title={t("tools.disable")}
                                className="flex items-center justify-center gap-1 rounded-md border border-warning/30 bg-warning/10 px-2.5 py-1.5 text-xs font-medium text-warning hover:bg-warning/20 disabled:opacity-50 disabled:cursor-not-allowed">
                                <Square className="h-3 w-3" />
                              </button>
                            )}
                            <button disabled={pending} onClick={() => setDeleteConfirm({ kind: "yaml", name: tool.name })}
                              title={t("common.delete")}
                              className="flex items-center justify-center gap-1 rounded-md border border-destructive/30 bg-destructive/10 px-2.5 py-1.5 text-xs font-medium text-destructive hover:bg-destructive/20 disabled:opacity-50 disabled:cursor-not-allowed">
                              <Trash2 className="h-3 w-3" />
                            </button>
                          </div>
                        </div>
                      );
                    })}
                  </div>
                </section>
              )}
              </>;
            })()}

            {/* ── MCP Servers ──────────────────────────────── */}
            {mcpServers.length > 0 && (
              <section>
                <div className="mb-4 flex items-center gap-3">
                  <Activity className="h-4 w-4 text-violet-400" />
                  <h3 className="text-sm font-semibold text-foreground">{t("tools.mcp_servers")}</h3>
                  <Badge variant="secondary" className="text-xs">{mcpServers.length}</Badge>
                </div>
                <div className="grid gap-4 md:grid-cols-2 lg:grid-cols-3">
                  {mcpServers.map((s) => {
                    const isRunning = s.status === "running";
                    const active = s.enabled && isRunning;
                    const endpoint = s.url ?? (s.container && s.port ? `${s.container}:${s.port}` : s.container ?? "\u2014");
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
                          <button disabled={pending} onClick={() => startEditMcp(s)}
                            className="flex flex-1 items-center justify-center gap-1 rounded-md border border-border bg-muted/20 px-2.5 py-1.5 text-xs font-medium text-muted-foreground hover:bg-accent hover:text-foreground disabled:opacity-50 disabled:cursor-not-allowed">
                            <Pencil className="h-3 w-3" /> {t("common.edit")}
                          </button>
                          {s.enabled && s.status === "running" && (
                            <button disabled={pending} onClick={() => reloadMcp(s.name)}
                              title={t("tools.reload")}
                              className="flex items-center justify-center gap-1 rounded-md border border-border bg-muted/20 px-2.5 py-1.5 text-xs font-medium text-muted-foreground hover:bg-accent hover:text-foreground disabled:opacity-50 disabled:cursor-not-allowed">
                              <RotateCcw className={`h-3 w-3 ${actionPending === "reload:" + s.name ? "animate-spin" : ""}`} />
                            </button>
                          )}
                          <button disabled={pending} onClick={() => toggleMcp(s.name)}
                            title={s.enabled ? t("tools.disable") : t("tools.enable")}
                            className={`flex items-center justify-center gap-1 rounded-md border px-2.5 py-1.5 text-xs font-medium disabled:opacity-50 disabled:cursor-not-allowed ${
                              s.enabled
                                ? "border-warning/30 bg-warning/10 text-warning hover:bg-warning/20"
                                : "border-success/30 bg-success/10 text-success hover:bg-success/20"
                            }`}>
                            {s.enabled ? <Square className="h-3 w-3" /> : <Play className="h-3 w-3" />}
                          </button>
                          <button disabled={pending} onClick={() => setDeleteConfirm({ kind: "mcp", name: s.name })}
                            title={t("common.delete")}
                            className="flex items-center justify-center gap-1 rounded-md border border-destructive/30 bg-destructive/10 px-2.5 py-1.5 text-xs font-medium text-destructive hover:bg-destructive/20 disabled:opacity-50 disabled:cursor-not-allowed">
                            <Trash2 className="h-3 w-3" />
                          </button>
                        </div>
                      </div>
                    );
                  })}
                </div>
              </section>
            )}

            {/* YAML Tools section removed — tools are now grouped under their services above */}
          </div>
        )}
      </div>

      <AlertDialog open={!!deleteConfirm} onOpenChange={(open) => { if (!open) setDeleteConfirm(null); }}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>{t("tools.delete_confirm_title", { kind: deleteConfirm?.kind === "mcp" ? "MCP" : deleteConfirm?.kind === "service" ? t("tools.add_service") : "YAML", name: deleteConfirm?.name ?? "" })}</AlertDialogTitle>
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

      <AlertDialog open={!!restartConfirm} onOpenChange={(open) => { if (!open) setRestartConfirm(null); }}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>
              {restartConfirm?.action === "rebuild" ? t("services.confirm_rebuild_title") : t("services.confirm_restart_title")}
            </AlertDialogTitle>
            <AlertDialogDescription>
              {restartConfirm?.action === "rebuild"
                ? t("services.confirm_rebuild_description", { command: "docker compose up -d --build --no-deps", name: restartConfirm?.name ?? "" })
                : t("services.confirm_restart_description", { name: restartConfirm?.name ?? "" })}
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>{t("common.cancel")}</AlertDialogCancel>
            <AlertDialogAction onClick={() => { if (restartConfirm) { runServiceAction(restartConfirm.name, restartConfirm.action); setRestartConfirm(null); } }}>
              {restartConfirm?.action === "rebuild"
                ? <><Hammer className="mr-1.5 h-4 w-4" /> {t("services.rebuild")}</>
                : <><RotateCcw className="mr-1.5 h-4 w-4" /> {t("services.restart")}</>}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  );
}
