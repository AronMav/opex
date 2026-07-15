"use client";

import { PulseDotLoader } from "@/components/ui/loader";
import { cn } from "@/lib/utils";
import { useTranslation } from "@/hooks/use-translation";

interface ReconnectingIndicatorProps {
  className?: string;
}

/**
 * LLM-level provider-retry indicator. Shown while `isLlmReconnecting` is set by
 * the server's `reconnecting` SSE event. T8 removed the transport-level attempt
 * counters, so this is a plain "Reconnecting…" affordance with no numeric count.
 */
export function ReconnectingIndicator({ className }: ReconnectingIndicatorProps) {
  const { t } = useTranslation();
  return (
    <div
      role="status"
      aria-live="polite"
      aria-label={t("chat.reconnecting")}
      className={cn(
        "mx-auto flex max-w-fit items-center gap-2 rounded-lg border border-primary/30 bg-muted/30 px-3 py-2",
        className,
      )}
    >
      <PulseDotLoader size="sm" />
      <span className="text-sm text-muted-foreground">
        {t("chat.reconnecting")}
        <span className="inline-flex">
          <span className="animate-[loading-dots_1.4s_infinite_0.2s]">.</span>
          <span className="animate-[loading-dots_1.4s_infinite_0.4s]">.</span>
          <span className="animate-[loading-dots_1.4s_infinite_0.6s]">.</span>
        </span>
      </span>
    </div>
  );
}
