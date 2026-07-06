"use client";

import { memo, useState, useMemo, useEffect, useRef } from "react";
import { useTranslation } from "@/hooks/use-translation";
import { Collapsible, CollapsibleTrigger, CollapsibleContent } from "@/components/ui/collapsible";
import { ChevronRight, FileText, Wrench, Check, X, Clock } from "lucide-react";
import { truncateOutput } from "@/lib/format";

export const TOOL_OUTPUT_MAX_CHARS = 10_000;

// ── Icon picker by tool name ──────────────────────────────────────────────────
function ToolIcon({ toolName }: { toolName: string }) {
  const cls = "h-4 w-4 shrink-0";
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

// ── Elapsed time display ─────────────────────────────────────────────────────
function useElapsed(isRunning: boolean): number {
  const [elapsed, setElapsed] = useState(0);
  const startRef = useRef(Date.now());

  useEffect(() => {
    if (!isRunning) return;
    startRef.current = Date.now();
    setElapsed(0);
    const interval = setInterval(() => {
      setElapsed(Date.now() - startRef.current);
    }, 100);
    return () => clearInterval(interval);
  }, [isRunning]);

  return elapsed;
}

function formatElapsed(ms: number): string {
  if (ms < 1000) return `${ms}ms`;
  if (ms < 60_000) return `${(ms / 1000).toFixed(1)}s`;
  return `${Math.floor(ms / 60_000)}m ${Math.floor((ms % 60_000) / 1000)}s`;
}

// ── Syntax-highlighted tool output ───────────────────────────────────────────
function HighlightedOutput({ code, language }: { code: string; language?: string }) {
  const [html, setHtml] = useState<string | null>(null);

  useEffect(() => {
    if (!code || code.length > 5000) {
      setHtml(null);
      return;
    }
    let cancelled = false;
    (async () => {
      try {
        const { codeToHtml } = await import("shiki");
        const result = await codeToHtml(code, {
          lang: language || "text",
          theme: document.documentElement.classList.contains("dark") ? "github-dark" : "github-light",
        });
        if (!cancelled) setHtml(result);
      } catch {
        if (!cancelled) setHtml(null);
      }
    })();
    return () => { cancelled = true; };
  }, [code, language]);

  if (!html) {
    return <pre className="max-h-[300px] overflow-auto px-3 pb-2.5 font-mono text-2xs leading-relaxed whitespace-pre-wrap">{code}</pre>;
  }

  return (
    <div
      className="max-h-[300px] overflow-auto px-3 pb-2.5 [&>pre]:bg-transparent [&>pre]:px-0 [&>pre]:py-0 [&>pre]:text-2xs [&>pre]:leading-relaxed"
      dangerouslySetInnerHTML={{ __html: html }}
    />
  );
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
  const elapsed = useElapsed(isRunning);

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

  // Detect language for syntax highlighting
  const resultLanguage = useMemo(() => {
    if (toolName === "code_exec") {
      const lang = args.language ?? args.lang;
      if (typeof lang === "string") return lang;
    }
    if (resultRaw.includes("<?xml") || resultRaw.includes("<html")) return "html";
    if (resultRaw.startsWith("{") || resultRaw.startsWith("[")) return "json";
    return undefined;
  }, [toolName, args, resultRaw]);

  const canExpand = hasContent || !!inputDisplay;

  return (
    <Collapsible className="group">
      <CollapsibleTrigger asChild>
        <button
          type="button"
          disabled={!canExpand}
          aria-label={`${toolName}${detail ? `: ${detail}` : ""} — ${isRunning ? t("chat.tool_running") : isComplete ? t("chat.tool_result") : hasError ? t("chat.tool_error") : isDenied ? t("chat.tool_denied") : ""}`}
          className="flex w-full min-w-0 items-center gap-2 rounded-xl border border-border/50 bg-card/50 px-2.5 py-1.5 text-left transition-colors hover:border-border disabled:cursor-default dark:bg-card/30 dark:hover:bg-card/50"
        >
          {/* tool type icon */}
          <span className="text-muted-foreground/50"><ToolIcon toolName={toolName} /></span>

          {/* tool name */}
          <span className="font-mono text-2xs font-semibold min-w-0 truncate text-foreground/80">
            {toolName}
          </span>

          {/* file / detail */}
          {detail && (
            <span className="text-3xs text-muted-foreground-subtle flex-1 min-w-0 truncate">
              {detail}
            </span>
          )}

          {/* elapsed time */}
          {isRunning && elapsed > 0 && (
            <span className="text-3xs text-muted-foreground-subtle font-mono tabular-nums shrink-0">
              {formatElapsed(elapsed)}
            </span>
          )}

          {/* status indicator */}
          <span className="ml-auto shrink-0 flex items-center gap-1.5">
            {isRunning ? (
              <span className="h-3 w-3 rounded-full bg-warning animate-pulse" />
            ) : isComplete ? (
              <Check className="h-4 w-4 text-success" />
            ) : (hasError || isDenied) ? (
              <X className="h-4 w-4 text-destructive" />
            ) : (
              <Clock className="h-4 w-4 text-muted-foreground/50" />
            )}
            {canExpand && (
              <ChevronRight className="h-3.5 w-3.5 text-muted-foreground/50 transition-transform duration-200 group-data-[state=open]:rotate-90" />
            )}
          </span>
        </button>
      </CollapsibleTrigger>

      <CollapsibleContent>
        <div className="mt-1 overflow-hidden rounded-md border border-border/30 bg-muted/20 text-xs">
          {inputDisplay && (
            <div className={hasContent ? "border-b border-border/30" : ""}>
              <div className="px-3 pt-2.5 pb-1">
                <span className="font-mono text-3xs font-bold uppercase tracking-widest text-primary/50">
                  {t("chat.tool_input")}
                </span>
              </div>
              <pre className="max-h-[150px] overflow-auto px-3 pb-2.5 font-mono text-2xs leading-relaxed text-foreground/80 whitespace-pre-wrap">
                {inputDisplay}
              </pre>
            </div>
          )}

          {hasContent && (
            <div>
              <div className="flex items-center justify-between px-3 pt-2.5 pb-1">
                <span className={`font-mono text-3xs font-bold uppercase tracking-widest ${
                  hasError || isDenied ? "text-destructive/70" : "text-success/70"
                }`}>
                  {hasError ? t("chat.tool_error") : isDenied ? t("chat.tool_denied") : t("chat.tool_result")}
                </span>
                {resultTruncated && (
                  <button
                    type="button"
                    onClick={() => setShowFullOutput(true)}
                    className="text-3xs text-primary/50 hover:text-primary underline underline-offset-2"
                  >
                    {t("chat.tool_show_full", { chars: Math.round(resultHiddenChars / 1000) })}
                  </button>
                )}
              </div>
              {hasError || isDenied ? (
                <pre className="max-h-[300px] overflow-auto px-3 pb-2.5 font-mono text-2xs leading-relaxed whitespace-pre-wrap text-destructive/80">
                  {resultDisplay}
                </pre>
              ) : (
                <HighlightedOutput code={resultDisplay} language={resultLanguage} />
              )}
            </div>
          )}
        </div>
      </CollapsibleContent>
    </Collapsible>
  );
});
