"use client";

import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "@/hooks/use-translation";
import type { TranslationKey } from "@/i18n/types";
import { useProviders } from "@/lib/queries";
import {
  useUpdateProfile,
  PROFILE_CAPABILITIES,
  type ProfileBase,
  type ProfileCapability,
  type ProfileSlots,
  type SlotEntry,
} from "@/hooks/use-profiles";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  Dialog,
  DialogBody,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { ModelCombobox, ProviderSelect, VoiceSelect } from "@/components/provider-fields";
import { ArrowDown, ArrowUp, Plus, Trash2 } from "lucide-react";
import { toast } from "sonner";

const CAP_LABEL_KEY: Record<ProfileCapability, TranslationKey> = {
  text: "profiles.slot_text",
  compaction: "profiles.slot_compaction",
  stt: "profiles.slot_stt",
  tts: "profiles.slot_tts",
  vision: "profiles.slot_vision",
  imagegen: "profiles.slot_imagegen",
  websearch: "profiles.slot_websearch",
};

/** Provider `type` categories accepted for each capability's dropdown. `text`
 *  and `compaction` slots hold LLM providers, which may be registered under
 *  either the legacy `llm` type or the current `text` type. */
function categoriesFor(cap: ProfileCapability): string[] {
  return cap === "text" || cap === "compaction" ? ["text", "llm"] : [cap];
}

/** Whether this capability's rows carry a free-text `model` field. */
function hasModelField(cap: ProfileCapability): boolean {
  return cap === "text" || cap === "compaction" || cap === "vision";
}

export interface ProfileEditorProps {
  profile: ProfileBase;
  open: boolean;
  onClose: () => void;
}

export function ProfileEditor({ profile, open, onClose }: ProfileEditorProps) {
  const { t } = useTranslation();
  const { data: providers = [] } = useProviders();
  const updateProfile = useUpdateProfile();

  const [name, setName] = useState(profile.name);
  const [slots, setSlots] = useState<ProfileSlots>(profile.slots);
  const [saving, setSaving] = useState(false);

  // Re-seed local editable state whenever a different profile is opened.
  useEffect(() => {
    setName(profile.name);
    setSlots(profile.slots);
  }, [profile]);

  const rowsFor = useCallback((cap: ProfileCapability): SlotEntry[] => slots[cap] ?? [], [slots]);

  const providerIdByName = (name: string) =>
    providers.find((p) => p.name === name)?.id ?? null;

  const setRows = (cap: ProfileCapability, rows: SlotEntry[]) => {
    setSlots((prev) => ({ ...prev, [cap]: rows }));
  };

  const addReserve = (cap: ProfileCapability) => {
    setRows(cap, [...rowsFor(cap), { provider: "" }]);
  };

  const removeRow = (cap: ProfileCapability, idx: number) => {
    setRows(cap, rowsFor(cap).filter((_, i) => i !== idx));
  };

  const moveRow = (cap: ProfileCapability, idx: number, dir: -1 | 1) => {
    const rows = rowsFor(cap);
    const j = idx + dir;
    if (j < 0 || j >= rows.length) return;
    const next = rows.slice();
    [next[idx], next[j]] = [next[j], next[idx]];
    setRows(cap, next);
  };

  const updateRow = (cap: ProfileCapability, idx: number, patch: Partial<SlotEntry>) => {
    const rows = rowsFor(cap).slice();
    rows[idx] = { ...rows[idx], ...patch };
    setRows(cap, rows);
  };

  const handleSave = () => {
    setSaving(true);
    updateProfile.mutate(
      { id: profile.id, name, slots },
      {
        onSuccess: () => {
          setSaving(false);
          toast.success(t("profiles.saved"));
          onClose();
        },
        onError: () => setSaving(false),
      },
    );
  };

  return (
    <Dialog open={open} onOpenChange={(o) => { if (!o) onClose(); }}>
      <DialogContent size="2xl" layout="panel" className="max-h-[85dvh]">
        <DialogHeader className="p-6 pb-0">
          <DialogTitle>{profile.name}</DialogTitle>
        </DialogHeader>

        <DialogBody className="px-6 py-4 space-y-6">
          <div className="space-y-1.5">
            <label className="text-xs font-medium text-muted-foreground">
              {t("profiles.name_label")}
            </label>
            <Input
              value={name}
              disabled={profile.name === "Default"}
              onChange={(e) => setName(e.target.value)}
            />
          </div>

          {PROFILE_CAPABILITIES.map((cap) => {
            const rows = rowsFor(cap);
            return (
              <div key={cap} className="space-y-2 border-t border-border pt-4 first:border-t-0 first:pt-0" data-testid={`profile-slot-${cap}`}>
                <div className="flex items-center justify-between gap-2">
                  <h3 className="text-sm font-semibold text-foreground">{t(CAP_LABEL_KEY[cap])}</h3>
                  <Button variant="outline" size="sm" onClick={() => addReserve(cap)} className="gap-1.5">
                    <Plus className="h-3.5 w-3.5" /> {t("profiles.add_reserve")}
                  </Button>
                </div>

                {rows.length === 0 ? (
                  <p className="text-xs text-muted-foreground-subtle italic">{t("profiles.empty")}</p>
                ) : (
                  <div className="space-y-2">
                    {rows.map((row, idx) => (
                      <div
                        key={idx}
                        data-testid={`profile-row-${cap}-${idx}`}
                        className="flex flex-wrap items-center gap-2 rounded-md border border-border/50 bg-muted/10 p-2 min-w-0"
                      >
                        <ProviderSelect
                          value={row.provider}
                          categories={categoriesFor(cap)}
                          size="sm"
                          className="w-auto min-w-0 flex-1 basis-40"
                          onChange={(v) => {
                            // Провайдер сменился — прежние model/voice ему не принадлежат.
                            // Пустая model = default_model провайдера (семантика useAgentTextModel).
                            const patch: Partial<SlotEntry> = { provider: v };
                            if (hasModelField(cap)) patch.model = "";
                            if (cap === "tts") patch.voice = "";
                            updateRow(cap, idx, patch);
                          }}
                        />

                        {hasModelField(cap) && (
                          <ModelCombobox
                            value={row.model ?? ""}
                            onChange={(m) => updateRow(cap, idx, { model: m })}
                            providerId={providerIdByName(row.provider)}
                            disabled={!row.provider}
                            placeholder={row.provider ? t("profiles.model_default_placeholder") : t("fields.select_provider_first")}
                            className="min-w-0 flex-1 basis-40"
                            data-testid={`profile-model-${cap}-${idx}`}
                          />
                        )}

                        {cap === "tts" && (
                          <VoiceSelect
                            value={row.voice ?? ""}
                            onChange={(v) => updateRow(cap, idx, { voice: v })}
                            providerName={row.provider}
                            size="sm"
                            className="w-auto min-w-0 flex-1 basis-40"
                          />
                        )}

                        <div className="ml-auto flex items-center gap-1 shrink-0">
                          <Button
                            variant="ghost"
                            size="icon-sm"
                            disabled={idx === 0}
                            onClick={() => moveRow(cap, idx, -1)}
                            aria-label={t("profiles.move_up")}
                          >
                            <ArrowUp className="h-4 w-4" />
                          </Button>
                          <Button
                            variant="ghost"
                            size="icon-sm"
                            disabled={idx === rows.length - 1}
                            onClick={() => moveRow(cap, idx, 1)}
                            aria-label={t("profiles.move_down")}
                          >
                            <ArrowDown className="h-4 w-4" />
                          </Button>
                          <Button
                            variant="ghost"
                            size="icon-sm"
                            onClick={() => removeRow(cap, idx)}
                            aria-label={t("profiles.remove_row")}
                          >
                            <Trash2 className="h-4 w-4" />
                          </Button>
                        </div>
                      </div>
                    ))}
                  </div>
                )}
              </div>
            );
          })}
        </DialogBody>

        <DialogFooter className="p-6 pt-4 border-t border-border">
          <Button variant="outline" onClick={onClose}>{t("common.cancel")}</Button>
          <Button onClick={handleSave} disabled={saving}>{t("common.save")}</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
