"use client";

import { ChevronRight } from "lucide-react";
import { ToolCallPartView } from "@/components/chat/ToolCallPartView";
import { mapToolPartState } from "@/lib/tool-state";
import type { StepGroupPart } from "@/stores/chat-store";
import { useTranslation } from "@/hooks/use-translation";

// ── StepGroup component ────────────────────────────────────────────────────

export function StepGroup({
  stepGroup,
  isLastGroup = false,
}: {
  stepGroup: StepGroupPart;
  isLastGroup?: boolean;
}) {
  const { t } = useTranslation();

  const allDone = stepGroup.toolParts.every(
    (tp) => tp.state === "output-available" || tp.state === "output-error" || tp.state === "output-denied",
  );
  const defaultOpen = isLastGroup || !allDone;

  return (
    <details
      className="group"
      open={defaultOpen ? true : undefined}
    >
      <summary className="flex items-center gap-1.5 py-1 cursor-pointer list-none [&::-webkit-details-marker]:hidden w-fit">
        <ChevronRight className="h-3.5 w-3.5 shrink-0 text-muted-foreground/40 transition-transform group-open:rotate-90" />
        <span className="text-[11px] text-muted-foreground/50 select-none">
          {allDone ? t("chat.step_done") : t("chat.step_processing")}
        </span>
        {!allDone && (
          <span className="h-1.5 w-1.5 rounded-full bg-primary animate-pulse" />
        )}
      </summary>

      <div className="mt-1.5 space-y-1">
        {stepGroup.toolParts.map((tp) => (
          <ToolCallPartView
            key={tp.toolCallId}
            toolName={tp.toolName}
            args={tp.input}
            result={tp.output}
            status={{ type: mapToolPartState(tp.state) }}
          />
        ))}
      </div>
    </details>
  );
}
