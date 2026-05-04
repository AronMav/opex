"use client";

import { memo, useState, useMemo } from "react";
import { useTranslation } from "@/hooks/use-translation";
import { Collapsible, CollapsibleTrigger, CollapsibleContent } from "@/components/ui/collapsible";
import { ChevronRight, FileText, Wrench, Check, X, Clock } from "lucide-react";
import { truncateOutput } from "@/lib/format";

export const TOOL_OUTPUT_MAX_CHARS = 10_000;

// ── Icon picker by tool name ──────────────────────────────────────────────────
function ToolIcon({ toolName }: { toolName: string }) {
  const cls = "h-3 w-3 shrink-0";
  if (toolName.startsWith("workspace_")) return <FileText className={cls} />;
  return <Wrench className={cls} />;
}

// ── Extract a short detail string from tool args ──────────────────────────────
function toolDetail(toolName: string, args: Record<string, unknown>): string | null {
  if (toolName.startsWith("workspace_")) {
    const p = args.path ?? args.file_path ?? args.target_path;
    if (typeof p === "string") return p.split(/[\\/]/).slice(-2).join("/");
  }
  if (toolName === "code_exec") {
    const lang = args.language ?? args.lang;
    return typeof lang === "string" ? lang : null;
  }
  return null;
}

export const ToolCallPartView = memo(function ToolCallPartView({ toolName, args, result, status }: {
  toolName: string;
  args: Record<string, unknown>;
  result?: unknown;
  status: { type: string };
}) {
  const { t } = useTranslation();
  const isComplete = status.type === "complete";
  const hasError   = status.type === "error";
  const isDenied   = status.type === "denied";
  const isRunning  = status.type === "running" || status.type === "calling" || status.type === "requires-action";
  const hasContent = isComplete || hasError || isDenied;

  const detail = useMemo(() => toolDetail(toolName, args), [toolName, args]);

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

  const canExpand = hasContent || !!inputDisplay;

  return (
    <Collapsible className="group">
      <CollapsibleTrigger asChild>
        <button
          type="button"
          disabled={!canExpand}
          className="flex w-full items-center gap-2 rounded-xl border border-border/60 bg-card/50 px-2.5 py-1.5 text-left transition-colors hover:border-border disabled:cursor-default dark:bg-card/30 dark:hover:bg-card/50"
        >
          {/* tool type icon */}
          <span className="text-muted-foreground/50"><ToolIcon toolName={toolName} /></span>

          {/* tool name */}
          <span className="font-mono text-[11px] font-semibold shrink-0 truncate text-foreground/80">
            {toolName}
          </span>

          {/* file / detail */}
          {detail && (
            <span className="text-[10px] text-muted-foreground/50 flex-1 truncate">
              {detail}
            </span>
          )}

          {/* status indicator */}
          <span className="ml-auto shrink-0 flex items-center gap-1.5">
            {isRunning ? (
              <span className="h-2 w-2 rounded-full bg-warning animate-pulse" />
            ) : isComplete ? (
              <Check className="h-3 w-3 text-success" />
            ) : (hasError || isDenied) ? (
              <X className="h-3 w-3 text-destructive" />
            ) : (
              <Clock className="h-3 w-3 text-muted-foreground/40" />
            )}
            {canExpand && (
              <ChevronRight className="h-3.5 w-3.5 text-muted-foreground/40 transition-transform duration-200 group-data-[state=open]:rotate-90" />
            )}
          </span>
        </button>
      </CollapsibleTrigger>

      <CollapsibleContent>
        <div className="mt-1 overflow-hidden rounded-md border border-border/40 bg-muted/20 text-xs">
          {inputDisplay && (
            <div className={hasContent ? "border-b border-border/30" : ""}>
              <div className="px-3 pt-2.5 pb-1">
                <span className="font-mono text-[9px] font-bold uppercase tracking-widest text-primary/50">
                  {t("chat.tool_input")}
                </span>
              </div>
              <pre className="max-h-[150px] overflow-auto px-3 pb-2.5 font-mono text-[11px] leading-relaxed text-foreground/70 whitespace-pre-wrap">
                {inputDisplay}
              </pre>
            </div>
          )}

          {hasContent && (
            <div>
              <div className="flex items-center justify-between px-3 pt-2.5 pb-1">
                <span className={`font-mono text-[9px] font-bold uppercase tracking-widest ${
                  hasError || isDenied ? "text-destructive/70" : "text-success/70"
                }`}>
                  {hasError ? t("chat.tool_error") : isDenied ? t("chat.tool_denied") : t("chat.tool_result")}
                </span>
                {resultTruncated && (
                  <button
                    type="button"
                    onClick={() => setShowFullOutput(true)}
                    className="text-[10px] text-primary/60 hover:text-primary underline underline-offset-2"
                  >
                    {t("chat.tool_show_full", { chars: Math.round(resultHiddenChars / 1000) })}
                  </button>
                )}
              </div>
              <pre className={[
                "max-h-[300px] overflow-auto px-3 pb-2.5 font-mono text-[11px] leading-relaxed whitespace-pre-wrap",
                hasError || isDenied ? "text-destructive/80" : "text-success/90",
              ].join(" ")}>
                {resultDisplay}
              </pre>
            </div>
          )}
        </div>
      </CollapsibleContent>
    </Collapsible>
  );
});
