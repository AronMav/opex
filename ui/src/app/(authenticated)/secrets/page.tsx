"use client";

import { useState, useCallback, useEffect } from "react";
import { useSecrets, useUpsertSecret, useDeleteSecret, useAgents } from "@/lib/queries";
import { apiGet } from "@/lib/api";
import { formatDate } from "@/lib/format";
import { useTranslation } from "@/hooks/use-translation";
import { ErrorBanner } from "@/components/ui/error-banner";
import { PageHeader } from "@/components/ui/page-header";
import { Skeleton } from "@/components/ui/skeleton";
import { Input } from "@/components/ui/input";
import { Field } from "@/components/ui/field";
import { EmptyState } from "@/components/ui/empty-state";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Card } from "@/components/ui/card";
import { PageContainer } from "@/components/ui/page-container";
import { StatusBadge } from "@/components/ui/status-badge";
import { IconTile } from "@/components/ui/icon-tile";
import { DataRow } from "@/components/ui/data-row";
import { CopyableCode } from "@/components/ui/copyable-code";
import { ConfirmDialog } from "@/components/ui/confirm-dialog";
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
import { KeyRound, Plus, RefreshCw, Trash2, Edit3, Eye } from "lucide-react";
import { toast } from "sonner";

// Auto-hide duration for the revealed secret value.
const REVEAL_TTL_SECONDS = 30;

// ── Form helpers (extracted for testability) ────────────────────────────────

export function buildAddSecretBody(
  name: string,
  value: string,
  desc: string,
  scope: string,
): { name: string; value: string; description?: string; scope?: string } | null {
  if (!name.trim() || !value.trim()) return null;
  const body: { name: string; value: string; description?: string; scope?: string } = {
    name: name.trim(),
    value: value.trim(),
  };
  const trimmedDesc = desc.trim();
  if (trimmedDesc) body.description = trimmedDesc;
  if (scope && scope !== "__global__") body.scope = scope;
  return body;
}

export function buildEditSecretBody(
  editTarget: string,
  value: string,
  desc: string,
  scope: string,
): { name: string; value: string; description?: string; scope?: string } | null {
  if (!value.trim()) return null;
  const body: { name: string; value: string; description?: string; scope?: string } = {
    name: editTarget,
    value: value.trim(),
  };
  const trimmedDesc = desc.trim();
  if (trimmedDesc) body.description = trimmedDesc;
  if (scope) body.scope = scope;
  return body;
}

export function buildRevealUrl(name: string, scope: string): string {
  const scopeParam = scope ? `&scope=${encodeURIComponent(scope)}` : "";
  return `/api/secrets/${encodeURIComponent(name)}?reveal=true${scopeParam}`;
}

export default function SecretsPage() {
  const { t, locale } = useTranslation();
  const { data: secrets = [], isLoading, error, refetch } = useSecrets();
  const { data: agents = [] } = useAgents();
  const upsertSecret = useUpsertSecret();
  const deleteSecret = useDeleteSecret();

  const [newName, setNewName] = useState("");
  const [newValue, setNewValue] = useState("");
  const [newDesc, setNewDesc] = useState("");
  const [newScope, setNewScope] = useState("");
  const [deleteTarget, setDeleteTarget] = useState<string | null>(null);
  const [deleteTargetScope, setDeleteTargetScope] = useState("");
  const [editTarget, setEditTarget] = useState<string | null>(null);
  const [editValue, setEditValue] = useState("");
  const [editDesc, setEditDesc] = useState("");
  const [editScope, setEditScope] = useState("");
  const [revealedSecret, setRevealedSecret] = useState<{ name: string; value: string } | null>(null);
  const [revealCountdown, setRevealCountdown] = useState(REVEAL_TTL_SECONDS);

  const mutating = upsertSecret.isPending || deleteSecret.isPending;
  const actionError =
    (upsertSecret.error ? `${upsertSecret.error}` : null) ||
    (deleteSecret.error ? `${deleteSecret.error}` : null) ||
    "";

  const addSecret = useCallback(async () => {
    const body = buildAddSecretBody(newName, newValue, newDesc, newScope);
    if (!body) return;
    await upsertSecret.mutateAsync(body);
    setNewName("");
    setNewValue("");
    setNewDesc("");
    setNewScope("");
  }, [newName, newValue, newDesc, newScope, upsertSecret]);

  const [editValidation, setEditValidation] = useState("");

  const doEdit = useCallback(async () => {
    if (!editTarget) return;
    const body = buildEditSecretBody(editTarget, editValue, editDesc, editScope);
    if (!body) {
      setEditValidation(t("secrets.value_required"));
      return;
    }
    setEditValidation("");
    await upsertSecret.mutateAsync(body);
    setEditTarget(null);
    setEditValue("");
    setEditDesc("");
    setEditScope("");
  }, [editTarget, editValue, editDesc, editScope, t, upsertSecret]);

  const doDelete = useCallback(async () => {
    if (!deleteTarget) return;
    try {
      await deleteSecret.mutateAsync({ name: deleteTarget, scope: deleteTargetScope || undefined });
      setDeleteTarget(null);
      setDeleteTargetScope("");
    } catch (e) {
      toast.error(t("secrets.delete_error", { error: String(e) }));
    }
  }, [deleteTarget, deleteTargetScope, deleteSecret, t]);

  // Visible countdown → auto-hide the revealed value. Reset on each reveal;
  // clears the interval on hide/unmount.
  useEffect(() => {
    if (!revealedSecret) return;
    setRevealCountdown(REVEAL_TTL_SECONDS);
    const tick = setInterval(() => {
      setRevealCountdown((n) => {
        if (n <= 1) {
          setRevealedSecret(null);
          return 0;
        }
        return n - 1;
      });
    }, 1000);
    return () => clearInterval(tick);
  }, [revealedSecret]);

  const revealSecret = useCallback(async (name: string, scope: string) => {
    try {
      const result = await apiGet<{ name: string; value: string }>(buildRevealUrl(name, scope));
      setRevealedSecret({ name, value: result.value });
    } catch (e) {
      toast.error(`${e}`);
    }
  }, []);


  return (
    <PageContainer>
        <PageHeader
          title={t("secrets.title")}
          description={t("secrets.subtitle")}
          actions={
            <Button variant="outline" size="sm" onClick={() => refetch()} disabled={isLoading || mutating}>
              <RefreshCw className={`mr-2 h-4 w-4 ${isLoading ? 'animate-spin' : ''}`} /> {t("common.refresh")}
            </Button>
          }
        />

        {(error || actionError) && <ErrorBanner error={error ? `${error}` : actionError} />}

        <Card interactive={false} className="mb-8 p-4 md:p-6">
          <div className="mb-4 flex items-center gap-2">
            <Plus className="h-4 w-4 text-primary" />
            <span className="text-sm font-semibold text-foreground">{t("secrets.add_secret")}</span>
          </div>
          <div className="grid grid-cols-1 gap-3 md:grid-cols-2">
            <Field label={t("secrets.name_placeholder")}>
              <Input
                className="font-mono text-sm h-11"
                value={newName}
                onChange={(e) => setNewName(e.target.value)}
              />
            </Field>
            <Field label={t("secrets.value_placeholder")}>
              <Input
                type="password"
                className="font-mono text-sm h-11"
                value={newValue}
                onChange={(e) => setNewValue(e.target.value)}
              />
            </Field>
            <Field label={t("secrets.description_placeholder")}>
              <Input
                className="text-sm h-11"
                value={newDesc}
                onChange={(e) => setNewDesc(e.target.value)}
              />
            </Field>
            <Field label={t("secrets.scope")}>
              <Select value={newScope} onValueChange={setNewScope}>
                <SelectTrigger className="text-sm h-11">
                  <SelectValue placeholder={t("secrets.scope_global")} />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="__global__">{t("secrets.scope_global")}</SelectItem>
                  {agents.map((a) => (
                    <SelectItem key={a.name} value={a.name}>{a.name}</SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </Field>
            <Button onClick={addSecret} disabled={isLoading || mutating || !newName.trim() || !newValue.trim()} className="h-11 font-semibold md:col-span-2">
              {t("common.add")}
            </Button>
          </div>
        </Card>

        {isLoading ? (
          <div className="space-y-3">
            {[1, 2, 3].map((i) => (
              <Skeleton key={i} className="h-20 rounded-xl" />
            ))}
          </div>
        ) : secrets.length === 0 ? (
          <EmptyState icon={KeyRound} text={t("common.no_records_found")} height="h-40" />
        ) : (
          <div className="space-y-3 pb-8">
            {secrets.map((s) => (
              <DataRow
                key={s.name}
                leading={
                  <IconTile>
                    <KeyRound />
                  </IconTile>
                }
                title={
                  <span className="group-hover:text-primary transition-colors">{s.name}</span>
                }
                subtitle={t("secrets.updated_at", { date: formatDate(s.updated_at, locale) })}
                actions={
                  <>
                    <Button
                      variant="ghost"
                      size="icon-lg"
                      className="w-full md:w-auto text-muted-foreground hover:text-warning hover:bg-warning/10"
                      onClick={() => revealSecret(s.name, s.scope || "")}
                      disabled={isLoading || mutating}
                      title={t("secrets.reveal")}
                      aria-label={t("common.reveal")}
                    >
                      <Eye className="h-4 w-4" />
                    </Button>
                    <Button
                      variant="ghost"
                      size="icon-lg"
                      className="w-full md:w-auto text-muted-foreground hover:text-primary hover:bg-primary/10"
                      onClick={() => { setEditTarget(s.name); setEditValue(""); setEditDesc(s.description || ""); setEditScope(s.scope || ""); setEditValidation(""); }}
                      disabled={isLoading || mutating}
                      aria-label={t("common.edit")}
                    >
                      <Edit3 className="h-4 w-4" />
                    </Button>
                    <Button
                      variant="ghost"
                      size="icon-lg"
                      className="w-full md:w-auto text-muted-foreground hover:text-destructive hover:bg-destructive/10"
                      onClick={() => { setDeleteTarget(s.name); setDeleteTargetScope(s.scope || ""); }}
                      disabled={isLoading || mutating}
                      aria-label={t("common.delete")}
                    >
                      <Trash2 className="h-4 w-4" />
                    </Button>
                  </>
                }
              >
                <div className="flex flex-wrap items-center gap-3">
                  <StatusBadge status={s.has_value ? "active" : "empty"}>
                    {s.has_value ? t("secrets.active") : t("secrets.empty")}
                  </StatusBadge>
                  {s.scope && (
                    <Badge variant="outline-primary" size="xs" className="font-mono shrink-0 whitespace-nowrap">
                      {s.scope}
                    </Badge>
                  )}
                  <span className="flex-1 min-w-0 text-sm text-muted-foreground leading-relaxed">
                    {s.description || <span className="italic opacity-30">{t("secrets.description_not_set")}</span>}
                  </span>
                </div>
              </DataRow>
            ))}
          </div>
        )}

      <ConfirmDialog
        open={!!deleteTarget}
        onClose={() => setDeleteTarget(null)}
        onConfirm={doDelete}
        title={t("secrets.delete_title")}
        description={t("secrets.delete_description", { name: deleteTarget ?? "" })}
      />

      <Dialog open={!!editTarget} onOpenChange={(o) => { if (!o) setEditTarget(null); }}>
        <DialogContent size="md">
          <DialogHeader>
            <DialogTitle>{t("secrets.edit_title")}</DialogTitle>
            <p className="text-sm text-muted-foreground">{editTarget}</p>
          </DialogHeader>
          <div className="space-y-3 py-4">
            <Field label={t("secrets.new_value_placeholder")}>
              <Input
                type="password"
                value={editValue}
                onChange={(e) => setEditValue(e.target.value)}
                className="font-mono text-sm h-12"
                autoFocus
              />
            </Field>
            <Field label={t("secrets.description_placeholder")}>
              <Input
                value={editDesc}
                onChange={(e) => setEditDesc(e.target.value)}
                className="text-sm h-10"
              />
            </Field>
            <Field label={t("secrets.scope")}>
              <Select value={editScope || "__global__"} onValueChange={(v) => setEditScope(v === "__global__" ? "" : v)}>
                <SelectTrigger className="text-sm h-10">
                  <SelectValue placeholder={t("secrets.scope_global")} />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="__global__">{t("secrets.scope_global")}</SelectItem>
                  {agents.map((a) => (
                    <SelectItem key={a.name} value={a.name}>{a.name}</SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </Field>
          </div>
          {editValidation && <p className="text-xs text-destructive px-1">{editValidation}</p>}
          <DialogFooter className="gap-2">
            <Button variant="ghost" onClick={() => setEditTarget(null)}>{t("common.cancel")}</Button>
            <Button onClick={doEdit} disabled={!editValue.trim()}>{t("common.save")}</Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* Reveal secret dialog */}
      <Dialog open={!!revealedSecret} onOpenChange={(o) => { if (!o) setRevealedSecret(null); }}>
        <DialogContent size="md">
          <DialogHeader>
            <DialogTitle>{t("secrets.reveal_title")}</DialogTitle>
            <p className="text-sm text-muted-foreground">{revealedSecret?.name}</p>
          </DialogHeader>
          <div className="space-y-2 py-4">
            {revealedSecret && (
              <CopyableCode
                value={revealedSecret.value}
                onCopied={() => toast.success(t("secrets.value_copied"))}
              />
            )}
            <p role="status" aria-live="polite" className="text-xs text-muted-foreground-subtle tabular-nums">
              {t("secrets.hides_in", { n: revealCountdown })}
            </p>
          </div>
          <DialogFooter>
            <Button onClick={() => setRevealedSecret(null)}>{t("common.close")}</Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </PageContainer>
  );
}
