"use client";

import { useState, useCallback, useEffect } from "react";
import { useSecrets, useUpsertSecret, useDeleteSecret, useAgents } from "@/lib/queries";
import { apiGet } from "@/lib/api";
import { formatDate } from "@/lib/format";
import { useTranslation } from "@/hooks/use-translation";
import { ErrorBanner } from "@/components/ui/error-banner";
import { Input } from "@/components/ui/input";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
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
      toast.error(`Failed to delete secret: ${e}`);
    }
  }, [deleteTarget, deleteTargetScope, deleteSecret]);

  useEffect(() => {
    if (!revealedSecret) return;
    const timer = setTimeout(() => setRevealedSecret(null), 30_000);
    return () => clearTimeout(timer);
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
    <div className="flex-1 overflow-y-auto p-4 md:p-6 lg:p-8 selection:bg-primary/20">
        <div className="mb-8 flex flex-col gap-4 md:flex-row md:items-center md:justify-between">
          <div>
            <h2 className="font-display text-lg font-bold tracking-tight text-foreground">{t("secrets.title")}</h2>
            <p className="text-sm text-muted-foreground mt-1">
              {t("secrets.subtitle")}
            </p>
          </div>
          <div className="flex gap-2">
            <Button variant="outline" size="sm" onClick={() => refetch()} disabled={isLoading || mutating}>
              <RefreshCw className={`mr-2 h-4 w-4 ${isLoading ? 'animate-spin' : ''}`} /> {t("common.refresh")}
            </Button>
          </div>
        </div>

        {(error || actionError) && <ErrorBanner error={error ? `${error}` : actionError} />}

        <div className="mb-8 neu-card p-4 md:p-6">
          <div className="mb-4 flex items-center gap-2">
            <Plus className="h-4 w-4 text-primary" />
            <span className="text-sm font-semibold text-foreground">{t("secrets.add_secret")}</span>
          </div>
          <div className="grid grid-cols-1 gap-3 md:grid-cols-2">
            <Input
              placeholder={t("secrets.name_placeholder")}
              className="font-mono text-sm h-11"
              value={newName}
              onChange={(e) => setNewName(e.target.value)}
            />
            <Input
              type="password"
              placeholder={t("secrets.value_placeholder")}
              className="font-mono text-sm h-11"
              value={newValue}
              onChange={(e) => setNewValue(e.target.value)}
            />
            <Input
              placeholder={t("secrets.description_placeholder")}
              className="text-sm h-11"
              value={newDesc}
              onChange={(e) => setNewDesc(e.target.value)}
            />
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
            <Button onClick={addSecret} disabled={isLoading || mutating || !newName.trim() || !newValue.trim()} className="h-11 font-semibold md:col-span-2">
              {t("common.add")}
            </Button>
          </div>
        </div>

        {secrets.length === 0 ? (
          <div className="flex h-40 items-center justify-center rounded-xl border border-dashed border-border bg-muted/10">
            <p className="font-mono text-sm text-muted-foreground/40 uppercase tracking-wider">{t("common.no_records_found")}</p>
          </div>
        ) : (
          <div className="space-y-3 pb-8">
            {secrets.map((s) => (
              <div
                key={s.name}
                className="group relative flex flex-col md:flex-row md:items-center gap-4 neu-flat p-4 transition-all hover:border-primary/20"
              >
                <div className="flex items-center gap-3 md:min-w-[240px]">
                  <div className="flex h-10 w-10 shrink-0 items-center justify-center rounded-lg bg-primary/10 border border-primary/20">
                    <KeyRound className="h-5 w-5 text-primary" />
                  </div>
                  <div className="flex flex-col min-w-0">
                    <span className="break-all font-mono text-sm font-bold text-foreground group-hover:text-primary transition-colors">
                      {s.name}
                    </span>
                    <span className="font-mono text-xs text-muted-foreground/40 tabular-nums">
                      {t("secrets.updated_at", { date: formatDate(s.updated_at, locale) })}
                    </span>
                  </div>
                </div>

                <div className="flex flex-1 flex-wrap items-center gap-3">
                  <Badge variant={s.has_value ? "default" : "secondary"} className={`text-xs ${s.has_value ? 'bg-success/20 text-success border-success/30' : 'bg-muted text-muted-foreground border-border'}`}>
                    {s.has_value ? t("secrets.active") : t("secrets.empty")}
                  </Badge>
                  {s.scope && (
                    <Badge variant="outline" className="text-[10px] font-mono border-primary/40 text-primary bg-primary/5">
                      {s.scope}
                    </Badge>
                  )}
                  <span className="flex-1 min-w-0 sm:min-w-[150px] text-sm text-muted-foreground leading-relaxed">
                    {s.description || <span className="italic opacity-30">{t("secrets.description_not_set")}</span>}
                  </span>
                </div>

                <div className="grid grid-cols-3 md:flex md:items-center md:justify-end gap-2 border-t border-border/50 pt-3 md:border-0 md:pt-0 shrink-0">
                  <Button
                    variant="ghost"
                    size="icon-lg"
                    className="w-full md:w-auto text-muted-foreground hover:text-amber-500 hover:bg-amber-500/10"
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
                </div>
              </div>
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
        <DialogContent className="rounded-xl border-border bg-card max-w-[95vw] sm:max-w-md max-h-[90vh] overflow-y-auto">
          <DialogHeader>
            <DialogTitle className="text-base font-bold text-foreground">{t("secrets.edit_title")}</DialogTitle>
            <p className="text-sm text-muted-foreground mt-1">{editTarget}</p>
          </DialogHeader>
          <div className="space-y-3 py-4">
            <Input
              type="password"
              placeholder={t("secrets.new_value_placeholder")}
              value={editValue}
              onChange={(e) => setEditValue(e.target.value)}
              className="font-mono text-sm h-12"
              autoFocus
            />
            <Input
              placeholder={t("secrets.description_placeholder")}
              value={editDesc}
              onChange={(e) => setEditDesc(e.target.value)}
              className="text-sm h-10"
            />
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
        <DialogContent className="rounded-xl border-border bg-card max-w-[95vw] sm:max-w-md">
          <DialogHeader>
            <DialogTitle className="text-base font-bold text-foreground">{t("secrets.reveal_title")}</DialogTitle>
            <p className="text-sm text-muted-foreground mt-1">{revealedSecret?.name}</p>
          </DialogHeader>
          <div className="py-4">
            <div className="flex items-center gap-2 rounded-lg bg-muted/30 border border-border/50 px-3 py-3">
              <code className="flex-1 text-xs font-mono text-foreground break-all select-all">{revealedSecret?.value}</code>
            </div>
          </div>
          <DialogFooter>
            <Button onClick={() => setRevealedSecret(null)}>{t("common.close")}</Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
