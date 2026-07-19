"use client";

import { PulseDotLoader } from "@/components/ui/loader";
import { cn } from "@/lib/utils";
import { useTranslation } from "@/hooks/use-translation";
import { useChatStore } from "@/stores/chat-store";

interface ReconnectingIndicatorProps {
  className?: string;
}

/**
 * LLM-level provider-retry indicator AND transport-reconnect indicator.
 *
 * Two distinct reconnect surfaces route through this single affordance:
 *   * `isLlmReconnecting` — set by the server's `reconnecting` SSE event
 *     (LLM-level provider retry inside the deadline-retry loop).
 *   * `transportReconnectAttempt` — set by the streaming renderer while it
 *     backs off and retries the GET envelope after a drop. H8 fix: the
 *     attempt counter is now surfaced so the user sees "attempt N/M" instead
 *     of an opaque 30s spinner.
 *
 * The label switches based on which signal is active so the user gets an
 * accurate hint about what is happening.
 */
export function ReconnectingIndicator({ className }: ReconnectingIndicatorProps) {
  const { t } = useTranslation();
  const isLlmReconnecting = useChatStore(
    (s) => s.agents[s.currentAgent]?.isLlmReconnecting ?? false,
  );
  const transportAttempt = useChatStore(
    (s) => s.agents[s.currentAgent]?.transportReconnectAttempt ?? 0,
  );
  // Mirror the renderer's RECONNECT_MAX_RETRIES so the badge can show "N/M".
  const RECONNECT_MAX_RETRIES = 6;
  const label = transportAttempt > 0
    ? `${t("chat.reconnecting")} (${transportAttempt}/${RECONNECT_MAX_RETRIES})`
    : t("chat.reconnecting");
  return (
    <div
      role="status"
      aria-live="polite"
      aria-label={label}
      className={cn(
        "mx-auto flex max-w-fit items-center gap-2 rounded-lg border border-primary/30 bg-muted/30 px-3 py-2",
        className,
      )}
    >
      <PulseDotLoader size="sm" />
      <span className="text-sm text-muted-foreground">
        {isLlmReconnecting || transportAttempt > 0 ? label : t("chat.reconnecting")}
        <span className="inline-flex">
          <span className="animate-[loading-dots_1.4s_infinite_0.2s]">.</span>
          <span className="animate-[loading-dots_1.4s_infinite_0.4s]">.</span>
          <span className="animate-[loading-dots_1.4s_infinite_0.6s]">.</span>
        </span>
      </span>
    </div>
  );
}
