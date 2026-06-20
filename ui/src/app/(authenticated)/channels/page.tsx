"use client";

import { useState, useCallback } from "react";
import { apiPost, apiDelete, apiPut, apiGet } from "@/lib/api";
import { useChannels, useActiveChannels } from "@/lib/queries";
import { useQueryClient } from "@tanstack/react-query";
import { useTranslation } from "@/hooks/use-translation";
import { ErrorBanner } from "@/components/ui/error-banner";
import { useAuthStore } from "@/stores/auth-store";
import { useWsSubscription } from "@/hooks/use-ws-subscription";
import type { ChannelRow } from "@/types/api";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogFooter,
} from "@/components/ui/dialog";
import { ConfirmDialog } from "@/components/ui/confirm-dialog";
import { Field } from "@/components/ui/field";
import { Skeleton } from "@/components/ui/skeleton";
import { EmptyState } from "@/components/ui/empty-state";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  Radio,
  Plus,
  Trash2,
  RotateCcw,
  Pencil,
  Wifi,
  WifiOff,
  Loader2,
  Bot,
  Send,
  Gamepad2,
  Link,
  Hash,
  MessageSquare,
  Phone,
  type LucideProps,
} from "lucide-react";
import { toast } from "sonner";

const CHANNEL_TYPES = ["telegram", "discord", "matrix", "irc", "slack", "whatsapp"];

const CHANNEL_ICONS: Record<string, React.FC<LucideProps>> = {
  telegram: Send,
  discord: Gamepad2,
  matrix: Link,
  irc: Hash,
  slack: MessageSquare,
  whatsapp: Phone,
};

interface ConfigField {
  key: string;
  labelKey: string;
  placeholder: string;
  type?: "text" | "password";
  required?: boolean;
}

const CHANNEL_CONFIG_FIELDS: Record<string, ConfigField[]> = {
  telegram: [
    { key: "bot_token", labelKey: "channels.field_bot_token", placeholder: "", type: "password", required: true },
    { key: "api_url", labelKey: "channels.field_api_url", placeholder: "https://api.telegram.org" },
  ],
  discord: [
    { key: "bot_token", labelKey: "channels.field_bot_token", placeholder: "", type: "password", required: true },
    { key: "guild_id", labelKey: "channels.field_guild_id", placeholder: "123456789012345678" },
  ],
  matrix: [
    { key: "homeserver_url", labelKey: "channels.field_homeserver", placeholder: "https://matrix.org", required: true },
    { key: "access_token", labelKey: "channels.field_access_token", placeholder: "", type: "password", required: true },
    { key: "room_id", labelKey: "channels.field_room_id", placeholder: "!roomid:matrix.org" },
  ],
  irc: [
    { key: "server", labelKey: "channels.field_server", placeholder: "irc.libera.chat:6697", required: true },
    { key: "channel", labelKey: "channels.field_irc_channel", placeholder: "#mychannel" },
    { key: "nickname", labelKey: "channels.field_nickname", placeholder: "mybot" },
    { key: "password", labelKey: "channels.field_password", placeholder: "", type: "password" },
  ],
  slack: [
    { key: "bot_token", labelKey: "channels.field_bot_token", placeholder: "xoxb-...", type: "password", required: true },
    { key: "app_token", labelKey: "channels.field_app_token", placeholder: "xapp-...", type: "password", required: true },
  ],
  whatsapp: [
    { key: "phone_number_id", labelKey: "channels.field_phone_id", placeholder: "123456789012345", required: true },
    { key: "access_token", labelKey: "channels.field_access_token", placeholder: "", type: "password", required: true },
    { key: "verify_token", labelKey: "channels.field_verify_token", placeholder: "" },
  ],
};

export default function ChannelsPage() {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const agents = useAuthStore((s) => s.agents);

  const { data: channels = [], isLoading: loading, error } = useChannels();
  const { data: active = [] } = useActiveChannels();

  const [dialogOpen, setDialogOpen] = useState(false);
  const [editingChannel, setEditingChannel] = useState<ChannelRow | null>(null);
  const [deleteTarget, setDeleteTarget] = useState<ChannelRow | null>(null);

  // Form state
  const [formAgent, setFormAgent] = useState("");
  const [formType, setFormType] = useState("telegram");
  const [formName, setFormName] = useState("");
  const [formConfig, setFormConfig] = useState<Record<string, string>>({});
  const [saving, setSaving] = useState(false);

  useWsSubscription("channels_changed", useCallback(() => {
    queryClient.invalidateQueries({ queryKey: ["channels"] });
  }, [queryClient]));

  const isOnline = (ch: ChannelRow) =>
    active.some((a) => a.channel_id === ch.id);

  const openCreate = () => {
    setEditingChannel(null);
    setFormAgent(agents[0] || "");
    setFormType("telegram");
    setFormName("");
    setFormConfig({});
    setDialogOpen(true);
  };

  const openEdit = (ch: ChannelRow) => {
    setEditingChannel(ch);
    setFormAgent(ch.agent_name);
    setFormType(ch.channel_type);
    setFormName(ch.display_name);
    // Pre-fill non-secret fields from config; secrets stay empty (user re-enters if needed)
    const cfg: Record<string, string> = {};
    const fields = CHANNEL_CONFIG_FIELDS[ch.channel_type] || [];
    for (const f of fields) {
      if (f.type !== "password") {
        const val = (ch.config as Record<string, string>)[f.key];
        if (val) cfg[f.key] = val;
      }
    }
    setFormConfig(cfg);
    setDialogOpen(true);
  };

  const handleSave = async () => {
    if (!formAgent || !formName.trim()) return;
    setSaving(true);
    try {
      // Only include non-empty values in config
      const config: Record<string, string> = {};
      for (const [k, v] of Object.entries(formConfig)) {
        if (v.trim()) config[k] = v;
      }

      if (editingChannel) {
        // Merge non-credential fields from original config, then overlay form values.
        // Credential fields from origConfig are ALWAYS masked ("5092...xxx") so we must
        // exclude them — only send credentials the user explicitly typed in the form.
        const credentialKeys = ["bot_token", "access_token", "password", "app_token", "verify_token"];
        const origConfig = (editingChannel.config as Record<string, string>) || {};
        const safeOrig: Record<string, string> = {};
        for (const [k, v] of Object.entries(origConfig)) {
          if (!credentialKeys.includes(k)) safeOrig[k] = v;
        }
        const mergedConfig = { ...safeOrig, ...config };

        if (formAgent !== editingChannel.agent_name) {
          // Copy access policy from source agent to target if target has none.
          // Prevents accidental access downgrade when moving a channel.
          try {
            const [srcAgent, dstAgent] = await Promise.all([
              apiGet<{ access?: { mode: string; owner_id?: string } }>(`/api/agents/${editingChannel.agent_name}`),
              apiGet<{ access?: { mode: string; owner_id?: string } }>(`/api/agents/${formAgent}`),
            ]);
            if (srcAgent.access?.mode === "restricted" && !dstAgent.access) {
              await apiPut(`/api/agents/${formAgent}`, { access: srcAgent.access });
            }
          } catch {
            // Best-effort — don't block channel move if agent fetch fails
          }

          // Agent changed — create new, then delete old (safer: if create fails, old stays)
          try {
            await apiPost(`/api/agents/${formAgent}/channels`, {
              channel_type: editingChannel.channel_type,
              display_name: formName,
              config: mergedConfig,
            });
          } catch (createErr) {
            toast.error(`${t("channels.create_failed")}: ${createErr}`);
            return;
          }
          try {
            await apiDelete(`/api/agents/${editingChannel.agent_name}/channels/${editingChannel.id}`);
          } catch (deleteErr) {
            // New channel created but old not deleted — warn user
            toast.warning(`${t("channels.old_delete_failed")}: ${deleteErr}`);
          }
          toast.success(t("channels.updated"));
        } else {
          await apiPut(`/api/agents/${formAgent}/channels/${editingChannel.id}`, {
            display_name: formName,
            config: mergedConfig,
          });
          toast.success(t("channels.updated"));
        }
      } else {
        await apiPost(`/api/agents/${formAgent}/channels`, {
          channel_type: formType,
          display_name: formName,
          config,
        });
        toast.success(t("channels.created"));
      }
      setDialogOpen(false);
      queryClient.invalidateQueries({ queryKey: ["channels"] });
    } catch (e) {
      toast.error(`${e}`);
    } finally {
      setSaving(false);
    }
  };

  const confirmDelete = async () => {
    if (!deleteTarget) return;
    try {
      await apiDelete(`/api/agents/${deleteTarget.agent_name}/channels/${deleteTarget.id}`);
      toast.success(t("channels.deleted"));
      setDeleteTarget(null);
      queryClient.invalidateQueries({ queryKey: ["channels"] });
    } catch (e) {
      toast.error(`${e}`);
    }
  };

  const handleRestart = async (ch: ChannelRow) => {
    try {
      await apiPost(`/api/agents/${ch.agent_name}/channels/${ch.id}/restart`, {});
      toast.success(t("channels.restarting"));
      queryClient.invalidateQueries({ queryKey: ["channels"] });
    } catch (e) {
      toast.error(`${e}`);
    }
  };

  const grouped = channels.reduce<Record<string, ChannelRow[]>>((acc, ch) => {
    (acc[ch.agent_name] ||= []).push(ch);
    return acc;
  }, {});

  return (
    <div className="flex-1 overflow-y-auto p-4 md:p-6 lg:p-8 selection:bg-primary/20">
        <div className="mb-8">
          <div className="flex flex-col md:flex-row md:items-center justify-between gap-4">
            <div className="flex flex-col gap-1">
              <h2 className="font-display text-lg font-bold tracking-tight text-foreground">
                {t("channels.title")}
              </h2>
              <span className="text-sm text-muted-foreground">{t("channels.subtitle")}</span>
            </div>
            <Button size="sm" onClick={openCreate} className="gap-1.5 w-full md:w-auto">
              <Plus className="h-3.5 w-3.5" />
              {t("channels.add")}
            </Button>
          </div>
        </div>

        {error && <ErrorBanner error={`${error}`} />}

        {loading ? (
          <div className="space-y-4">
            {[1, 2, 3].map((i) => (
              <Skeleton key={i} className="h-20 rounded-xl" />
            ))}
          </div>
        ) : channels.length === 0 ? (
          <EmptyState icon={Radio} text={t("channels.empty")} hint={<p className="text-xs mt-1 opacity-60">{t("channels.empty_hint")}</p>} height="h-48" />
        ) : (
          <div className="space-y-6">
            {Object.entries(grouped).map(([agentName, agentChannels]) => (
              <div key={agentName} className="neu-flat p-5 md:p-6">
                <div className="flex items-center gap-2 mb-4 pb-3 border-b border-border/50">
                  <Bot className="h-4 w-4 text-primary" />
                  <span className="text-sm font-semibold text-foreground">{agentName}</span>
                  <Badge variant="outline" className="text-[10px] ml-auto">
                    {agentChannels.length}
                  </Badge>
                </div>
                <div className="space-y-3">
                  {agentChannels.map((ch) => {
                    const online = isOnline(ch);
                    return (
                      <div
                        key={ch.id}
                        className="flex items-center gap-3 rounded-lg border border-border/50 p-3 hover:bg-muted/30 transition-colors overflow-hidden"
                      >
                        {(() => {
                          const Icon = CHANNEL_ICONS[ch.channel_type] || Radio;
                          return <Icon className="h-5 w-5 text-muted-foreground/70 shrink-0" />;
                        })()}
                        <div className="flex-1 min-w-0">
                          <div className="flex items-center gap-2">
                            <span className="text-sm font-medium text-foreground truncate">
                              {ch.display_name}
                            </span>
                            <Badge
                              variant="outline"
                              className={`text-[9px] ${
                                online
                                  ? "bg-success/15 text-success border-success/30"
                                  : ch.status === "error"
                                    ? "bg-destructive/15 text-destructive border-destructive/30"
                                    : "bg-muted text-muted-foreground border-border"
                              }`}
                            >
                              {online ? (
                                <><Wifi className="h-2.5 w-2.5 mr-1" />{t("channels.online")}</>
                              ) : ch.status === "error" ? (
                                t("channels.status_error")
                              ) : (
                                <><WifiOff className="h-2.5 w-2.5 mr-1" />{t("channels.offline")}</>
                              )}
                            </Badge>
                          </div>
                          <div className="flex items-center gap-2 mt-0.5">
                            <span className="text-[10px] font-mono text-muted-foreground/50">
                              {ch.channel_type}
                            </span>
                            <span className="text-[10px] font-mono text-muted-foreground/30">
                              {ch.id.slice(0, 8)}
                            </span>
                            {ch.error_msg && (
                              <span className="text-[10px] text-destructive truncate max-w-xs">
                                {ch.error_msg}
                              </span>
                            )}
                          </div>
                        </div>
                        <div className="flex items-center gap-1 shrink-0">
                          <Button size="icon-sm" variant="ghost" onClick={() => openEdit(ch)} aria-label={t("common.edit")}>
                            <Pencil className="h-3.5 w-3.5" />
                          </Button>
                          <Button size="icon-sm" variant="ghost" onClick={() => handleRestart(ch)} aria-label={t("channels.restart")}>
                            <RotateCcw className="h-3.5 w-3.5" />
                          </Button>
                          <Button size="icon-sm" variant="ghost" className="text-destructive hover:bg-destructive/10" onClick={() => setDeleteTarget(ch)} aria-label={t("common.delete")}>
                            <Trash2 className="h-3.5 w-3.5" />
                          </Button>
                        </div>
                      </div>
                    );
                  })}
                </div>
              </div>
            ))}
          </div>
        )}

        {/* Create/Edit Dialog */}
        <Dialog open={dialogOpen} onOpenChange={setDialogOpen}>
          <DialogContent className="rounded-xl border-border bg-card max-w-[95vw] sm:max-w-lg max-h-[90vh] overflow-y-auto">
            <DialogHeader>
              <DialogTitle>
                {editingChannel ? t("channels.edit") : t("channels.create")}
              </DialogTitle>
            </DialogHeader>
            <div className="space-y-4 py-2">
              <Field label={t("channels.agent")} labelClassName="text-xs">
                <Select value={formAgent} onValueChange={setFormAgent}>
                  <SelectTrigger><SelectValue /></SelectTrigger>
                  <SelectContent>
                    {agents.map((a) => (
                      <SelectItem key={a} value={a}>{a}</SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </Field>
              {!editingChannel && (
                <Field label={t("channels.type")} labelClassName="text-xs">
                  <Select value={formType} onValueChange={setFormType}>
                    <SelectTrigger><SelectValue /></SelectTrigger>
                    <SelectContent>
                      {CHANNEL_TYPES.map((ct) => {
                        const CIcon = CHANNEL_ICONS[ct] || Radio;
                        return (
                          <SelectItem key={ct} value={ct}>
                            <span className="flex items-center gap-2">
                              <CIcon className="h-3.5 w-3.5" />
                              {ct}
                            </span>
                          </SelectItem>
                        );
                      })}
                    </SelectContent>
                  </Select>
                </Field>
              )}
              <Field label={t("channels.display_name")} labelClassName="text-xs">
                <Input
                  value={formName}
                  onChange={(e) => setFormName(e.target.value)}
                  placeholder={t("channels.placeholder_name")}
                  className="font-mono text-sm"
                />
              </Field>
              {/* Dynamic config fields based on channel type */}
              {(CHANNEL_CONFIG_FIELDS[formType] || []).map((field) => (
                <Field
                  key={field.key}
                  label={`${t(field.labelKey as Parameters<typeof t>[0])}${field.required ? " *" : ""}`}
                  labelClassName="text-xs"
                >
                  <Input
                    type={field.type || "text"}
                    value={formConfig[field.key] || ""}
                    onChange={(e) => setFormConfig({ ...formConfig, [field.key]: e.target.value })}
                    placeholder={editingChannel && field.type === "password" ? t("channels.bot_token_keep") : field.placeholder}
                    className="font-mono text-sm"
                  />
                </Field>
              ))}
              {(formType === "telegram" || formType === "discord") && (
                <Field label={t("channels.typing_mode")} hint={t("channels.typing_hint")} labelClassName="text-xs">
                  <Select value={formConfig["typing_mode"] || "instant"} onValueChange={(v) => setFormConfig({ ...formConfig, typing_mode: v })}>
                    <SelectTrigger className="font-mono text-sm">
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      <SelectItem value="instant">{t("channels.typing_instant")}</SelectItem>
                      <SelectItem value="thinking">{t("channels.typing_thinking")}</SelectItem>
                      <SelectItem value="message">{t("channels.typing_message")}</SelectItem>
                      <SelectItem value="never">{t("channels.typing_never")}</SelectItem>
                    </SelectContent>
                  </Select>
                </Field>
              )}
            </div>
            <DialogFooter>
              <Button variant="ghost" onClick={() => setDialogOpen(false)}>
                {t("common.cancel")}
              </Button>
              <Button onClick={handleSave} disabled={saving || !formName.trim()}>
                {saving && <Loader2 className="h-3.5 w-3.5 mr-1.5 animate-spin" />}
                {t("common.save")}
              </Button>
            </DialogFooter>
          </DialogContent>
        </Dialog>

        {/* Delete Confirmation */}
        <ConfirmDialog
          open={!!deleteTarget}
          onClose={() => setDeleteTarget(null)}
          onConfirm={confirmDelete}
          title={t("channels.delete_title")}
          description={t("channels.delete_confirm", { name: deleteTarget?.display_name ?? "" })}
        />
    </div>
  );
}
