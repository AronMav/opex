"use client";

import { PulseDotLoader } from "@/components/ui/loader";
import { cn } from "@/lib/utils";
import { useTranslation } from "@/hooks/use-translation";

interface ReconnectingIndicatorProps {
  attempt: number;
  maxAttempts: number;
  className?: string;
}

export function ReconnectingIndicator({ attempt, maxAttempts, className }: ReconnectingIndicatorProps) {
  const { t } = useTranslation();
  return (
    <div
      role="status"
      aria-live="polite"
      aria-label={t("chat.reconnecting_aria", { attempt, max: maxAttempts })}
      className={cn(
        "mx-auto flex max-w-fit items-center gap-2 rounded-lg border border-primary/20 bg-muted/40 px-3 py-2",
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
      <span className="text-xs text-muted-foreground">
        ({t("chat.reconnecting_attempt_word")} <span className="text-foreground">{attempt}</span>/{maxAttempts})
      </span>
    </div>
  );
}
