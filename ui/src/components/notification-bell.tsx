"use client";

import { useState, useEffect, useRef } from "react";
import { useRouter } from "next/navigation";
import { Bell } from "lucide-react";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { useNotificationStore } from "@/stores/notification-store";
import {
  useNotifications,
  useMarkNotificationRead,
  useMarkAllRead,
  useClearAllNotifications,
  useNotificationWsSync,
} from "@/lib/queries";
import { Button } from "@/components/ui/button";
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

// ── Media notification body ───────────────────────────────────────────────────

interface MediaNotificationBodyProps {
  notification: NotificationRow;
}

// Event types emitted by `media_background.rs`.
// Voice keeps the historical `tts_*` names for back-compat with code paths
// outside the notification bell (e.g. analytics, hooks). Photo / video / other
// have dedicated events with kind-appropriate inline rendering.
const READY_EVENTS = new Set([
  "tts_ready",
  "image_ready",
  "video_ready",
  "media_ready",
]);
const ERROR_EVENTS = new Set([
  "tts_error",
  "image_error",
  "video_error",
  "media_error",
]);

function readyKindFromType(type: string): "voice" | "image" | "video" | "media" | null {
  switch (type) {
    case "tts_ready":   return "voice";
    case "image_ready": return "image";
    case "video_ready": return "video";
    case "media_ready": return "media";
    default:            return null;
  }
}

export function MediaNotificationBody({ notification }: MediaNotificationBodyProps) {
  const { type, title, body, data } = notification;
  const url = typeof data?.url === "string" ? (data.url as string) : null;

  // Success path — render an inline preview when we have a url, otherwise
  // fall through to a plain body line.
  const readyKind = readyKindFromType(type);
  if (readyKind && url) {
    return (
      <div className="flex flex-col gap-1 w-full" onClick={(e) => e.stopPropagation()}>
        <span className="text-xs text-muted-foreground">{body}</span>
        {readyKind === "voice" && (
          <audio
            controls
            src={url}
            className="w-full mt-1 h-8"
            data-testid="tts-audio-player"
          />
        )}
        {readyKind === "image" && (
          // eslint-disable-next-line @next/next/no-img-element
          <img
            src={url}
            alt={title}
            className="mt-1 max-h-48 w-auto rounded-md border border-border object-contain"
            data-testid="image-preview"
            loading="lazy"
          />
        )}
        {readyKind === "video" && (
          <video
            controls
            src={url}
            className="w-full mt-1 max-h-48 rounded-md border border-border"
            data-testid="video-player"
            preload="metadata"
          />
        )}
        {readyKind === "media" && (
          <a
            href={url}
            target="_blank"
            rel="noopener noreferrer"
            className="mt-1 text-xs text-primary underline underline-offset-2"
            data-testid="media-download"
          >
            {url.split("/").pop() || url}
          </a>
        )}
      </div>
    );
  }

  // Error path for any media-flavoured event.
  if (ERROR_EVENTS.has(type)) {
    return (
      <span className="text-xs text-destructive line-clamp-2">{body}</span>
    );
  }

  // Default body line — also used when a ready event arrives without a usable url.
  return <span className="text-xs text-muted-foreground line-clamp-2">{body}</span>;
}

// Back-compat alias — kept so any external consumer still imports the old name
// without breaking. Internal code uses `MediaNotificationBody` directly.
export const TtsNotificationBody = MediaNotificationBody;

// Used by `getNotificationRoute` to decide that a notification is rendered
// inline (no navigation) rather than linking somewhere.
function isMediaEvent(type: string): boolean {
  return READY_EVENTS.has(type) || ERROR_EVENTS.has(type);
}

// ── Notification type → target route ─────────────────────────────────────────

// Exported so unit tests can verify routing decisions for every notification
// type without rendering the bell. Keeping it free-standing also documents
// the routing contract in one place.
export function getNotificationRoute(type: string): string | null {
  // Media events (tts/image/video/media — ready + error) render inline; no nav.
  if (isMediaEvent(type)) return null;
  switch (type) {
    case "access_request":  return "/access";
    case "tool_approval":   return "/monitor/?tab=approvals";
    case "agent_error":     return "/monitor/?tab=logs";
    case "watchdog_alert":  return "/monitor/?tab=watchdog";
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
            <span className="absolute -right-0.5 -top-0.5 flex h-4 min-w-4 items-center justify-center rounded-full bg-primary px-1 text-[10px] font-bold text-primary-foreground leading-none">
              {unread_count > 99 ? "99+" : unread_count}
            </span>
          )}
        </button>
      </DropdownMenuTrigger>
      <DropdownMenuContent       align="end"
      className="w-80 max-w-[calc(100dvw-0.5rem)] p-0" data-testid="notification-list">
        {/* Header */}
        <div className="flex items-center justify-between border-b border-border px-4 py-3">
          <span className="text-sm font-semibold">{t("notifications.title")}</span>
          <div className="flex items-center gap-3">
            {unread_count > 0 && (
              <Button
                variant="link"
                size="xs"
                onClick={() => markAllRead.mutate()}
              >
                {t("notifications.mark_all_read")}
              </Button>
            )}
            {notifications.length > 0 && (
              <Button
                variant="link"
                size="xs"
                className="text-destructive"
                onClick={() => clearAll.mutate()}
              >
                {t("notifications.clear_all")}
              </Button>
            )}
          </div>
        </div>
        {/* List */}
        <div className="max-h-[min(24rem,calc(100dvh-8rem))] overflow-y-auto overscroll-contain">
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
                    <span className="mt-1 h-3 w-3 shrink-0 rounded-full bg-primary" />
                  )}
                </div>
                <MediaNotificationBody notification={n} />
                <span className="text-[11px] text-muted-foreground-subtle">
                  {new Date(n.created_at).toLocaleString()}
                </span>
              </button>
            ))
          )}
        </div>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
