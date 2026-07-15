"use client";

import { useState } from "react";
import { useTranslation } from "@/hooks/use-translation";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import { PageHeader } from "@/components/ui/page-header";
import { PageContainer } from "@/components/ui/page-container";
import { ErrorBanner } from "@/components/ui/error-banner";
import { Skeleton } from "@/components/ui/skeleton";
import { EmptyState } from "@/components/ui/empty-state";
import { ConfirmDialog } from "@/components/ui/confirm-dialog";
import { Plus, Copy, Trash2, Layers } from "lucide-react";
import {
  useProfiles,
  useCreateProfile,
  useCopyProfile,
  useDeleteProfile,
  PROFILE_CAPABILITIES,
  type ProfileRow,
  type ProfileCapability,
  type SlotEntry,
} from "@/hooks/use-profiles";
import { ProfileEditor } from "./_parts/ProfileEditor";

/** One human-readable summary line per non-empty capability slot, e.g.
 *  "tts: minimax (clone:Arty) +1". Capability is rendered verbatim (not
 *  translated) — it's a compact technical label, not user-facing prose. */
function slotSummary(cap: ProfileCapability, chain: SlotEntry[] | undefined): string | null {
  if (!chain || chain.length === 0) return null;
  const first = chain[0];
  const voice = first.voice ? ` (${first.voice})` : "";
  const extra = chain.length > 1 ? ` +${chain.length - 1}` : "";
  return `${cap}: ${first.provider}${voice}${extra}`;
}

export default function ProfilesPage() {
  const { t } = useTranslation();
  const { data, isLoading, error } = useProfiles();
  const profiles = data?.profiles ?? [];

  const createProfile = useCreateProfile();
  const copyProfile = useCopyProfile();
  const deleteProfile = useDeleteProfile();

  const [editingId, setEditingId] = useState<string | null>(null);
  const [deleteTarget, setDeleteTarget] = useState<ProfileRow | null>(null);

  const editingProfile = profiles.find((p) => p.id === editingId) ?? null;

  const handleCreate = () => {
    const name = window.prompt(t("profiles.create"));
    if (!name || !name.trim()) return;
    createProfile.mutate(
      { name: name.trim(), slots: {} },
      { onSuccess: (created) => setEditingId(created.id) },
    );
  };

  const confirmDelete = () => {
    if (!deleteTarget) return;
    const target = deleteTarget;
    setDeleteTarget(null);
    deleteProfile.mutate(target.id);
  };

  return (
    <PageContainer className="flex flex-col gap-8 min-w-0">
      <PageHeader
        title={t("profiles.title")}
        description={t("profiles.subtitle")}
        actions={
          <Button size="lg" onClick={handleCreate} className="w-full md:w-auto gap-2">
            <Plus className="h-4 w-4" /> {t("profiles.create")}
          </Button>
        }
      />

      {error && <ErrorBanner error={String(error)} />}

      {isLoading ? (
        <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-4">
          {[1, 2, 3].map((i) => (
            <Skeleton key={i} className="h-48 rounded-xl" />
          ))}
        </div>
      ) : profiles.length === 0 ? (
        <EmptyState icon={Layers} text={t("profiles.empty")} height="h-48" />
      ) : (
        <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-4">
          {profiles.map((p) => {
            const isDefault = p.name === "Default";
            const inUse = p.agents.length > 0;
            const deleteTitle = isDefault
              ? t("profiles.default_hint")
              : inUse
                ? t("profiles.in_use_hint")
                : undefined;
            return (
              <Card
                key={p.id}
                interactive
                className="flex flex-col gap-3 p-5 min-w-0 overflow-hidden cursor-pointer"
                onClick={() => setEditingId(p.id)}
              >
                <span className="font-bold text-sm text-foreground truncate min-w-0" title={p.name}>
                  {p.name}
                </span>

                <div className="space-y-1 text-xs text-muted-foreground min-w-0">
                  {PROFILE_CAPABILITIES.map((cap) => {
                    const line = slotSummary(cap, p.slots[cap]);
                    if (!line) return null;
                    return (
                      <div key={cap} className="truncate font-mono" title={line}>
                        {line}
                      </div>
                    );
                  })}
                </div>

                {inUse && (
                  <div className="flex flex-wrap gap-1 mt-auto pt-2 min-w-0">
                    {p.agents.map((a) => (
                      <Badge key={a} variant="outline" size="sm">{a}</Badge>
                    ))}
                  </div>
                )}

                <div className="flex gap-1.5 pt-1" onClick={(e) => e.stopPropagation()}>
                  <Button
                    variant="outline"
                    size="sm"
                    className="flex-1 gap-1.5"
                    onClick={() => copyProfile.mutate(p.id)}
                  >
                    <Copy className="h-3.5 w-3.5" /> {t("profiles.copy")}
                  </Button>
                  <Button
                    variant="outline-destructive"
                    size="sm"
                    disabled={isDefault || inUse}
                    title={deleteTitle}
                    aria-label={t("profiles.delete")}
                    onClick={() => setDeleteTarget(p)}
                  >
                    <Trash2 className="h-3.5 w-3.5" />
                  </Button>
                </div>
              </Card>
            );
          })}
        </div>
      )}

      {editingProfile && (
        <ProfileEditor
          profile={editingProfile}
          open
          onClose={() => setEditingId(null)}
        />
      )}

      <ConfirmDialog
        open={!!deleteTarget}
        onClose={() => setDeleteTarget(null)}
        onConfirm={confirmDelete}
        title={t("profiles.delete_title")}
        description={deleteTarget?.name ?? ""}
      />
    </PageContainer>
  );
}
