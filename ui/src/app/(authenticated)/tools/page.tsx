"use client";

import { useCallback, useState, type FormEvent } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { apiGet, apiPost, apiPut, apiDelete } from "@/lib/api";
import {
  useYamlTools, useMcpServers, useHandlers, useSetHandlerAllowlist,
  useHandlerSource, useDeleteHandler,
  qk,
} from "@/lib/queries";
import { useTranslation } from "@/hooks/use-translation";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import { ErrorBanner } from "@/components/ui/error-banner";
import { PageHeader } from "@/components/ui/page-header";
import { Badge } from "@/components/ui/badge";
import { Card } from "@/components/ui/card";
import { PageContainer } from "@/components/ui/page-container";
import { IconTile } from "@/components/ui/icon-tile";
import { Skeleton } from "@/components/ui/skeleton";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Tabs, TabsContent } from "@/components/ui/tabs";
import { FilterTabsList } from "@/components/ui/filter-tabs";
import { EmptyState } from "@/components/ui/empty-state";
import { Button } from "@/components/ui/button";
import {
  Activity, FileCode2, CheckCircle2, FileCog,
  RefreshCw, Plus, Pencil, Trash2, RotateCcw,
  ArrowLeft, Save, Square, Play,
  ExternalLink, Download,
} from "lucide-react";
import { toast } from "sonner";
import {
  AlertDialog, AlertDialogAction, AlertDialogCancel,
  AlertDialogContent, AlertDialogDescription, AlertDialogFooter,
  AlertDialogHeader, AlertDialogTitle,
} from "@/components/ui/alert-dialog";
import { Switch } from "@/components/ui/switch";
import { useLanguageStore } from "@/stores/language-store";
import type { McpEntry, YamlToolEntry, HandlerAdminRow } from "@/types/api";
import { Field, Row, TypeBadge, StatusBadge } from "./ToolHelpers";
import { HandlerEditor } from "./HandlerEditor";

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

/* ── HandlerEditor wrapper (resolves source before opening editor) ── */

const HANDLER_STARTER_TEMPLATE = `# <handler>
#   <id>my_handler</id>
#   <label lang="en">My Handler</label>
#   <label lang="ru">Мой обработчик</label>
#   <description lang="en">Describe what this handler does</description>
#   <icon>file</icon>
#   <match>
#     <mime>application/octet-stream</mime>
#   </match>
#   <execution>sync</execution>
#   <output>text</output>
#   <order>100</order>
#   <enabled>true</enabled>
# </handler>
"""My custom file handler."""


async def run(ctx, file, params):
    return ctx.result.text(f"received {len(file.bytes)} bytes")
`;

function HandlerEditorWrapper({
  editorId,
  onSaved,
  onClose,
}: {
  editorId: string | "create";
  onSaved: () => void;
  onClose: () => void;
}) {
  const isCreate = editorId === "create";
  const { data: sourceData, isLoading } = useHandlerSource(isCreate ? null : editorId);

  if (!isCreate && isLoading) return null;

  const initialSource = isCreate
    ? HANDLER_STARTER_TEMPLATE
    : (sourceData?.source ?? "");

  return (
    <HandlerEditor
      id={isCreate ? undefined : editorId}
      initialSource={initialSource}
      sourceKind={isCreate ? undefined : sourceData?.source_kind}
      onSaved={onSaved}
      onClose={onClose}
    />
  );
}

/* ── Page ────────────────────────────────────────────────────────── */

export default function ToolsPage() {
  const { t } = useTranslation();
  const qc = useQueryClient();

  const lang = useLanguageStore((s) => s.locale);
  const { data: yamlTools = [], isLoading: yamlLoading2, error: yamlError } = useYamlTools();
  const { data: mcpServers = [], isLoading: mcpLoading, error: mcpError } = useMcpServers();
  const { data: handlers = [], isLoading: handlersLoading, error: handlersError } = useHandlers();
  const setHandlerAllowlist = useSetHandlerAllowlist();
  const deleteHandler = useDeleteHandler();

  const loading = yamlLoading2 || mcpLoading || handlersLoading;
  const errorMsg = yamlError ? String(yamlError) : mcpError ? String(mcpError) : handlersError ? String(handlersError) : "";

  const [actionPending, setActionPending] = useState<string | null>(null);
  const [handlerPendingId, setHandlerPendingId] = useState<string | null>(null);
  // handler editor: "create" = new handler, string = edit existing id, null = closed
  const [handlerEditorId, setHandlerEditorId] = useState<string | "create" | null>(null);

  // Edit forms (full-page)
  const [editView, setEditView] = useState<EditView | null>(null);
  const [mcpForm, setMcpForm] = useState<McpFormData>(emptyMcpForm());
  const [yamlContent, setYamlContent] = useState("");
  const [yamlLoading, setYamlLoading] = useState(false);
  const [formBusy, setFormBusy] = useState(false);
  // OpenAPI import dialog
  const [importOpen, setImportOpen] = useState(false);
  const [importUrl, setImportUrl] = useState("");
  const [importPrefix, setImportPrefix] = useState("");
  const [importBusy, setImportBusy] = useState(false);
  const [deleteConfirm, setDeleteConfirm] = useState<
    | { kind: "mcp"; name: string }
    | { kind: "yaml"; name: string }
    | { kind: "handler"; id: string; name: string; isReset: boolean }
    | null
  >(null);

  const invalidateAll = useCallback(() => {
    qc.invalidateQueries({ queryKey: qk.yamlTools });
    qc.invalidateQueries({ queryKey: qk.mcpServers });
    qc.invalidateQueries({ queryKey: qk.handlers });
  }, [qc]);

  const runImportOpenapi = useCallback(async () => {
    if (!importUrl.trim()) return;
    setImportBusy(true);
    try {
      const res = await apiPost<{ discovered: number; created: string[]; errors: string[] }>(
        "/api/tools/import-openapi",
        { spec_url: importUrl.trim(), prefix: importPrefix.trim() },
      );
      toast.success(t("tools.import_openapi_success", { count: res.created.length }));
      if (res.errors?.length) toast.warning(res.errors.slice(0, 3).join("; "));
      qc.invalidateQueries({ queryKey: qk.yamlTools });
      setImportOpen(false);
      setImportUrl("");
      setImportPrefix("");
    } catch (e) {
      toast.error(e instanceof Error ? e.message : t("tools.import_openapi_error"));
    } finally {
      setImportBusy(false);
    }
  }, [importUrl, importPrefix, qc, t]);

  const handleConfirmDelete = async () => {
    const target = deleteConfirm;
    if (!target) return;
    setDeleteConfirm(null);
    if (target.kind === "mcp") await deleteMcp(target.name);
    else if (target.kind === "yaml") await deleteYamlTool(target.name);
    else deleteHandler.mutate(target.id);
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
      <PageContainer>
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

          <form onSubmit={saveMcp}>
            <Card className="p-6 space-y-5">
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
            <div className="rounded-lg border border-border/50 bg-muted/10 p-4 space-y-4">
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
              <Button type="button" variant="ghost" onClick={cancelEdit}>
                {t("common.cancel")}
              </Button>
              <Button type="submit" disabled={formBusy}>
                <Save className="h-4 w-4" /> {formBusy ? t("common.saving") : isNew ? t("common.create") : t("common.save")}
              </Button>
            </div>
            </Card>
          </form>
        </div>
      </PageContainer>
    );
  }

  /* ── YAML Tool Edit Form (full-page) ─────────────────────────────── */

  if (editView?.kind === "yaml") {
    const isNewYaml = editView.id === "new";
    return (
      <PageContainer>
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
            <form onSubmit={saveYaml}>
              <Card className="p-6 space-y-5">
              <Field label={t("tools.yaml_config")} hint={t("tools.yaml_edit_hint")}>
                <Textarea
                  value={yamlContent}
                  onChange={(e) => setYamlContent(e.target.value)}
                  rows={24}
                  spellCheck={false}
                  className="font-mono leading-relaxed resize-y"
                />
              </Field>
              <div className="flex justify-end gap-3 pt-3 border-t border-border/50">
                <Button type="button" variant="ghost" onClick={cancelEdit}>
                  {t("common.cancel")}
                </Button>
                <Button type="submit" disabled={formBusy}>
                  <Save className="h-4 w-4" /> {formBusy ? t("common.saving") : isNewYaml ? t("common.create") : t("common.save")}
                </Button>
              </div>
              </Card>
            </form>
          )}
        </div>
      </PageContainer>
    );
  }

  /* ── Card renderers ──────────────────────────────────────────────── */

  const renderYamlCard = (tool: YamlToolEntry, idx: number) => {
    const pending = actionPending === tool.name;
    return (
      <Card key={`yaml-${tool.name}-${idx}`}
        className={`flex flex-col gap-3 p-5 min-w-0 overflow-hidden ${tool.status === "disabled" ? "opacity-50" : ""}`}>
        <div className="flex items-start justify-between gap-2">
          <div className="flex items-center gap-3 min-w-0">
            <IconTile
              tone={tool.status === "verified" ? "success" : tool.status === "draft" ? "warning" : "muted"}
              size="sm"
            >
              <FileCode2 />
            </IconTile>
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
            <span className="font-mono text-primary/80 truncate" title={tool.endpoint}>{tool.endpoint}</span>
          </div>
        </div>
        <div className="flex gap-1.5 pt-1">
          <Button variant="outline" size="sm" disabled={pending} onClick={() => startEditYaml(tool.name)} className="flex-1">
            <Pencil className="h-4 w-4" /> {t("common.edit")}
          </Button>
          {tool.status === "draft" && (
            <Button variant="outline-success" size="sm" disabled={pending} aria-label={t("tools.verify")}
              onClick={() => handleVerify(tool.name)}>
              <CheckCircle2 className="h-4 w-4" />
            </Button>
          )}
          {tool.status === "disabled" ? (
            <Button variant="outline-success" size="sm" disabled={pending} aria-label={t("tools.enable")}
              onClick={() => handleEnable(tool.name)}>
              <Play className="h-4 w-4" />
            </Button>
          ) : (
            <Button variant="outline-warning" size="sm" disabled={pending} aria-label={t("tools.disable")}
              onClick={() => handleDisable(tool.name)}>
              <Square className="h-4 w-4" />
            </Button>
          )}
          <Button variant="outline-destructive" size="sm" disabled={pending} aria-label={t("common.delete")}
            onClick={() => setDeleteConfirm({ kind: "yaml", name: tool.name })}>
            <Trash2 className="h-4 w-4" />
          </Button>
        </div>
      </Card>
    );
  };

  const renderMcpCard = (s: McpEntry) => {
    const isRunning = s.status === "running";
    const active = s.enabled && isRunning;
    const endpoint = s.url ?? (s.container && s.port ? `${s.container}:${s.port}` : s.container ?? "—");
    const pending = actionPending === s.name || actionPending === "reload:" + s.name || actionPending === "toggle:" + s.name;

    return (
      <Card key={`mcp-${s.name}`}
        className={`flex flex-col gap-3 p-5 min-w-0 overflow-hidden ${!s.enabled ? "opacity-50" : ""}`}>
        <div className="flex items-start justify-between gap-2">
          <div className="flex items-center gap-3 min-w-0">
            <IconTile tone="muted" size="sm">
              <Activity />
            </IconTile>
            <span className="font-mono text-sm font-bold text-foreground break-words leading-snug min-w-0" title={s.name}>{s.name}</span>
          </div>
        </div>
        <div className="space-y-1.5 mt-auto text-xs">
          <Row label={t("tools.mode")} value={s.mode} />
          <div className="flex flex-col gap-0.5 bg-muted/20 rounded px-2.5 py-1.5 border border-border/50 overflow-hidden">
            <span className="text-muted-foreground">{s.url ? t("tools.url") : t("tools.container")}</span>
            <span className="font-mono text-primary/80 truncate" title={endpoint}>{endpoint}</span>
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
            <Pencil className="h-4 w-4" /> {t("common.edit")}
          </Button>
          {s.enabled && s.status === "running" && (
            <Button variant="outline" size="sm" disabled={pending} aria-label={t("tools.reload")}
              onClick={() => reloadMcp(s.name)}>
              <RotateCcw className={`h-4 w-4 ${actionPending === "reload:" + s.name ? "animate-spin" : ""}`} />
            </Button>
          )}
          <Button
            variant={s.enabled ? "outline-warning" : "outline-success"}
            size="sm"
            disabled={pending}
            aria-label={s.enabled ? t("tools.disable") : t("tools.enable")}
            onClick={() => toggleMcp(s.name)}
          >
            {s.enabled ? <Square className="h-4 w-4" /> : <Play className="h-4 w-4" />}
          </Button>
          <Button variant="outline-destructive" size="sm" disabled={pending} aria-label={t("common.delete")}
            onClick={() => setDeleteConfirm({ kind: "mcp", name: s.name })}>
            <Trash2 className="h-4 w-4" />
          </Button>
        </div>
      </Card>
    );
  };

  const sourceBadgeLabel = (source: HandlerAdminRow["source"]) => {
    if (source === "override") return t("tools.handler_source_override");
    if (source === "workspace") return t("tools.handler_source_workspace");
    return t("tools.handler_source_builtin");
  };

  const renderHandlerCard = (h: HandlerAdminRow) => {
    const label = h.labels?.[lang] ?? h.labels?.en ?? h.id;
    const description = h.descriptions?.[lang] ?? h.descriptions?.en ?? "";
    const isBuiltin = h.tier === "builtin";
    const pending = handlerPendingId === h.id;
    const isDeleting = deleteHandler.isPending && deleteHandler.variables === h.id;
    return (
      <Card key={`handler-${h.id}`}
        className={`flex flex-col gap-3 p-5 min-w-0 overflow-hidden ${isBuiltin && !h.enabled ? "opacity-50" : ""}`}>
        <div className="flex items-start justify-between gap-2">
          <div className="flex items-center gap-3 min-w-0">
            <IconTile tone="muted" size="sm">
              <FileCog />
            </IconTile>
            <span className="font-mono text-sm font-bold text-foreground break-words leading-snug min-w-0" title={h.id}>{label}</span>
          </div>
          <div className="flex flex-col items-end gap-1 shrink-0">
            <TypeBadge type={isBuiltin ? "INT" : "EXT"} />
            <Badge variant="secondary" size="xs">
              {h.execution === "async" ? t("tools.handler_async") : t("tools.handler_sync")}
            </Badge>
            <Badge variant="outline" size="xs">
              {sourceBadgeLabel(h.source)}
            </Badge>
          </div>
        </div>
        {description && (
          <p className="text-xs text-muted-foreground line-clamp-2">{description}</p>
        )}
        <div className="space-y-1.5 mt-auto text-xs">
          <Row label={t("tools.handler_tier")} value={isBuiltin ? t("tools.handler_builtin") : t("tools.handler_workspace")} />
          {h.match?.mime?.length ? (
            <div className="flex flex-col gap-0.5 bg-muted/20 rounded px-2.5 py-1.5 border border-border/50 overflow-hidden">
              <span className="text-muted-foreground">{t("tools.handler_mime")}</span>
              <span className="font-mono text-primary/80 truncate" title={h.match.mime.join(", ")}>{h.match.mime.join(", ")}</span>
            </div>
          ) : null}
          {h.provider && (
            <Row label={t("tools.handler_provider")} value={h.provider} />
          )}
        </div>
        <div className="flex flex-wrap items-center justify-between gap-2 pt-1">
          {isBuiltin ? (
            <Switch
              aria-label={h.id}
              checked={h.enabled}
              disabled={pending}
              onCheckedChange={(v) => {
                setHandlerPendingId(h.id);
                setHandlerAllowlist.mutate(
                  { action_ref: h.id, enabled: v },
                  { onSettled: () => setHandlerPendingId(null) },
                );
              }}
            />
          ) : (
            <Badge variant="secondary" size="xs">{t("tools.handler_always_on")}</Badge>
          )}
          <div className="flex flex-wrap gap-1.5">
            <Button
              variant="outline"
              size="sm"
              disabled={pending || isDeleting}
              onClick={() => setHandlerEditorId(h.id)}
              aria-label={t("tools.handler_edit")}
            >
              <Pencil className="h-4 w-4" /> {t("tools.handler_edit")}
            </Button>
            {h.source !== "builtin" && (
              <Button
                variant="outline-destructive"
                size="sm"
                disabled={pending || isDeleting}
                onClick={() => setDeleteConfirm({ kind: "handler", id: h.id, name: label, isReset: h.source === "override" })}
                aria-label={h.source === "override" ? t("tools.handler_reset") : t("tools.handler_delete")}
              >
                {h.source === "override" ? (
                  <RotateCcw className="h-4 w-4" />
                ) : (
                  <Trash2 className="h-4 w-4" />
                )}
                {h.source === "override" ? t("tools.handler_reset") : t("tools.handler_delete")}
              </Button>
            )}
          </div>
        </div>
      </Card>
    );
  };

  /* ── Tabbed view ─────────────────────────────────────────────────── */

  return (
    <PageContainer>
      <div>
        {/* Header */}
        <PageHeader
          title={t("tools.title")}
          description={t("tools.subtitle")}
          actions={
            <div className="flex flex-wrap items-center gap-2">
              <Button variant="outline" size="lg" onClick={startCreateYaml} className="w-full md:w-auto gap-2">
                <Plus className="h-4 w-4" /> {t("tools.external_apis")}
              </Button>
              <Button variant="outline" size="lg" onClick={() => setImportOpen(true)} className="w-full md:w-auto gap-2">
                <Download className="h-4 w-4" /> {t("tools.import_openapi_button")}
              </Button>
              <Button variant="outline" size="lg" onClick={startCreateMcp} className="w-full md:w-auto gap-2">
                <Plus className="h-4 w-4" /> {t("tools.add_mcp")}
              </Button>
              <Button variant="outline" size="lg" onClick={() => setHandlerEditorId("create")} className="w-full md:w-auto gap-2">
                <Plus className="h-4 w-4" /> {t("tools.handler_create")}
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
            <FilterTabsList
              items={[
                { value: "external", label: t("tools.external_apis"), icon: <ExternalLink />, count: yamlTools.length },
                { value: "mcp", label: t("tools.mcp_servers"), icon: <Activity />, count: mcpServers.length },
                { value: "handlers", label: t("tools.file_handlers"), icon: <FileCog />, count: handlers.length },
              ]}
            />

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

            {/* ── File Handlers ── */}
            <TabsContent value="handlers" className="mt-6">
              {handlers.length === 0 ? (
                <EmptyState
                  icon={FileCog}
                  text={t("tools.no_handlers")}
                  hint={
                    <a href="/workspace/" className="mt-3 text-xs text-primary hover:underline">
                      {t("tools.add_handler")}
                    </a>
                  }
                />
              ) : (
                <div className="grid gap-4 md:grid-cols-2 lg:grid-cols-3 2xl:grid-cols-4">
                  {handlers.map((h) => renderHandlerCard(h))}
                </div>
              )}
            </TabsContent>
          </Tabs>
        )}
      </div>

      {handlerEditorId != null && (
        <HandlerEditorWrapper
          editorId={handlerEditorId}
          onSaved={() => {
            qc.invalidateQueries({ queryKey: qk.handlers });
            setHandlerEditorId(null);
          }}
          onClose={() => setHandlerEditorId(null)}
        />
      )}

      <AlertDialog open={!!deleteConfirm} onOpenChange={(open) => { if (!open) setDeleteConfirm(null); }}>
        <AlertDialogContent>
          <AlertDialogHeader>
            {deleteConfirm?.kind === "handler" ? (
              <>
                <AlertDialogTitle>
                  {deleteConfirm.isReset
                    ? t("tools.handler_reset_confirm_title", { name: deleteConfirm.name })
                    : t("tools.handler_delete_confirm_title", { name: deleteConfirm.name })}
                </AlertDialogTitle>
                <AlertDialogDescription>
                  {deleteConfirm.isReset
                    ? t("tools.handler_reset_confirm_description")
                    : t("tools.handler_delete_confirm_description")}
                </AlertDialogDescription>
              </>
            ) : (
              <>
                <AlertDialogTitle>{t("tools.delete_confirm_title", { kind: deleteConfirm?.kind === "mcp" ? "MCP" : "YAML", name: deleteConfirm?.name ?? "" })}</AlertDialogTitle>
                <AlertDialogDescription>
                  {t("tools.delete_confirm_description")}
                </AlertDialogDescription>
              </>
            )}
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>{t("common.cancel")}</AlertDialogCancel>
            <AlertDialogAction variant="destructive" onClick={handleConfirmDelete}>
              {deleteConfirm?.kind === "handler" && deleteConfirm.isReset ? t("tools.handler_reset") : t("common.delete")}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>

      {/* OpenAPI import dialog */}
      <AlertDialog open={importOpen} onOpenChange={(open) => { if (!open && !importBusy) setImportOpen(false); }}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>{t("tools.import_openapi_title")}</AlertDialogTitle>
            <AlertDialogDescription>{t("tools.import_openapi_desc")}</AlertDialogDescription>
          </AlertDialogHeader>
          <div className="space-y-2">
            <Input
              autoFocus
              placeholder="https://api.example.com/openapi.json"
              value={importUrl}
              onChange={(e) => setImportUrl(e.target.value)}
              onKeyDown={(e) => { if (e.key === "Enter" && importUrl.trim() && !importBusy) runImportOpenapi(); }}
            />
            <Input
              placeholder={t("tools.import_openapi_prefix")}
              value={importPrefix}
              onChange={(e) => setImportPrefix(e.target.value)}
            />
          </div>
          <AlertDialogFooter>
            <AlertDialogCancel disabled={importBusy}>{t("common.cancel")}</AlertDialogCancel>
            <Button onClick={runImportOpenapi} disabled={importBusy || !importUrl.trim()} className="gap-2">
              {importBusy ? <RefreshCw className="h-4 w-4 animate-spin" /> : <Download className="h-4 w-4" />}
              {t("tools.import_openapi_run")}
            </Button>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </PageContainer>
  );
}
