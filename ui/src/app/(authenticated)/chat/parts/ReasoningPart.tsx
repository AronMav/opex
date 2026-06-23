"use client";

import { useTranslation } from "@/hooks/use-translation";
import { MessageContent } from "@/components/ui/message";

export function ReasoningPart({ text }: { text: string }) {
  const { t } = useTranslation();
  return (
    <div className="rounded-xl border border-primary/30 bg-primary/10 p-3 text-sm text-muted-foreground/70 dark:text-muted-foreground/70">
      <div className="flex items-center gap-2 mb-1.5">
        <div className="h-1.5 w-1.5 rounded-full bg-primary/80 animate-pulse" />
        <span className="font-mono text-xs font-semibold uppercase tracking-wider text-primary/70 dark:text-primary/50">
          {t("chat.reasoning")}
        </span>
      </div>
      <MessageContent
        markdown
        className="prose prose-sm max-w-none bg-transparent p-0
          [&_p]:leading-relaxed [&_p]:text-foreground/70 dark:[&_p]:text-foreground/70 [&_p]:text-sm
          [&_a]:text-primary/70 [&_a]:no-underline hover:[&_a]:underline
          [&_li]:text-foreground/70 [&_strong]:text-foreground/70"
      >
        {text}
      </MessageContent>
    </div>
  );
}
