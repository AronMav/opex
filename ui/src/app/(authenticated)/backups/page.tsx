"use client";

import { useState, useEffect, useCallback } from "react";
import { useBackups, useCreateBackup } from "@/lib/queries";
import { getToken, apiGet, apiPut } from "@/lib/api";
import { formatDate, formatBytes } from "@/lib/format";
import { useTranslation } from "@/hooks/use-translation";
import { ErrorBanner } from "@/components/ui/error-banner";
import { Button } from "@/components/ui/button";
import { ConfirmDialog } from "@/components/ui/confirm-dialog";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import { CronSchedulePicker } from "@/components/ui/cron-schedule-picker";
import type { CronPreset } from "@/lib/cron";
import { Archive, Download, RefreshCw, Plus, RotateCcw, Trash2, Settings2 } from "lucide-react";
import { apiDelete } from "@/lib/api";
import { toast } from "sonner";

// ── Backup-specific cron presets (5-field) ───────────────────────────────────

const BACKUP_PRESETS: CronPreset[] = [
  { value: "0 1 * * *", labelKey: "backups.cron_daily_1" },
  { value: "0 3 * * *", labelKey: "backups.cron_daily_3" },
  { value: "0 5 * * *", labelKey: "backups.cron_daily_5" },
  { value: "0 0 * * *", labelKey: "backups.cron_midnight" },
  { value: "0 5 * * 1", labelKey: "backups.cron_weekly_mon" },
  { value: "0 5 * * 0", labelKey: "backups.cron_weekly_sun" },
  { value: "0 0,12 * * *", labelKey: "backups.cron_twice_daily" },
];

/** Convert 6-field cron (with seconds) to 5-field for UI display */
function cron6to5(expr: string): string {
  const parts = expr.trim().split(/\s+/);
  return parts.length === 6 ? parts.slice(1).join(" ") : expr;
}

/** Convert 5-field cron to 6-field (prepend "0" seconds) for backend */
function cron5to6(expr: string): string {
  const parts = expr.trim().split(/\s+/);
  return parts.length === 5 ? `0 ${expr}` : expr;
}

// ── Backup settings section ──────────────────────────────────────────────────

interface BackupConfig {
  enabled: boolean;
  cron: string;
  retention_days: number;
}

function BackupSettings() {
  const { t } = useTranslation();
  const [config, setConfig] = useState<BackupConfig | null>(null);
  const [saving, setSaving] = useState(false);
  const [editCron, setEditCron] = useState("");
  const [editRetention, setEditRetention] = useState("");

  const loadConfig = useCallback(() => {
    apiGet<{ backup?: BackupConfig }>("/api/config")
      .then((d) => {
        if (d.backup) {
          setConfig(d.backup);
          setEditCron(cron6to5(d.backup.cron));
          setEditRetention(String(d.backup.retention_days));
        }
      })
      .catch(() => {});
  }, []);

  useEffect(() => { loadConfig(); }, [loadConfig]);

  const toggleEnabled = async (enabled: boolean) => {
    setSaving(true);
    try {
      await apiPut("/api/config", { backup_enabled: enabled });
      toast.success(enabled ? t("backups.scheduler_on") : t("backups.scheduler_off"));
      loadConfig();
    } catch (e) {
      toast.error(String(e));
    } finally {
      setSaving(false);
    }
  };

  const saveSettings = async () => {
    setSaving(true);
    try {
      const payload: Record<string, unknown> = {};
      if (cron5to6(editCron) !== config?.cron) payload.backup_cron = cron5to6(editCron);
      if (Number(editRetention) !== config?.retention_days) payload.backup_retention_days = Number(editRetention);
      if (Object.keys(payload).length > 0) {
        await apiPut("/api/config", payload);
        toast.success(t("backups.settings_saved"));
        loadConfig();
      }
    } catch (e) {
      toast.error(String(e));
    } finally {
      setSaving(false);
    }
  };

  if (!config) return null;

  const hasChanges = cron5to6(editCron) !== config.cron || Number(editRetention) !== config.retention_days;

  return (
    <div className="rounded-lg border border-border bg-card text-card-foreground shadow-sm p-4 space-y-4">
      <div className="flex items-center gap-2 text-sm font-semibold text-foreground">
        <Settings2 className="h-4 w-4" />
        {t("backups.settings")}
      </div>

      <div className="flex items-center justify-between">
        <div>
          <span className="text-sm font-medium">{t("backups.auto_backup")}</span>
          <p className="text-xs text-muted-foreground">{t("backups.auto_backup_desc")}</p>
        </div>
        <Switch
          checked={config.enabled}
          onCheckedChange={toggleEnabled}
          disabled={saving}
        />
      </div>

      {config.enabled && (
        <div className="space-y-4">
          <div className="space-y-1.5">
            <span className="text-sm font-medium text-muted-foreground ml-1">{t("backups.cron_schedule")}</span>
            <CronSchedulePicker
              value={editCron}
              onChange={setEditCron}
              showTimezone={false}
              presets={BACKUP_PRESETS}
            />
          </div>
          <div className="space-y-1.5 max-w-[200px]">
            <span className="text-sm font-medium text-muted-foreground ml-1">{t("backups.retention")}</span>
            <Input
              type="number"
              value={editRetention}
              onChange={(e) => setEditRetention(e.target.value)}
              min={1}
              max={365}
              className="font-mono text-sm"
              disabled={saving}
            />
            <p className="text-xs text-muted-foreground ml-1">{t("backups.retention_hint")}</p>
          </div>
          {hasChanges && (
            <Button size="sm" onClick={saveSettings} disabled={saving} className="w-full sm:w-auto">
              {t("common.save")}
            </Button>
          )}
        </div>
      )}
    </div>
  );
}

// ── Main page ────────────────────────────────────────────────────────────────

export default function BackupsPage() {
  const { t, locale } = useTranslation();
  const { data: backups = [], isLoading, error, refetch } = useBackups();
  const createBackup = useCreateBackup();

  const [restoreTarget, setRestoreTarget] = useState<string | null>(null);
  const [deleteTarget, setDeleteTarget] = useState<string | null>(null);
  const [restoring, setRestoring] = useState(false);
  const [deleting, setDeleting] = useState(false);
  const [actionError, setActionError] = useState("");

  const handleDownload = (filename: string) => {
    const token = getToken();
    const url = `/api/backup/${encodeURIComponent(filename)}`;
    const a = document.createElement("a");
    a.href = url;
    a.download = filename;
    // For auth, fetch as blob and create object URL
    fetch(url, {
      headers: { Authorization: `Bearer ${token}` },
    })
      .then((r) => {
        if (!r.ok) throw new Error(`HTTP ${r.status}`);
        return r.blob();
      })
      .then((blob) => {
        const objectUrl = URL.createObjectURL(blob);
        a.href = objectUrl;
        a.click();
        URL.revokeObjectURL(objectUrl);
      })
      .catch((e) => setActionError(String(e)));
  };

  const handleRestore = async () => {
    if (!restoreTarget) return;
    setRestoring(true);
    setActionError("");
    try {
      // Note: The restore API only accepts blob body (no server-side filename endpoint).
      // This means the backup is downloaded to browser memory then re-uploaded.
      // For large backups this may be slow or fail on low-memory devices.
      const targetBackup = backups.find((b) => b.filename === restoreTarget);
      const SIZE_WARN_THRESHOLD = 50 * 1024 * 1024; // 50MB
      if (targetBackup && targetBackup.size_bytes > SIZE_WARN_THRESHOLD) {
        toast.warning(`Large backup (${formatBytes(targetBackup.size_bytes)}). Restore may take a while.`);
      }

      const token = getToken();
      const resp = await fetch(`/api/backup/${encodeURIComponent(restoreTarget)}`, {
        headers: { Authorization: `Bearer ${token}` },
      });
      if (!resp.ok) throw new Error(`Failed to download backup: HTTP ${resp.status}`);
      const blob = await resp.blob();

      const restoreResp = await fetch("/api/restore", {
        method: "POST",
        headers: {
          Authorization: `Bearer ${token}`,
          "Content-Type": "application/octet-stream",
        },
        body: blob,
      });
      if (!restoreResp.ok) {
        const text = await restoreResp.text().catch(() => "");
        throw new Error(text || `HTTP ${restoreResp.status}`);
      }
      refetch();
    } catch (e) {
      setActionError(String(e));
    } finally {
      setRestoring(false);
      setRestoreTarget(null);
    }
  };

  const handleDelete = async () => {
    if (!deleteTarget) return;
    setDeleting(true);
    setActionError("");
    try {
      await apiDelete(`/api/backup/${encodeURIComponent(deleteTarget)}`);
      toast.success(t("backups.deleted"));
      refetch();
    } catch (e) {
      setActionError(String(e));
    } finally {
      setDeleting(false);
      setDeleteTarget(null);
    }
  };

  const mutating = createBackup.isPending || restoring || deleting;
  const combinedError =
    (error ? `${error}` : "") ||
    (createBackup.error ? `${createBackup.error}` : "") ||
    actionError;

  return (
    <div className="flex-1 overflow-y-auto p-4 md:p-6 lg:p-8 selection:bg-primary/20">
        <div className="mb-8 flex flex-col gap-4 md:flex-row md:items-center md:justify-between">
          <div>
            <h2 className="font-display text-lg font-bold tracking-tight text-foreground">
              {t("backups.title")}
            </h2>
            <p className="text-sm text-muted-foreground mt-1">
              {t("backups.subtitle")}
            </p>
          </div>
          <div className="grid grid-cols-2 md:flex gap-2">
            <Button
              variant="outline"
              size="sm"
              onClick={() => refetch()}
              disabled={isLoading || mutating}
            >
              <RefreshCw className={`mr-2 h-4 w-4 ${isLoading ? "animate-spin" : ""}`} />
              {t("common.refresh")}
            </Button>
            <Button
              size="sm"
              onClick={() => createBackup.mutate()}
              disabled={isLoading || mutating}
            >
              <Plus className="mr-2 h-4 w-4" />
              {createBackup.isPending ? t("backups.creating") : t("backups.create")}
            </Button>
          </div>
        </div>

        <BackupSettings />

        {combinedError && <ErrorBanner error={combinedError} className="mt-6" />}

        {backups.length === 0 ? (
          <div className="mt-6 flex h-40 items-center justify-center rounded-xl border border-dashed border-border bg-muted/10">
            <p className="font-mono text-sm text-muted-foreground/40 uppercase tracking-wider">
              {t("backups.empty")}
            </p>
          </div>
        ) : (
          <div className="mt-6 space-y-3 pb-8">
            {backups.map((b) => (
              <div
                key={b.filename}
                className="group relative flex flex-col md:flex-row md:items-center gap-4 neu-flat p-4 transition-all hover:border-primary/20"
              >
                <div className="flex items-center gap-3 md:min-w-[300px]">
                  <div className="flex h-10 w-10 shrink-0 items-center justify-center rounded-lg bg-primary/10 border border-primary/20">
                    <Archive className="h-5 w-5 text-primary" />
                  </div>
                  <div className="flex flex-col min-w-0">
                    <span className="break-all font-mono text-sm font-bold text-foreground group-hover:text-primary transition-colors">
                      {b.filename}
                    </span>
                    <span className="font-mono text-xs text-muted-foreground/40 tabular-nums">
                      {b.created_at ? formatDate(b.created_at, locale) : "—"}
                    </span>
                  </div>
                </div>

                <div className="flex flex-1 items-center gap-3">
                  <span className="font-mono text-sm text-muted-foreground tabular-nums">
                    {formatBytes(b.size_bytes)}
                  </span>
                </div>

                <div className="grid grid-cols-3 md:flex md:items-center md:justify-end gap-2 border-t border-border/50 pt-3 md:border-0 md:pt-0 shrink-0">
                  <Button
                    variant="ghost"
                    size="sm"
                    className="text-muted-foreground hover:text-primary hover:bg-primary/10"
                    onClick={() => handleDownload(b.filename)}
                    disabled={mutating}
                    title={t("backups.download")}
                  >
                    <Download className="h-4 w-4 md:mr-2" />
                    <span className="hidden md:inline">{t("backups.download")}</span>
                  </Button>
                  <Button
                    variant="ghost"
                    size="sm"
                    className="text-muted-foreground hover:text-destructive hover:bg-destructive/10"
                    onClick={() => setRestoreTarget(b.filename)}
                    disabled={mutating}
                    title={restoring ? t("backups.restoring") : t("backups.restore")}
                  >
                    <RotateCcw className="h-4 w-4 md:mr-2" />
                    <span className="hidden md:inline">{restoring ? t("backups.restoring") : t("backups.restore")}</span>
                  </Button>
                  <Button
                    variant="ghost"
                    size="sm"
                    className="text-muted-foreground hover:text-destructive hover:bg-destructive/10"
                    onClick={() => setDeleteTarget(b.filename)}
                    disabled={mutating}
                    title={t("common.delete")}
                  >
                    <Trash2 className="h-4 w-4 md:mr-2" />
                    <span className="hidden md:inline">{t("common.delete")}</span>
                  </Button>
                </div>
              </div>
            ))}
          </div>
        )}

      <ConfirmDialog
        open={!!restoreTarget}
        onClose={() => setRestoreTarget(null)}
        onConfirm={handleRestore}
        title={t("backups.restore_title")}
        description={t("backups.restore_description", { filename: restoreTarget ?? "" })}
      />
      <ConfirmDialog
        open={!!deleteTarget}
        onClose={() => setDeleteTarget(null)}
        onConfirm={handleDelete}
        title={t("backups.delete_title")}
        description={t("backups.delete_description", { filename: deleteTarget ?? "" })}
      />
    </div>
  );
}
