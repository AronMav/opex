"use client";

import { useState, useEffect, useCallback } from "react";
import { useBackups, useCreateBackup } from "@/lib/queries";
import { getToken, apiGet, apiPut } from "@/lib/api";
import { formatDate, formatBytes } from "@/lib/format";
import { useTranslation } from "@/hooks/use-translation";
import { ErrorBanner } from "@/components/ui/error-banner";
import { PageHeader } from "@/components/ui/page-header";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { PageContainer } from "@/components/ui/page-container";
import { ConfirmDialog } from "@/components/ui/confirm-dialog";
import { Input } from "@/components/ui/input";
import { Field } from "@/components/ui/field";
import { Switch } from "@/components/ui/switch";
import { IconTile } from "@/components/ui/icon-tile";
import { DataRow } from "@/components/ui/data-row";
import { SectionHeader } from "@/components/ui/section-header";
import { CronSchedulePicker } from "@/components/ui/cron-schedule-picker";
import type { CronPreset } from "@/lib/cron";
import { Archive, Download, RefreshCw, Plus, RotateCcw, Trash2, Settings2 } from "lucide-react";
import { EmptyState } from "@/components/ui/empty-state";
import { Skeleton } from "@/components/ui/skeleton";
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
    <Card interactive={false} className="p-4">
      <SectionHeader icon={Settings2} title={t("backups.settings")} className="mb-4" />

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
        <div className="mt-4 space-y-4">
          <Field label={t("backups.cron_schedule")}>
            <CronSchedulePicker
              value={editCron}
              onChange={setEditCron}
              showTimezone={false}
              presets={BACKUP_PRESETS}
            />
          </Field>
          <Field label={t("backups.retention")} hint={t("backups.retention_hint")} className="max-w-56">
            <Input
              type="number"
              value={editRetention}
              onChange={(e) => setEditRetention(e.target.value)}
              min={1}
              max={365}
              className="font-mono text-sm"
              disabled={saving}
            />
          </Field>
          {hasChanges && (
            <Button size="sm" onClick={saveSettings} disabled={saving} className="w-full sm:w-auto">
              {t("common.save")}
            </Button>
          )}
        </div>
      )}
    </Card>
  );
}

// ── Main page ────────────────────────────────────────────────────────────────

export default function BackupsPage() {
  const { t, locale } = useTranslation();
  const { data: backups = [], isLoading, error, refetch } = useBackups();
  const createBackup = useCreateBackup();

  const [restoreTarget, setRestoreTarget] = useState<string | null>(null);
  const [deleteTarget, setDeleteTarget] = useState<string | null>(null);
  // Scoped to the acting row so only that backup shows "Restoring…".
  const [restoringFile, setRestoringFile] = useState<string | null>(null);
  const [deleting, setDeleting] = useState(false);
  const [actionError, setActionError] = useState("");

  const handleDownload = (filename: string) => {
    const token = getToken();
    const url = `/api/backup/${encodeURIComponent(filename)}`;
    const a = document.createElement("a");
    a.href = url;
    a.download = filename;
    // X-Confirm-Download required by backend (audit 2026-05-08): backup
    // archives contain plaintext secrets, so the explicit header acts as
    // defence-in-depth on top of the bearer token.
    fetch(url, {
      headers: {
        Authorization: `Bearer ${token}`,
        "X-Confirm-Download": "yes-i-am-sure",
      },
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
    const target = restoreTarget;
    setRestoringFile(target);
    setActionError("");
    try {
      // Note: The restore API only accepts blob body (no server-side filename endpoint).
      // This means the backup is downloaded to browser memory then re-uploaded.
      // For large backups this may be slow or fail on low-memory devices.
      const targetBackup = backups.find((b) => b.filename === target);
      const SIZE_WARN_THRESHOLD = 50 * 1024 * 1024; // 50MB
      if (targetBackup && targetBackup.size_bytes > SIZE_WARN_THRESHOLD) {
        toast.warning(t("backups.large_warning", { size: formatBytes(targetBackup.size_bytes) }));
      }

      const token = getToken();
      const resp = await fetch(`/api/backup/${encodeURIComponent(target)}`, {
        headers: {
          Authorization: `Bearer ${token}`,
          "X-Confirm-Download": "yes-i-am-sure",
        },
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
      toast.success(t("backups.restore_success"));
      refetch();
    } catch (e) {
      setActionError(String(e));
    } finally {
      setRestoringFile(null);
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

  const mutating = createBackup.isPending || restoringFile !== null || deleting;
  const combinedError =
    (error ? `${error}` : "") ||
    (createBackup.error ? `${createBackup.error}` : "") ||
    actionError;

  return (
    <PageContainer>
        <PageHeader
          title={t("backups.title")}
          description={t("backups.subtitle")}
          actions={
            <div className="flex flex-wrap items-center gap-2">
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
                size="lg"
                onClick={() => createBackup.mutate()}
                disabled={isLoading || mutating}
                className="w-full md:w-auto gap-2"
              >
                <Plus className="h-4 w-4" />
                {createBackup.isPending ? t("backups.creating") : t("backups.create")}
              </Button>
            </div>
          }
        />

        <BackupSettings />

        {combinedError && <ErrorBanner error={combinedError} className="mt-6" />}

        {isLoading ? (
          <div className="mt-6 space-y-3">
            {[1, 2, 3].map((i) => (
              <Skeleton key={i} className="h-20 w-full rounded-xl" />
            ))}
          </div>
        ) : backups.length === 0 ? (
          <EmptyState icon={Archive} text={t("backups.empty")} height="h-40" className="mt-6" />
        ) : (
          <div className="mt-6 space-y-3 pb-8">
            {backups.map((b) => {
              const restoring = restoringFile === b.filename;
              return (
                <DataRow
                  key={b.filename}
                  interactive
                  leading={
                    <IconTile>
                      <Archive />
                    </IconTile>
                  }
                  title={
                    <span className="break-all group-hover:text-primary transition-colors">{b.filename}</span>
                  }
                  subtitle={b.created_at ? formatDate(b.created_at, locale) : "—"}
                  actions={
                    <>
                      <Button
                        variant="ghost"
                        size="sm"
                        onClick={() => handleDownload(b.filename)}
                        disabled={mutating}
                        title={t("backups.download")}
                      >
                        <Download className="h-4 w-4 md:mr-2" />
                        <span className="hidden md:inline">{t("backups.download")}</span>
                      </Button>
                      <Button
                        variant="outline-warning"
                        size="sm"
                        onClick={() => setRestoreTarget(b.filename)}
                        disabled={mutating}
                        title={restoring ? t("backups.restoring") : t("backups.restore")}
                      >
                        <RotateCcw className={`h-4 w-4 md:mr-2 ${restoring ? "animate-spin" : ""}`} />
                        <span className="hidden md:inline">{restoring ? t("backups.restoring") : t("backups.restore")}</span>
                      </Button>
                      <Button
                        variant="outline-destructive"
                        size="sm"
                        onClick={() => setDeleteTarget(b.filename)}
                        disabled={mutating}
                        title={t("common.delete")}
                      >
                        <Trash2 className="h-4 w-4 md:mr-2" />
                        <span className="hidden md:inline">{t("common.delete")}</span>
                      </Button>
                    </>
                  }
                >
                  <span className="font-mono text-sm text-muted-foreground tabular-nums">
                    {formatBytes(b.size_bytes)}
                  </span>
                </DataRow>
              );
            })}
          </div>
        )}

      <ConfirmDialog
        open={!!restoreTarget}
        onClose={() => setRestoreTarget(null)}
        onConfirm={handleRestore}
        title={t("backups.restore_title")}
        description={t("backups.restore_description", { filename: restoreTarget ?? "" })}
        variant="warning"
        confirmLabel={t("backups.restore")}
      />
      <ConfirmDialog
        open={!!deleteTarget}
        onClose={() => setDeleteTarget(null)}
        onConfirm={handleDelete}
        title={t("backups.delete_title")}
        description={t("backups.delete_description", { filename: deleteTarget ?? "" })}
      />
    </PageContainer>
  );
}
