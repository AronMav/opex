"use client";

// ── CheckpointPanel.tsx ───────────────────────────────────────────────────────
// Sheet-панель истории чекпойнтов агента с Diff и Restore.

import React, { useState } from "react";
import { toast } from "sonner";
import {
  Sheet,
  SheetContent,
  SheetHeader,
  SheetTitle,
} from "@/components/ui/sheet";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { ConfirmDialog } from "@/components/ui/confirm-dialog";
import { useCheckpoints, useRestoreCheckpoint } from "@/lib/queries";
import { diffCheckpoint } from "@/lib/api";
import { relativeTime } from "@/lib/format";
import { useTranslation } from "@/hooks/use-translation";

interface CheckpointPanelProps {
  agent: string | null;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

export function CheckpointPanel({ agent, open, onOpenChange }: CheckpointPanelProps) {
  const { t } = useTranslation();
  const { data, isLoading } = useCheckpoints(agent, open);
  const restore = useRestoreCheckpoint();

  // ── diff dialog state ────────────────────────────────────────────────────
  const [diffOpen, setDiffOpen] = useState(false);
  const [diffText, setDiffText] = useState<string | null>(null);
  const [diffN, setDiffN] = useState<number | null>(null);

  // ── restore confirm state ─────────────────────────────────────────────────
  const [confirmOpen, setConfirmOpen] = useState(false);
  const [restoreN, setRestoreN] = useState<number | null>(null);

  function handleDiff(n: number) {
    if (!agent) return;
    setDiffN(n);
    setDiffText(null);
    setDiffOpen(true);
    diffCheckpoint(agent, n)
      .then((r) => setDiffText(r.diff))
      .catch((e: Error) => {
        setDiffOpen(false);
        toast.error(e.message);
      });
  }

  function handleRestoreClick(n: number) {
    setRestoreN(n);
    setConfirmOpen(true);
  }

  function handleRestoreConfirm() {
    if (!agent || restoreN == null) return;
    setConfirmOpen(false);
    restore.mutate(
      { agent, n: restoreN },
      {
        onSuccess: () => toast.success(t("checkpoints.rollback_done", { n: restoreN })),
      },
    );
  }

  // ── body ─────────────────────────────────────────────────────────────────
  let body: React.ReactNode;

  if (isLoading) {
    body = (
      <p className="text-sm text-muted-foreground px-4 py-6 text-center">
        {t("checkpoints.loading")}
      </p>
    );
  } else if (!data?.enabled) {
    body = (
      <p className="text-sm text-muted-foreground px-4 py-6 text-center">
        {t("checkpoints.disabled")}
      </p>
    );
  } else if (data.items.length === 0) {
    body = (
      <p className="text-sm text-muted-foreground px-4 py-6 text-center">
        {t("checkpoints.empty")}
      </p>
    );
  } else {
    body = (
      <ul className="divide-y divide-border overflow-y-auto">
        {data.items.map((cp) => (
          <li
            key={cp.n}
            className="flex items-start justify-between gap-2 px-4 py-3 hover:bg-muted/20 transition-colors"
          >
            <div className="min-w-0 flex-1">
              <span className="font-mono text-xs text-primary/80 mr-1">#{cp.n}</span>
              <span className="text-xs text-muted-foreground mr-1">·</span>
              <span className="text-xs text-muted-foreground mr-1">{relativeTime(cp.created)}</span>
              <span className="text-xs text-muted-foreground">· {cp.summary}</span>
            </div>
            <div className="flex gap-1 shrink-0">
              <Button
                variant="outline"
                size="xs"
                onClick={() => handleDiff(cp.n)}
              >
                {t("checkpoints.diff")}
              </Button>
              <Button
                variant="outline-destructive"
                size="xs"
                onClick={() => handleRestoreClick(cp.n)}
                disabled={restore.isPending}
              >
                {t("checkpoints.rollback")}
              </Button>
            </div>
          </li>
        ))}
      </ul>
    );
  }

  return (
    <>
      <Sheet open={open} onOpenChange={onOpenChange}>
        <SheetContent side="right" className="flex flex-col p-0 gap-0">
          <SheetHeader className="px-4 pt-5 pb-3 border-b border-border">
            <SheetTitle>{t("checkpoints.title")}</SheetTitle>
          </SheetHeader>
          {body}
        </SheetContent>
      </Sheet>

      {/* Diff viewer */}
      <Dialog open={diffOpen} onOpenChange={setDiffOpen}>
        <DialogContent size="2xl">
          <DialogHeader>
            <DialogTitle>{t("checkpoints.diff_title", { n: diffN ?? "" })}</DialogTitle>
          </DialogHeader>
          {diffText == null ? (
            <p className="text-sm text-muted-foreground py-4 text-center">{t("checkpoints.loading")}</p>
          ) : (
            <pre className="overflow-auto max-h-[60vh] rounded bg-muted/30 p-3 text-xs font-mono whitespace-pre-wrap">
              {diffText}
            </pre>
          )}
        </DialogContent>
      </Dialog>

      {/* Restore confirm */}
      <ConfirmDialog
        open={confirmOpen}
        onClose={() => setConfirmOpen(false)}
        onConfirm={handleRestoreConfirm}
        title={t("checkpoints.rollback_confirm_title")}
        description={t("checkpoints.rollback_confirm_description", { n: restoreN ?? "" })}
        confirmLabel={t("checkpoints.rollback")}
        variant="destructive"
      />
    </>
  );
}
