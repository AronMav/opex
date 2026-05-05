"use client";

import { useState, useEffect, useRef } from "react";
import { useRouter } from "next/navigation";
import { Bell } from "lucide-react";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { ScrollArea } from "@/components/ui/scroll-area";
import { useNotificationStore } from "@/stores/notification-store";
import {
  useNotifications,
  useMarkNotificationRead,
  useMarkAllRead,
  useClearAllNotifications,
  useNotificationWsSync,
} from "@/lib/queries";
import { useTranslation } from "@/hooks/use-translation";
import type { NotificationRow } from "@/types/api";

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

// ── TTS notification body ─────────────────────────────────────────────────────

interface TtsNotificationBodyProps {
  notification: NotificationRow;
}

export function TtsNotificationBody({ notification }: TtsNotificationBodyProps) {
  const { type, body, data } = notification;

  if (type === "tts_ready" && data?.url) {
    return (
      <div className="flex flex-col gap-1 w-full" onClick={(e) => e.stopPropagation()}>
        <span className="text-xs text-muted-foreground">{body}</span>
        <audio
          controls
          src={data.url as string}
          className="w-full mt-1 h-8"
          data-testid="tts-audio-player"
        />
      </div>
    );
  }

  if (type === "tts_error") {
    return (
      <span className="text-xs text-destructive line-clamp-2">{body}</span>
    );
  }

  return <span className="text-xs text-muted-foreground line-clamp-2">{body}</span>;
}

// ── Notification type → target route ─────────────────────────────────────────

function getNotificationRoute(type: string): string | null {
  switch (type) {
    case "access_request":  return "/access";
    case "tool_approval":   return "/monitor/?tab=approvals";
    case "agent_error":     return "/monitor/?tab=logs";
    case "watchdog_alert":  return "/monitor/?tab=watchdog";
    case "tts_ready":       return null;  // audio player inline — no navigation
    case "tts_error":       return null;
    default:                return "/monitor/";
  }
}

// ── NotificationBell ─────────────────────────────────────────────────────────

export function NotificationBell() {
  const { t } = useTranslation();
  const router = useRouter();
  const notifications = useNotificationStore((s) => s.notifications);
  const unread_count = useNotificationStore((s) => s.unread_count);

  const [flashing, setFlashing] = useState(false);
  const prevUnreadRef = useRef(unread_count);

  // Fetch initial notifications and wire WS real-time updates
  useNotifications();
  useNotificationWsSync();

  const markRead = useMarkNotificationRead();
  const markAllRead = useMarkAllRead();
  const clearAll = useClearAllNotifications();

  // Flash + sound on new notification arrival
  useEffect(() => {
    if (unread_count > prevUnreadRef.current) {
      setFlashing(true);
      playNotificationSound();
      const timer = setTimeout(() => setFlashing(false), 1500);
      prevUnreadRef.current = unread_count;
      return () => clearTimeout(timer);
    }
    prevUnreadRef.current = unread_count;
  }, [unread_count]);

  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <button
          className="group relative flex h-9 w-9 items-center justify-center rounded-md transition-colors hover:bg-accent text-muted-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
          aria-label={t("notifications.bell_label")}
        >
          <Bell
            size={20}
            className={`transition-all duration-150 ${
              flashing ? "text-primary scale-125" : "text-muted-foreground"
            }`}
          />
          {unread_count > 0 && (
            <span className="absolute -right-0.5 -top-0.5 flex h-4 min-w-4 items-center justify-center rounded-full bg-primary px-1 text-[10px] font-bold text-primary-foreground leading-none">
              {unread_count > 99 ? "99+" : unread_count}
            </span>
          )}
        </button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="w-80 p-0">
        {/* Header */}
        <div className="flex items-center justify-between border-b border-border px-4 py-3">
          <span className="text-sm font-semibold">{t("notifications.title")}</span>
          <div className="flex items-center gap-3">
            {unread_count > 0 && (
              <button
                onClick={() => markAllRead.mutate()}
                className="text-xs text-muted-foreground hover:text-foreground transition-colors"
              >
                {t("notifications.mark_all_read")}
              </button>
            )}
            {notifications.length > 0 && (
              <button
                onClick={() => clearAll.mutate()}
                className="text-xs text-destructive/70 hover:text-destructive transition-colors"
              >
                {t("notifications.clear_all")}
              </button>
            )}
          </div>
        </div>
        {/* List */}
        <ScrollArea className="max-h-96">
          {notifications.length === 0 ? (
            <div className="flex items-center justify-center py-8 text-sm text-muted-foreground">
              {t("notifications.empty")}
            </div>
          ) : (
            notifications.map((n) => (
              <button
                key={n.id}
                onClick={() => {
                  if (!n.read) markRead.mutate(n.id);
                  const route = getNotificationRoute(n.type);
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
                    <span className="mt-1 h-2 w-2 shrink-0 rounded-full bg-primary" />
                  )}
                </div>
                <TtsNotificationBody notification={n} />
                <span className="text-[11px] text-muted-foreground/60">
                  {new Date(n.created_at).toLocaleString()}
                </span>
              </button>
            ))
          )}
        </ScrollArea>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
