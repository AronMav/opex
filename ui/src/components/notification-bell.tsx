"use client";

import { useState, useEffect, useRef } from "react";
import type { UIEvent } from "react";
import { useRouter } from "next/navigation";
import { Bell, Loader2, Settings } from "lucide-react";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { useNotificationStore } from "@/stores/notification-store";
import {
  useNotifications,
  useNotificationWsSync,
  useNotificationRecovery,
  useMarkNotificationRead,
  useMarkAllRead,
  useClearAllNotifications,
  useLoadOlderNotifications,
  useNotificationPrefs,
  useUpdateNotificationPref,
} from "@/lib/queries";
import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/switch";
import { useTranslation } from "@/hooks/use-translation";
import { NotificationInfraBody } from "./notification-infra-body";
import type { NotificationRow } from "@/types/api";
import type { TranslationKey } from "@/i18n/types";

// ── Sound helper ─────────────────────────────────────────────────────────────

function playNotificationSound() {
  try {
    const ctx = new AudioContext();
    const osc = ctx.createOscillator();
    const gain = ctx.createGain();
    osc.connect(gain);
    gain.connect(ctx.destination);
    osc.type = "sine";
    osc.frequency.setValueAtTime(880, ctx.currentTime);
    gain.gain.setValueAtTime(0.15, ctx.currentTime);
    gain.gain.exponentialRampToValueAtTime(0.001, ctx.currentTime + 0.25);
    osc.start(ctx.currentTime);
    osc.stop(ctx.currentTime + 0.25);
    osc.onended = () => ctx.close().catch(() => {});
  } catch {
    // AudioContext not available (SSR or blocked by browser policy — silent fail)
  }
}

// ── Media notification body ───────────────────────────────────────────────────

interface MediaNotificationBodyProps {
  notification: NotificationRow;
}

// Error-flavoured event types emitted by `media_background.rs`. The
// corresponding `*_ready` events are no longer sent by the backend — the
// bell only ever needs to render the error path inline; success is
// communicated some other way (e.g. the channel action itself).
const ERROR_EVENTS = new Set([
  "tts_error",
  "image_error",
  "video_error",
  "media_error",
]);

export function MediaNotificationBody({ notification }: MediaNotificationBodyProps) {
  const { type, body } = notification;

  // Error path for any media-flavoured event.
  if (ERROR_EVENTS.has(type)) {
    return (
      <span className="text-xs text-destructive line-clamp-2">{body}</span>
    );
  }

  // Default body line for everything else.
  return <span className="text-xs text-muted-foreground line-clamp-2">{body}</span>;
}

// Used by `getNotificationRoute` to decide that a notification is rendered
// inline (no navigation) rather than linking somewhere.
function isMediaEvent(type: string): boolean {
  return ERROR_EVENTS.has(type);
}

// The user-facing alerting types exposed in the prefs panel. The backend mute
// works for ANY type; this is the curated subset worth toggling.
const PREF_TYPES: { type: string; labelKey: TranslationKey }[] = [
  { type: "agent_error", labelKey: "notifications.type.agent_error" },
  { type: "tool_approval", labelKey: "notifications.type.tool_approval" },
  { type: "watchdog_alert", labelKey: "notifications.type.watchdog_alert" },
  { type: "access_request", labelKey: "notifications.type.access_request" },
  { type: "infra_decision", labelKey: "notifications.type.infra_decision" },
  { type: "initiative_proposal", labelKey: "notifications.type.initiative_proposal" },
];

// ── Notification type → target route ─────────────────────────────────────────

// Exported so unit tests can verify routing decisions for every notification
// type without rendering the bell. Keeping it free-standing also documents
// the routing contract in one place.
//
// `data` is optional (existing callers/tests pass only `type`) — it's needed
// for "initiative_proposal", whose target route is agent-scoped. The plan
// page lives at `/agents/plan/?agent=` (a `?agent=` query param, NOT an
// `/agents/{name}/plan` dynamic segment) because the UI is a static export
// (`output: "export"` in next.config.ts) — a `[name]` route can't be
// pre-rendered for a runtime-configurable, open-ended agent set. Same
// query-param-as-router pattern as the `/monitor/?tab=` tabs. Stage C
// initiative — see `agent/initiative/tick.rs`'s
// `notify(..., "initiative_proposal", ..., {agent, proposal_id, text, rationale})`.
export function getNotificationRoute(type: string, data?: Record<string, unknown>): string | null {
  // Media error events (tts/image/video/media) render inline; no nav.
  if (isMediaEvent(type)) return null;
  switch (type) {
    case "infra_decision":      return null; // actionable buttons, not navigation
    case "access_request":      return "/access";
    case "tool_approval":       return "/monitor/?tab=approvals";
    case "agent_error":         return "/monitor/?tab=logs";
    case "watchdog_alert":      return "/monitor/?tab=watchdog";
    case "initiative_proposal": {
      const agent = typeof data?.agent === "string" ? data.agent : null;
      return agent ? `/agents/plan/?agent=${encodeURIComponent(agent)}` : "/monitor/";
    }
    default:                    return "/monitor/";
  }
}

// ── NotificationBell ─────────────────────────────────────────────────────────

export function NotificationBell() {
  const { t } = useTranslation();
  const router = useRouter();
  const notifications = useNotificationStore((s) => s.notifications);
  const unread_count = useNotificationStore((s) => s.unread_count);
  const newArrivalSeq = useNotificationStore((s) => s.newArrivalSeq);

  const [flashing, setFlashing] = useState(false);
  const prevSeqRef = useRef(newArrivalSeq);

  // Fetch initial notifications, wire WS real-time updates + reconnect recovery
  useNotifications();
  useNotificationWsSync();
  useNotificationRecovery();

  const markRead = useMarkNotificationRead();
  const markAllRead = useMarkAllRead();
  const clearAll = useClearAllNotifications();

  const { loadOlder, isLoading: loadingOlder, hasMore } = useLoadOlderNotifications();

  const prefs = useNotificationStore((s) => s.prefs);
  const [showPrefs, setShowPrefs] = useState(false);
  useNotificationPrefs();
  const updatePref = useUpdateNotificationPref();

  const onListScroll = (e: UIEvent<HTMLDivElement>) => {
    const el = e.currentTarget;
    // within 48px of the bottom → pull the next older page
    if (el.scrollHeight - el.scrollTop - el.clientHeight < 48 && hasMore && !loadingOlder) {
      void loadOlder();
    }
  };

  // Flash + sound ONLY on a genuine live arrival (newArrivalSeq bump).
  // Refetch-on-reconnect and the initial cold-load fetch do not bump the seq,
  // so they never beep.
  useEffect(() => {
    if (newArrivalSeq > prevSeqRef.current) {
      setFlashing(true);
      playNotificationSound();
      const timer = setTimeout(() => setFlashing(false), 1500);
      prevSeqRef.current = newArrivalSeq;
      return () => clearTimeout(timer);
    }
    prevSeqRef.current = newArrivalSeq;
  }, [newArrivalSeq]);

  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <button
          className="group relative flex h-8 w-8 items-center justify-center rounded-md transition-colors hover:bg-accent text-muted-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background"
          aria-label={t("notifications.bell_label")}
          data-testid="notifications-bell"
        >
          <Bell
            size={20}
            className={`transition-all duration-150 ${
              flashing ? "text-primary scale-125" : "text-muted-foreground"
            }`}
          />
          {unread_count > 0 && (
            <span className="absolute -right-0.5 -top-0.5 flex h-4 min-w-4 items-center justify-center rounded-full bg-primary px-1 text-3xs font-bold text-primary-foreground leading-none">
              {unread_count > 99 ? "99+" : unread_count}
            </span>
          )}
        </button>
      </DropdownMenuTrigger>
      <DropdownMenuContent       align="end"
      className="w-80 max-w-[calc(100dvw-0.5rem)] p-0" data-testid="notification-list">
        {/* Header — title on its own line; the action links sit on a full-width
            row below it with "mark all read" pinned left and "clear all" pushed
            right (ml-auto keeps clear-all right even when it's the only one). */}
        <div className="flex flex-wrap items-center gap-x-3 gap-y-1 border-b border-border px-4 py-3">
          <span className="text-sm font-semibold">{t("notifications.title")}</span>
          <button
            type="button"
            aria-label={t("notifications.settings")}
            className="ml-auto flex h-6 w-6 items-center justify-center rounded text-muted-foreground hover:bg-accent"
            onClick={(e) => {
              e.preventDefault();
              setShowPrefs((v) => !v);
            }}
          >
            <Settings size={15} className={showPrefs ? "text-primary" : ""} />
          </button>
          <div className="flex w-full items-center gap-3">
            {unread_count > 0 && (
              <Button
                variant="link"
                size="xs"
                className="h-auto whitespace-nowrap px-0"
                onClick={() => markAllRead.mutate()}
              >
                {t("notifications.mark_all_read")}
              </Button>
            )}
            {notifications.length > 0 && (
              <Button
                variant="link"
                size="xs"
                className="ml-auto h-auto whitespace-nowrap px-0 text-destructive"
                onClick={() => clearAll.mutate()}
              >
                {t("notifications.clear_all")}
              </Button>
            )}
          </div>
        </div>
        {showPrefs ? (
          <div className="max-h-[min(24rem,calc(100dvh-8rem))] overflow-y-auto overscroll-contain p-2">
            {PREF_TYPES.map(({ type, labelKey }) => {
              const p = prefs[type] ?? { muted: false, sound: true };
              return (
                <div key={type} className="flex items-center gap-3 px-2 py-2">
                  <span className="flex-1 truncate text-sm">{t(labelKey)}</span>
                  <label className="flex items-center gap-1 text-2xs text-muted-foreground">
                    {t("notifications.mute")}
                    <Switch
                      size="sm"
                      checked={p.muted}
                      onCheckedChange={(muted) =>
                        updatePref.mutate({ type, muted, sound: p.sound })
                      }
                    />
                  </label>
                  <label className="flex items-center gap-1 text-2xs text-muted-foreground">
                    {t("notifications.sound")}
                    <Switch
                      size="sm"
                      checked={p.sound}
                      disabled={p.muted}
                      onCheckedChange={(sound) =>
                        updatePref.mutate({ type, muted: p.muted, sound })
                      }
                    />
                  </label>
                </div>
              );
            })}
          </div>
        ) : (
          /* List */
          <div
            className="max-h-[min(24rem,calc(100dvh-8rem))] overflow-y-auto overscroll-contain"
            onScroll={onListScroll}
          >
            {notifications.length === 0 ? (
              <div className="flex items-center justify-center py-8 text-sm text-muted-foreground">
                {t("notifications.empty")}
              </div>
            ) : (
              <>
                {notifications.map((n) => (
                  <button
                    key={n.id}
                    onClick={() => {
                      if (!n.read) markRead.mutate(n.id);
                      const route = getNotificationRoute(n.type, n.data);
                      if (route) router.push(route);
                    }}
                    className={`flex w-full flex-col gap-1 px-4 py-3 text-left transition-colors hover:bg-accent border-b border-border/50 last:border-0 ${
                      n.read ? "opacity-60" : ""
                    }`}
                  >
                    <div className="flex items-start justify-between gap-2">
                      <span
                        className={`text-sm ${n.read ? "font-normal" : "font-semibold"}`}
                      >
                        {n.title}
                      </span>
                      {!n.read && (
                        <span className="mt-1 h-3 w-3 shrink-0 rounded-full bg-primary" />
                      )}
                    </div>
                    <MediaNotificationBody notification={n} />
                    {n.type === "infra_decision" && <NotificationInfraBody n={n} />}
                    <span className="text-2xs text-muted-foreground-subtle">
                      {new Date(n.created_at).toLocaleString()}
                    </span>
                  </button>
                ))}
                {loadingOlder && (
                  <div className="flex items-center justify-center py-3">
                    <Loader2 size={16} className="animate-spin text-muted-foreground" />
                  </div>
                )}
              </>
            )}
          </div>
        )}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
