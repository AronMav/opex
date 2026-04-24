"use client";

import { useState, useCallback } from "react";
import {
  useWebhooks,
  useCreateWebhook,
  useUpdateWebhook,
  useDeleteWebhook,
  useAgents,
} from "@/lib/queries";
import { apiPost } from "@/lib/api";
import { copyText } from "@/lib/clipboard";
import { useQueryClient } from "@tanstack/react-query";
import { useTranslation } from "@/hooks/use-translation";
import { formatDate } from "@/lib/format";
import { ErrorBanner } from "@/components/ui/error-banner";
import { EmptyState } from "@/components/ui/empty-state";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import { Badge } from "@/components/ui/badge";
import { Textarea } from "@/components/ui/textarea";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogFooter,
} from "@/components/ui/dialog";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { ConfirmDialog } from "@/components/ui/confirm-dialog";
import { toast } from "sonner";
import { Copy, Plus, Trash2, Edit3, Webhook, Check, RefreshCw } from "lucide-react";
import type { WebhookEntry } from "@/types/api";

const emptyForm = { name: "", agent: "", prompt_prefix: "", webhook_type: "generic" as "generic" | "github", event_filter: "" };

export default function WebhooksPage() {
  const { t, locale } = useTranslation();
  const { data: webhooks = [], isLoading, error } = useWebhooks();
  const { data: agents = [] } = useAgents();
  const createWebhook = useCreateWebhook();
  const updateWebhook = useUpdateWebhook();
  const deleteWebhook = useDeleteWebhook();

  const [formOpen, setFormOpen] = useState(false);
  const [editId, setEditId] = useState<string | null>(null);
  const [form, setForm] = useState(emptyForm);
  const [deleteTarget, setDeleteTarget] = useState<WebhookEntry | null>(null);
  const [actionError, setActionError] = useState("");
  const [createdSecret, setCreatedSecret] = useState<string | null>(null);
  const [copiedField, setCopiedField] = useState<string | null>(null);

  const queryClient = useQueryClient();

  const mutating = createWebhook.isPending || updateWebhook.isPending || deleteWebhook.isPending;

  const doCopy = (text: string, label: string) => {
    copyText(text).then(() => {
      setCopiedField(label);
      toast.success(t("webhooks.url_copied"));
      setTimeout(() => setCopiedField(null), 2000);
    });
  };

  const webhookUrl = (name: string) =>
    typeof window !== "undefined" ? `${window.location.origin}/webhook/${name}` : `/webhook/${name}`;

  const openCreate = () => {
    setEditId(null);
    setForm({ ...emptyForm, agent: agents[0]?.name || "", webhook_type: "generic", event_filter: "" });
    setFormOpen(true);
  };

  const openEdit = (w: WebhookEntry) => {
    setEditId(w.id);
    setForm({ name: w.name, agent: w.agent_id, prompt_prefix: w.prompt_prefix || "", webhook_type: w.webhook_type || "generic", event_filter: w.event_filter?.join(", ") || "" });
    setFormOpen(true);
  };

  const eventFilterPayload = form.event_filter.trim()
    ? form.event_filter.split(",").map((s) => s.trim()).filter(Boolean)
    : undefined;

  const saveWebhook = useCallback(async () => {
    setActionError("");
    try {
      if (editId) {
        await updateWebhook.mutateAsync({
          id: editId,
          name: form.name,
          agent: form.agent,
          prompt_prefix: form.prompt_prefix || undefined,
          webhook_type: form.webhook_type,
          event_filter: eventFilterPayload,
        });
        setFormOpen(false);
      } else {
        const result = await createWebhook.mutateAsync({
          name: form.name,
          agent: form.agent,
          prompt_prefix: form.prompt_prefix || undefined,
          webhook_type: form.webhook_type,
          event_filter: eventFilterPayload,
        });
        setFormOpen(false);
        if (result?.secret) {
          setCreatedSecret(result.secret);
        }
      }
    } catch (e) {
      setActionError(`${e}`);
    }
  }, [editId, form, eventFilterPayload, updateWebhook, createWebhook]);

  const regenerateSecret = useCallback(async (webhookId: string) => {
    setActionError("");
    try {
      const result = await apiPost<{ secret: string }>(`/api/webhooks/${webhookId}/regenerate-secret`);
      queryClient.invalidateQueries({ queryKey: ["webhooks"] });
      if (result?.secret) {
        setCreatedSecret(result.secret);
      }
    } catch (e) {
      setActionError(`${e}`);
    }
  }, [queryClient]);

  const toggleEnabled = useCallback(async (w: WebhookEntry) => {
    setActionError("");
    try {
      await updateWebhook.mutateAsync({ id: w.id, enabled: !w.enabled });
    } catch (e) {
      setActionError(`${e}`);
    }
  }, [updateWebhook]);

  const doDelete = useCallback(async () => {
    if (!deleteTarget) return;
    setActionError("");
    try {
      await deleteWebhook.mutateAsync(deleteTarget.id);
      setDeleteTarget(null);
    } catch (e) {
      setActionError(`${e}`);
    }
  }, [deleteTarget, deleteWebhook]);

  const errorMessage = error ? `${error}` : actionError;

  return (
    <div className="flex-1 overflow-y-auto p-4 md:p-6 lg:p-8 selection:bg-primary/20">
      <div className="mb-8 flex flex-col gap-4 md:flex-row md:items-center md:justify-between">
        <div>
          <h2 className="font-display text-lg font-bold tracking-tight text-foreground">{t("webhooks.title")}</h2>
          <p className="text-sm text-muted-foreground mt-1">{t("webhooks.subtitle")}</p>
        </div>
        <Button
          onClick={openCreate}
          className="w-full md:w-auto h-11 px-6 text-sm font-semibold transition-all duration-200 active:scale-95"
        >
          <Plus className="mr-2 h-4 w-4" /> {t("webhooks.new_webhook")}
        </Button>
      </div>

      {errorMessage && <ErrorBanner error={errorMessage} />}

      {webhooks.length === 0 ? (
        <EmptyState icon={Webhook} text={t("webhooks.no_webhooks")} />
      ) : (
        <div className="space-y-3 pb-8">
          {webhooks.map((w) => (
            <div
              key={w.id}
              className={`group relative flex flex-col md:flex-row md:items-center gap-4 neu-flat p-4 transition-all hover:border-primary/20 ${
                !w.enabled ? "opacity-60 hover:opacity-100" : ""
              }`}
            >
              {/* Left: icon + name + badges */}
              <div className="flex items-center gap-3 md:min-w-[220px]">
                <div className="flex h-10 w-10 shrink-0 items-center justify-center rounded-lg bg-primary/10 border border-primary/20">
                  <Webhook className="h-5 w-5 text-primary" />
                </div>
                <div className="flex flex-col min-w-0">
                  <span className="font-mono text-sm font-bold text-foreground group-hover:text-primary transition-colors truncate">
                    {w.name}
                  </span>
                  <span className="font-mono text-xs text-muted-foreground/40 tabular-nums">
                    {t("webhooks.created_at", { date: formatDate(w.created_at, locale) })}
                  </span>
                </div>
              </div>

              {/* Center: badges + url + stats */}
              <div className="flex flex-1 flex-col gap-2 min-w-0">
                <div className="flex items-center gap-2 flex-wrap">
                  {w.agent_id && (
                    <Badge variant="outline" className="text-xs border-primary/40 text-primary bg-primary/5">
                      {w.agent_id}
                    </Badge>
                  )}
                  <Badge variant="outline" className="text-[10px] font-mono">
                    {w.webhook_type === "github" ? "GitHub" : "Generic"}
                  </Badge>
                  <Badge
                    variant={w.enabled ? "default" : "secondary"}
                    className={`text-xs ${w.enabled ? "bg-success/20 text-success border-success/30" : "bg-muted text-muted-foreground border-border"}`}
                  >
                    {w.enabled ? t("common.enabled") : t("common.disabled")}
                  </Badge>
                  {w.event_filter && w.event_filter.length > 0 && w.event_filter.map((ev) => (
                    <Badge key={ev} variant="outline" className="text-[10px] font-mono">
                      {ev}
                    </Badge>
                  ))}
                </div>
                <div className="flex items-center gap-2">
                  <code className="font-mono text-[11px] text-muted-foreground/60 bg-muted/30 px-2 py-0.5 rounded border border-border/40 truncate max-w-[360px]">
                    {webhookUrl(w.name)}
                  </code>
                  <button
                    type="button"
                    className="inline-flex items-center gap-1 text-[11px] text-muted-foreground hover:text-primary transition-colors cursor-pointer"
                    onClick={() => doCopy(webhookUrl(w.name), `url-${w.id}`)}
                  >
                    {copiedField === `url-${w.id}` ? <Check className="h-3 w-3 text-success" /> : <Copy className="h-3 w-3" />}
                  </button>
                </div>
                <div className="flex flex-wrap items-center gap-x-4 gap-y-1 text-xs text-muted-foreground/50">
                  <span>{t("webhooks.trigger_count", { count: w.trigger_count })}</span>
                  <span>
                    {w.last_triggered_at
                      ? `${t("webhooks.last_triggered")}: ${formatDate(w.last_triggered_at, locale)}`
                      : t("webhooks.never_triggered")}
                  </span>
                </div>
                {w.prompt_prefix && (
                  <p className="font-mono text-xs text-foreground/50 line-clamp-1 break-words">{w.prompt_prefix}</p>
                )}
              </div>

              {/* Right: actions */}
              <div className="flex items-center gap-2 border-t border-border/50 pt-3 md:border-0 md:pt-0 shrink-0">
                <Switch
                  checked={w.enabled}
                  onCheckedChange={() => toggleEnabled(w)}
                  disabled={mutating}
                  className="data-[state=checked]:bg-primary"
                />
                <Button
                  variant="ghost"
                  size="icon"
                  onClick={() => regenerateSecret(w.id)}
                  disabled={mutating}
                  className="text-muted-foreground hover:text-amber-500 hover:bg-amber-500/10"
                  title={t("webhooks.regenerate_secret")}
                  aria-label={t("common.regenerate_secret")}
                >
                  <RefreshCw className="h-4 w-4" />
                </Button>
                <Button
                  variant="ghost"
                  size="icon"
                  onClick={() => openEdit(w)}
                  disabled={mutating}
                  className="text-muted-foreground hover:text-primary hover:bg-primary/10"
                  aria-label={t("common.edit")}
                >
                  <Edit3 className="h-4 w-4" />
                </Button>
                <Button
                  variant="ghost"
                  size="icon"
                  onClick={() => setDeleteTarget(w)}
                  disabled={mutating}
                  className="text-muted-foreground hover:text-destructive hover:bg-destructive/10"
                  aria-label={t("common.delete")}
                >
                  <Trash2 className="h-4 w-4" />
                </Button>
              </div>
            </div>
          ))}
        </div>
      )}

      {/* Create / Edit dialog */}
      <Dialog open={formOpen} onOpenChange={setFormOpen}>
        <DialogContent className="border-border rounded-xl max-w-[95vw] sm:max-w-xl max-h-[90vh] overflow-y-auto">
          <DialogHeader className="p-6 border-b border-border/50">
            <DialogTitle className="text-base font-bold text-foreground">
              {editId ? t("webhooks.editing") : t("webhooks.new_dialog")}
            </DialogTitle>
          </DialogHeader>
          <div className="p-6 space-y-5">
            <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
              <div className="space-y-2">
                <label className="text-sm font-medium text-muted-foreground ml-1">{t("webhooks.field_name")}</label>
                <Input
                  placeholder="my-webhook"
                  value={form.name}
                  onChange={(e) => setForm({ ...form, name: e.target.value })}
                  className="font-mono text-sm h-11"
                />
              </div>
              <div className="space-y-2">
                <label className="text-sm font-medium text-muted-foreground ml-1">{t("webhooks.field_agent")}</label>
                <Select value={form.agent} onValueChange={(v) => setForm({ ...form, agent: v })}>
                  <SelectTrigger className="font-mono text-sm h-11 w-full">
                    <SelectValue placeholder={t("webhooks.select_agent")} />
                  </SelectTrigger>
                  <SelectContent>
                    {agents.map((a) => (
                      <SelectItem key={a.name} value={a.name} className="font-mono">{a.name}</SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </div>
            </div>
            <div className="space-y-2">
              <label className="text-sm font-medium text-muted-foreground ml-1">{t("webhooks.field_type")}</label>
              <Select value={form.webhook_type} onValueChange={(v) => setForm({ ...form, webhook_type: v as "generic" | "github" })}>
                <SelectTrigger className="font-mono text-sm h-11 w-full">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="generic" className="font-mono">Generic (Bearer token)</SelectItem>
                  <SelectItem value="github" className="font-mono">GitHub (HMAC-SHA256)</SelectItem>
                </SelectContent>
              </Select>
            </div>
            {form.webhook_type === "github" && (
              <div className="space-y-2">
                <label className="text-sm font-medium text-muted-foreground ml-1">{t("webhooks.field_event_filter")}</label>
                <Input
                  value={form.event_filter}
                  placeholder="push, pull_request, issues"
                  onChange={(e) => setForm({ ...form, event_filter: e.target.value })}
                  className="font-mono text-sm h-11"
                />
                <span className="text-xs text-muted-foreground ml-1">{t("webhooks.event_filter_hint")}</span>
              </div>
            )}
            {form.name.trim() && (
              <div className="space-y-1.5">
                <label className="text-sm font-medium text-muted-foreground ml-1">{t("webhooks.endpoint_url")}</label>
                <div className="flex items-center gap-2 rounded-lg bg-muted/30 border border-border/50 px-3 py-2">
                  <code className="flex-1 text-xs font-mono text-primary/80 break-all select-all">
                    {webhookUrl(form.name.trim())}
                  </code>
                  <button
                    type="button"
                    className="inline-flex items-center text-muted-foreground hover:text-primary transition-colors cursor-pointer"
                    onClick={() => doCopy(webhookUrl(form.name.trim()), "form-url")}
                  >
                    {copiedField === "form-url" ? <Check className="h-3.5 w-3.5 text-success" /> : <Copy className="h-3.5 w-3.5" />}
                  </button>
                </div>
                <p className="text-[11px] text-muted-foreground/60 ml-1">
                  {form.webhook_type === "github"
                    ? t("webhooks.hint_github")
                    : t("webhooks.hint_generic")}
                </p>
              </div>
            )}
            <div className="space-y-2">
              <label className="text-sm font-medium text-muted-foreground ml-1">{t("webhooks.field_prompt_prefix")}</label>
              <Textarea
                placeholder={t("webhooks.prompt_prefix_placeholder")}
                value={form.prompt_prefix}
                onChange={(e) => setForm({ ...form, prompt_prefix: e.target.value })}
                className="font-mono text-sm min-h-[100px] resize-y"
              />
            </div>
          </div>
          <DialogFooter className="p-6 border-t border-border/50 gap-3">
            <Button variant="ghost" onClick={() => setFormOpen(false)}>{t("common.cancel")}</Button>
            <Button
              onClick={saveWebhook}
              disabled={mutating || !form.name.trim() || !form.agent}
            >
              {editId ? t("common.save") : t("common.create")}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <ConfirmDialog
        open={!!deleteTarget}
        onClose={() => setDeleteTarget(null)}
        onConfirm={doDelete}
        title={t("webhooks.delete_title")}
        description={t("webhooks.delete_description", { name: deleteTarget?.name ?? "" })}
      />

      {/* Secret reveal dialog */}
      <Dialog open={!!createdSecret} onOpenChange={(o) => { if (!o) setCreatedSecret(null); }}>
        <DialogContent className="border-border rounded-xl max-w-[95vw] sm:max-w-lg">
          <DialogHeader className="p-6 border-b border-border/50">
            <DialogTitle className="text-base font-bold text-foreground">
              {t("webhooks.secret_created_title")}
            </DialogTitle>
          </DialogHeader>
          <div className="p-6 space-y-4">
            <p className="text-sm text-muted-foreground">{t("webhooks.secret_created_warning")}</p>
            <div className="flex items-center gap-2 rounded-lg bg-muted/30 border border-border/50 px-3 py-3">
              <code className="flex-1 text-xs font-mono text-foreground break-all select-all">{createdSecret}</code>
              <button
                type="button"
                className="inline-flex items-center text-muted-foreground hover:text-primary transition-colors cursor-pointer"
                onClick={() => { if (createdSecret) { doCopy(createdSecret, "secret"); } }}
              >
                {copiedField === "secret" ? <Check className="h-3.5 w-3.5 text-success" /> : <Copy className="h-3.5 w-3.5" />}
              </button>
            </div>
          </div>
          <DialogFooter className="p-6 border-t border-border/50">
            <Button onClick={() => setCreatedSecret(null)}>{t("common.close")}</Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
