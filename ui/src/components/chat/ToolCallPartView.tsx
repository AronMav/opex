"use client";

import { memo, useState, useMemo } from "react";
import { useTranslation } from "@/hooks/use-translation";
import { Collapsible, CollapsibleTrigger, CollapsibleContent } from "@/components/ui/collapsible";
import { ChevronRight } from "lucide-react";
import { truncateOutput } from "@/lib/format";

export const TOOL_OUTPUT_MAX_CHARS = 10_000;

export const ToolCallPartView = memo(function ToolCallPartView({ toolName, args, result, status }: {
  toolName: string;
  args: Record<string, unknown>;
  result?: unknown;
  status: { type: string };
}) {
  const { t } = useTranslation();
  const isComplete = status.type === "complete";
  const hasError = status.type === "error";
  const isDenied = status.type === "denied";
  const isCalling = status.type === "calling";
  const isRunning = status.type === "running" || status.type === "calling" || status.type === "requires-action";
  const hasContent = isComplete || hasError || isDenied;

  const statusLabel = isCalling
    ? t("chat.tool_calling")
    : isRunning
      ? t("chat.tool_running")
      : isComplete
        ? "OK"
        : hasError
          ? "ERR"
          : isDenied
            ? "DENY"
            : "...";

  const inputDisplay = useMemo(
    () => args && Object.keys(args).length > 0 ? JSON.stringify(args, null, 2) : null,
    [args],
  );

  const resultRaw = useMemo(
    () => result ? (typeof result === "string" ? result : JSON.stringify(result, null, 2)) : "",
    [result],
  );
  const [showFullOutput, setShowFullOutput] = useState(false);
  const { text: resultDisplay, truncated: resultTruncated, hiddenChars: resultHiddenChars } =
    showFullOutput
      ? { text: resultRaw, truncated: false, hiddenChars: 0 }
      : truncateOutput(resultRaw, TOOL_OUTPUT_MAX_CHARS);

  return (
    <Collapsible
      className="group overflow-hidden rounded-xl border border-border/60 bg-card/50 dark:bg-card/30 transition-all hover:border-primary/40 dark:hover:bg-card/50"
    >
      <CollapsibleTrigger asChild>
        <button
          type="button"
          disabled={!hasContent && !inputDisplay}
          className="flex w-full items-center gap-3 px-4 py-3 text-left transition-colors disabled:cursor-default"
        >
          <div
            className={`h-2.5 w-2.5 rounded-full shrink-0 ${
              hasError || isDenied
                ? "bg-destructive shadow-lg shadow-destructive/30"
                : isComplete
                  ? "bg-success shadow-lg shadow-success/30"
                  : "bg-warning animate-pulse shadow-lg shadow-warning/30"
            }`}
          />
          <span className="font-mono text-xs font-semibold tracking-tight text-foreground truncate">
            {toolName}
          </span>
          <div className="ml-auto flex items-center gap-2 shrink-0">
            <span className={`font-mono text-[10px] font-bold uppercase tracking-widest ${
              hasError || isDenied
                ? "text-destructive"
                : isComplete
                  ? "text-success"
                  : "text-muted-foreground/50"
            }`}>
              {statusLabel}
            </span>
            {(hasContent || inputDisplay) && (
              <ChevronRight
                className="h-4 w-4 text-muted-foreground/40 transition-transform duration-300 group-data-[state=open]:rotate-90"
              />
            )}
          </div>
        </button>
      </CollapsibleTrigger>

      <CollapsibleContent>
        <div className="border-t border-border/50 bg-muted/40 dark:bg-muted/20">
          {inputDisplay && (
            <div className="border-b border-border/30 p-3">
              <div className="flex items-center gap-2 mb-1.5">
                <span className="font-mono text-[10px] font-bold uppercase tracking-wider text-primary/70">
                  {t("chat.tool_input")}
                </span>
              </div>
              <pre className="max-h-[150px] overflow-auto whitespace-pre-wrap font-mono text-xs leading-relaxed text-foreground/80 dark:text-foreground/60">
                {inputDisplay}
              </pre>
            </div>
          )}
          {(isComplete || hasError || isDenied) && (
            <div className="p-3">
              <div className="flex items-center justify-between mb-1.5">
                <span className={`font-mono text-[10px] font-bold uppercase tracking-wider ${
                  hasError || isDenied ? "text-destructive" : "text-success"
                }`}>
                  {hasError ? t("chat.tool_error") : isDenied ? t("chat.tool_denied") : t("chat.tool_result")}
                </span>
                {resultTruncated && (
                  <button
                    type="button"
                    onClick={() => setShowFullOutput(true)}
                    className="text-xs text-primary/70 hover:text-primary underline underline-offset-2"
                  >
                    {t("chat.tool_show_full", { chars: Math.round(resultHiddenChars / 1000) })}
                  </button>
                )}
              </div>
              <pre className="max-h-[300px] overflow-auto whitespace-pre-wrap font-mono text-xs leading-relaxed text-foreground/90 dark:text-foreground/70">
                {resultDisplay}
              </pre>
            </div>
          )}
        </div>
      </CollapsibleContent>
    </Collapsible>
  );
});
