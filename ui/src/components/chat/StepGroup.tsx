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

  const firstName = stepGroup.toolParts[0]?.toolName;
  let label: string;
  if (!firstName) {
    label = t("chat.step_processing");
  } else if (firstName === "searxng_search" || firstName.includes("search")) {
    const firstInput = stepGroup.toolParts[0]?.input;
    const query =
      firstInput && typeof firstInput === "object" && "query" in firstInput
        ? String((firstInput as Record<string, unknown>).query)
        : undefined;
    label = query
      ? t("chat.step_searched_query", { query })
      : t("chat.step_searched");
  } else if (firstName === "code_exec") {
    label = t("chat.step_executed_code");
  } else {
    label = t("chat.step_used_tool", { tool: firstName });
  }

  return (
    <details
      className="rounded-lg border border-border/50 bg-muted/10 group"
      open={isLastGroup && !stepGroup.isStreaming ? true : undefined}
    >
      <summary className="flex items-center gap-2 px-3 py-2 cursor-pointer list-none [&::-webkit-details-marker]:hidden">
        <ChevronRight className="size-4 shrink-0 transition-transform group-open:rotate-90" />
        <span className="text-sm text-muted-foreground truncate">
          {label}
        </span>
        {stepGroup.isStreaming && (
          <span className="size-2 rounded-full bg-primary animate-pulse" />
        )}
      </summary>
      <div className="p-4 space-y-1">
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
