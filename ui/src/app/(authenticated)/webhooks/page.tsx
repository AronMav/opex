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
import { useQueryClient } from "@tanstack/react-query";
import { useTranslation } from "@/hooks/use-translation";
import { formatDate } from "@/lib/format";
import { ErrorBanner } from "@/components/ui/error-banner";
import { PageHeader } from "@/components/ui/page-header";
import { PageContainer } from "@/components/ui/page-container";
import { EmptyState } from "@/components/ui/empty-state";
import { Skeleton } from "@/components/ui/skeleton";
import { Field } from "@/components/ui/field";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import { Badge } from "@/components/ui/badge";
import { StatusBadge } from "@/components/ui/status-badge";
import { IconTile } from "@/components/ui/icon-tile";
import { DataRow } from "@/components/ui/data-row";
import { CopyableCode } from "@/components/ui/copyable-code";
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
import { Plus, Trash2, Edit3, Webhook, RefreshCw } from "lucide-react";
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
  const [regenTarget, setRegenTarget] = useState<WebhookEntry | null>(null);

  const queryClient = useQueryClient();

  const mutating = createWebhook.isPending || updateWebhook.isPending || deleteWebhook.isPending;

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

  const regenerateSecret = useCallback(async () => {
    if (!regenTarget) return;
    setActionError("");
    try {
      const result = await apiPost<{ secret: string }>(`/api/webhooks/${regenTarget.id}/regenerate-secret`);
      queryClient.invalidateQueries({ queryKey: ["webhooks"] });
      setRegenTarget(null);
      if (result?.secret) {
        setCreatedSecret(result.secret);
      }
    } catch (e) {
      setActionError(`${e}`);
    }
  }, [regenTarget, queryClient]);

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
    <PageContainer>
      <PageHeader
        title={t("webhooks.title")}
        description={t("webhooks.subtitle")}
        actions={
          <Button
            size="lg"
            onClick={openCreate}
            className="w-full md:w-auto gap-2"
          >
            <Plus className="h-4 w-4" /> {t("webhooks.new_webhook")}
          </Button>
        }
      />

      {errorMessage && <ErrorBanner error={errorMessage} />}

      {isLoading ? (
        <div className="space-y-3">
          {[1, 2, 3].map((i) => (
            <Skeleton key={i} className="h-28 rounded-xl" />
          ))}
        </div>
      ) : webhooks.length === 0 ? (
        <EmptyState icon={Webhook} text={t("webhooks.no_webhooks")} />
      ) : (
        <div className="space-y-3 pb-8">
          {webhooks.map((w) => (
            <DataRow
              key={w.id}
              muted={!w.enabled}
              leading={
                <IconTile>
                  <Webhook />
                </IconTile>
              }
              title={
                <span className="group-hover:text-primary transition-colors">{w.name}</span>
              }
              subtitle={t("webhooks.created_at", { date: formatDate(w.created_at, locale) })}
              actions={
                <>
                  <Switch
                    checked={w.enabled}
                    onCheckedChange={() => toggleEnabled(w)}
                    disabled={mutating}
                  />
                  <Button
                    variant="ghost"
                    size="icon-sm"
                    onClick={() => setRegenTarget(w)}
                    disabled={mutating}
                    className="text-muted-foreground hover:text-warning hover:bg-warning/10"
                    title={t("webhooks.regenerate_secret")}
                    aria-label={t("common.regenerate_secret")}
                  >
                    <RefreshCw className="h-4 w-4" />
                  </Button>
                  <Button
                    variant="ghost"
                    size="icon-sm"
                    onClick={() => openEdit(w)}
                    disabled={mutating}
                    className="text-muted-foreground hover:text-primary hover:bg-primary/10"
                    aria-label={t("common.edit")}
                  >
                    <Edit3 className="h-4 w-4" />
                  </Button>
                  <Button
                    variant="ghost"
                    size="icon-sm"
                    onClick={() => setDeleteTarget(w)}
                    disabled={mutating}
                    className="text-muted-foreground hover:text-destructive hover:bg-destructive/10"
                    aria-label={t("common.delete")}
                  >
                    <Trash2 className="h-4 w-4" />
                  </Button>
                </>
              }
            >
              <div className="flex items-center gap-2 flex-wrap">
                {w.agent_id && (
                  <Badge variant="outline-primary">
                    {w.agent_id}
                  </Badge>
                )}
                <Badge variant="outline" size="xs" className="font-mono">
                  {w.webhook_type === "github" ? t("webhooks.type_github") : t("webhooks.type_generic")}
                </Badge>
                <StatusBadge status={w.enabled ? "enabled" : "disabled"}>
                  {w.enabled ? t("common.enabled") : t("common.disabled")}
                </StatusBadge>
                {w.event_filter?.map((ev) => (
                  <Badge key={ev} variant="outline" size="xs" className="font-mono">
                    {ev}
                  </Badge>
                ))}
              </div>
              <CopyableCode
                value={webhookUrl(w.name)}
                onCopied={() => toast.success(t("webhooks.url_copied"))}
              />
              <div className="flex flex-wrap items-center gap-x-4 gap-y-1 text-xs text-muted-foreground-subtle">
                <span>{t("webhooks.trigger_count", { count: w.trigger_count })}</span>
                <span>
                  {w.last_triggered_at
                    ? `${t("webhooks.last_triggered")}: ${formatDate(w.last_triggered_at, locale)}`
                    : t("webhooks.never_triggered")}
                </span>
              </div>
              {w.prompt_prefix && (
                <p className="font-mono text-xs text-foreground/80 line-clamp-1 break-words">{w.prompt_prefix}</p>
              )}
            </DataRow>
          ))}
        </div>
      )}

      {/* Create / Edit dialog */}
      <Dialog open={formOpen} onOpenChange={setFormOpen}>
        <DialogContent size="xl">
          <DialogHeader className="border-b border-border/50 pb-4">
            <DialogTitle>{editId ? t("webhooks.editing") : t("webhooks.new_dialog")}</DialogTitle>
          </DialogHeader>
          <div className="space-y-5">
            <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
              <Field label={t("webhooks.field_name")}>
                <Input
                  placeholder="my-webhook"
                  value={form.name}
                  onChange={(e) => setForm({ ...form, name: e.target.value })}
                  className="font-mono text-sm h-11"
                />
              </Field>
              <Field label={t("webhooks.field_agent")}>
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
              </Field>
            </div>
            <Field label={t("webhooks.field_type")}>
              <Select value={form.webhook_type} onValueChange={(v) => setForm({ ...form, webhook_type: v as "generic" | "github" })}>
                <SelectTrigger className="font-mono text-sm h-11 w-full">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="generic" className="font-mono">{t("webhooks.type_generic")}</SelectItem>
                  <SelectItem value="github" className="font-mono">{t("webhooks.type_github")}</SelectItem>
                </SelectContent>
              </Select>
            </Field>
            {form.webhook_type === "github" && (
              <Field label={t("webhooks.field_event_filter")} hint={t("webhooks.event_filter_hint")}>
                <Input
                  value={form.event_filter}
                  placeholder="push, pull_request, issues"
                  onChange={(e) => setForm({ ...form, event_filter: e.target.value })}
                  className="font-mono text-sm h-11"
                />
              </Field>
            )}
            {form.name.trim() && (
              <div className="space-y-1.5">
                <label className="text-sm font-medium text-muted-foreground ml-1">{t("webhooks.endpoint_url")}</label>
                <CopyableCode value={webhookUrl(form.name.trim())} onCopied={() => toast.success(t("webhooks.url_copied"))} />
                <p className="text-xs text-muted-foreground-subtle ml-1">
                  {form.webhook_type === "github" ? t("webhooks.hint_github") : t("webhooks.hint_generic")}
                </p>
              </div>
            )}
            <Field label={t("webhooks.field_prompt_prefix")}>
              <Textarea
                placeholder={t("webhooks.prompt_prefix_placeholder")}
                value={form.prompt_prefix}
                onChange={(e) => setForm({ ...form, prompt_prefix: e.target.value })}
                className="font-mono text-sm min-h-24 resize-y"
              />
            </Field>
          </div>
          <DialogFooter className="border-t border-border/50 pt-4">
            <Button variant="ghost" onClick={() => setFormOpen(false)}>{t("common.cancel")}</Button>
            <Button onClick={saveWebhook} disabled={mutating || !form.name.trim() || !form.agent}>
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

      <ConfirmDialog
        open={!!regenTarget}
        onClose={() => setRegenTarget(null)}
        onConfirm={regenerateSecret}
        title={t("webhooks.regenerate_confirm_title")}
        description={t("webhooks.regenerate_confirm_description", { name: regenTarget?.name ?? "" })}
        variant="warning"
        confirmLabel={t("webhooks.regenerate_secret")}
      />

      {/* Secret reveal dialog */}
      <Dialog open={!!createdSecret} onOpenChange={(o) => { if (!o) setCreatedSecret(null); }}>
        <DialogContent size="lg">
          <DialogHeader className="border-b border-border/50 pb-4">
            <DialogTitle>{t("webhooks.secret_created_title")}</DialogTitle>
          </DialogHeader>
          <div className="space-y-4">
            <p className="text-sm text-muted-foreground">{t("webhooks.secret_created_warning")}</p>
            {createdSecret && <CopyableCode value={createdSecret} onCopied={() => toast.success(t("webhooks.url_copied"))} />}
          </div>
          <DialogFooter className="border-t border-border/50 pt-4">
            <Button onClick={() => setCreatedSecret(null)}>{t("common.close")}</Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </PageContainer>
  );
}
