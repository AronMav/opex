"use client";

import { memo, useEffect, useState } from "react";
import { useTranslation } from "@/hooks/use-translation";
import { MessageContent } from "@/components/ui/message";
import { Collapsible, CollapsibleTrigger, CollapsibleContent } from "@/components/ui/collapsible";
import { ChevronRight } from "lucide-react";

function ReasoningPartImpl({ text, streaming = false }: { text: string; streaming?: boolean }) {
  const { t } = useTranslation();
  // Auto-expand while the model is still emitting reasoning; collapse once the
  // turn finishes. Controlled so the open state tracks `streaming`, while still
  // letting the user toggle manually between transitions.
  const [open, setOpen] = useState(streaming);
  useEffect(() => {
    setOpen(streaming);
  }, [streaming]);

  return (
    <Collapsible
      open={open}
      onOpenChange={setOpen}
      className="group rounded-xl border border-primary/30 bg-primary/10 p-3 text-sm text-muted-foreground-subtle"
    >
      <CollapsibleTrigger asChild>
        <button
          type="button"
          className="flex w-full items-center gap-2 text-left"
          aria-label={t("chat.reasoning")}
        >
          <div className={`h-1.5 w-1.5 rounded-full bg-primary/30 ${streaming ? "animate-pulse" : ""}`} />
          <span className="font-mono text-xs font-semibold uppercase tracking-wider text-primary/80 dark:text-primary/50">
            {t("chat.reasoning")}
          </span>
          <ChevronRight className="ml-auto h-3.5 w-3.5 text-primary/50 transition-transform duration-200 group-data-[state=open]:rotate-90" />
        </button>
      </CollapsibleTrigger>
      <CollapsibleContent>
        <MessageContent
          markdown
          className="prose prose-sm max-w-none bg-transparent p-0 mt-1.5
            [&_p]:leading-relaxed [&_p]:text-foreground/80 dark:[&_p]:text-foreground/80 [&_p]:text-sm
            [&_a]:text-primary/80 [&_a]:no-underline hover:[&_a]:underline
            [&_li]:text-foreground/80 [&_strong]:text-foreground/80"
        >
          {text}
        </MessageContent>
      </CollapsibleContent>
    </Collapsible>
  );
}

export const ReasoningPart = memo(ReasoningPartImpl);
