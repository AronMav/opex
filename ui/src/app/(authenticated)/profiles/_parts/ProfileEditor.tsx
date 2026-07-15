"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "@/hooks/use-translation";
import type { TranslationKey } from "@/i18n/types";
import { apiGet } from "@/lib/api";
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
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  Dialog,
  DialogBody,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
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

interface TtsVoice {
  id: string;
  name: string;
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
  const [voicesByProvider, setVoicesByProvider] = useState<Record<string, TtsVoice[]>>({});
  const [saving, setSaving] = useState(false);

  // Re-seed local editable state whenever a different profile is opened.
  useEffect(() => {
    setName(profile.name);
    setSlots(profile.slots);
    setVoicesByProvider({});
  }, [profile]);

  // Cancellation guard for fetchVoices below: a per-provider sequence number
  // so an in-flight (out-of-order) response can't clobber a later one, plus
  // an unmount flag so no state-set fires after the dialog closes/unmounts.
  const voiceFetchSeq = useRef<Record<string, number>>({});
  const unmountedRef = useRef(false);
  useEffect(() => {
    unmountedRef.current = false;
    return () => { unmountedRef.current = true; };
  }, []);

  const rowsFor = useCallback((cap: ProfileCapability): SlotEntry[] => slots[cap] ?? [], [slots]);

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

  const fetchVoices = useCallback((providerName: string) => {
    if (!providerName) return;
    const seq = (voiceFetchSeq.current[providerName] ?? 0) + 1;
    voiceFetchSeq.current[providerName] = seq;
    const isStale = () => unmountedRef.current || voiceFetchSeq.current[providerName] !== seq;
    apiGet<{ voices: TtsVoice[] }>(`/api/tts/voices?provider=${encodeURIComponent(providerName)}`)
      .then((data) => {
        if (isStale()) return;
        setVoicesByProvider((prev) => ({ ...prev, [providerName]: data.voices ?? [] }));
      })
      .catch(() => {
        if (isStale()) return;
        setVoicesByProvider((prev) => ({ ...prev, [providerName]: [] }));
      });
  }, []);

  const providersFor = (cap: ProfileCapability) => {
    const cats = categoriesFor(cap);
    return providers.filter((p) => cats.includes(p.type));
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
            const options = providersFor(cap);
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
                        <Select
                          value={row.provider}
                          onValueChange={(v) => {
                            // Reset the voice on provider change — a voice id from the
                            // previous provider may not exist (or mean something else)
                            // on the new one, so don't carry it over silently.
                            updateRow(cap, idx, cap === "tts" ? { provider: v, voice: "" } : { provider: v });
                            if (cap === "tts") fetchVoices(v);
                          }}
                        >
                          <SelectTrigger size="sm" className="w-40">
                            <SelectValue placeholder={t("profiles.provider_placeholder")} />
                          </SelectTrigger>
                          <SelectContent>
                            {options.map((p) => (
                              <SelectItem key={p.name} value={p.name}>{p.name}</SelectItem>
                            ))}
                          </SelectContent>
                        </Select>

                        {hasModelField(cap) && (
                          <Input
                            value={row.model ?? ""}
                            placeholder={t("profiles.model_placeholder")}
                            onChange={(e) => updateRow(cap, idx, { model: e.target.value })}
                            className="w-40"
                            data-testid={`profile-model-${cap}-${idx}`}
                          />
                        )}

                        {cap === "tts" && (
                          <Select
                            value={row.voice ?? ""}
                            onValueChange={(v) => updateRow(cap, idx, { voice: v })}
                          >
                            <SelectTrigger size="sm" className="w-40">
                              <SelectValue placeholder={t("profiles.voice_placeholder")} />
                            </SelectTrigger>
                            <SelectContent>
                              {(voicesByProvider[row.provider] ?? []).map((v) => (
                                <SelectItem key={v.id} value={v.id}>{v.name}</SelectItem>
                              ))}
                            </SelectContent>
                          </Select>
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
