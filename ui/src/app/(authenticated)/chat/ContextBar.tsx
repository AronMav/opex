"use client";

// ── ContextBar.tsx ────────────────────────────────────────────────────────────
// Always-visible model badge + token usage bar in the chat header.

import React from "react";
import { getContextLimit } from "@/lib/model-limits";
import { useTranslation } from "@/hooks/use-translation";
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from "@/components/ui/tooltip";

interface ContextBarProps {
  tokens: number | null;
  model: string | null | undefined;
  /** Real context window from backend (single source of truth). Overrides static table. */
  modelContextLimit?: number | null;
  cacheReadTokens?: number | null;
  cacheCreationTokens?: number | null;
  reasoningTokens?: number | null;
  /** True while the model is actively generating — values are stale until done. */
  isGenerating?: boolean;
}

function formatK(n: number): string {
  return n >= 1000 ? `${Math.round(n / 1000)}k` : `${n}`;
}

function formatNum(n: number): string {
  return n.toLocaleString();
}

// Shorten model ID to a readable label: "claude-sonnet-4-6" → "sonnet-4-6"
function shortModel(model: string): string {
  return model.replace(/^claude-/, "").replace(/-\d{8}$/, "");
}

export function ContextBar({
  tokens,
  model,
  modelContextLimit,
  cacheReadTokens,
  cacheCreationTokens,
  reasoningTokens,
  isGenerating = false,
}: ContextBarProps) {
  const { t } = useTranslation();
  // Backend-provided limit is the single source of truth; fall back to static table.
  const limit = modelContextLimit ?? (model ? getContextLimit(model) : null);

  if (!model && tokens == null) return null;

  const hasUsage = tokens != null && limit != null;
  const ratio = hasUsage ? Math.min(1, tokens! / limit!) : 0;
  const pct   = Math.round(ratio * 100);

  const barColor =
    ratio > 0.95 ? "bg-destructive" :
    ratio > 0.8  ? "bg-warning"     :
                   "bg-primary/50";

  // Tooltip: compact, no redundancy
  const tooltipLines: string[] = [];
  if (hasUsage) {
    const remaining = Math.max(0, limit! - tokens!);
    const pctUsed = Math.round(ratio * 100);
    tooltipLines.push(t("chat.context_tokens", { tokens: formatNum(tokens!), limit: formatNum(limit!), pct: pctUsed }));
    tooltipLines.push(t("chat.context_remaining", { remaining: formatNum(remaining) }));

    const hasCacheDetails =
      (cacheCreationTokens != null && cacheCreationTokens > 0) ||
      (cacheReadTokens != null && cacheReadTokens > 0) ||
      (reasoningTokens != null && reasoningTokens > 0);

    if (hasCacheDetails) {
      tooltipLines.push("");
      if (cacheCreationTokens != null && cacheCreationTokens > 0)
        tooltipLines.push(`↑ cache write: ${formatNum(cacheCreationTokens)}`);
      if (cacheReadTokens != null && cacheReadTokens > 0)
        tooltipLines.push(`↓ cache read: ${formatNum(cacheReadTokens)}`);
      if (reasoningTokens != null && reasoningTokens > 0)
        tooltipLines.push(`✦ reasoning: ${formatNum(reasoningTokens)}`);
    }

    if (isGenerating) {
      tooltipLines.push("");
      tooltipLines.push(t("chat.context_stale"));
    }
  }

  return (
    <TooltipProvider delayDuration={300}>
      <Tooltip>
        <TooltipTrigger asChild>
          <div className="flex items-center gap-2 cursor-default select-none min-w-0 shrink ml-auto">

            {/* Model badge */}
            {model && (
              <span className="rounded-md border border-border/40 bg-muted/30 px-2 py-0.5 font-mono text-[11px] text-muted-foreground/60 whitespace-nowrap">
                {shortModel(model)}
              </span>
            )}

            {/* Token count + progress bar */}
            {hasUsage && (
              <>
                <span className={`text-[11px] tabular-nums whitespace-nowrap transition-opacity ${isGenerating ? "text-muted-foreground/40" : "text-muted-foreground/60"}`}>
                  {formatK(tokens!)} / {formatK(limit!)}
                </span>
                <div className="relative h-[4px] w-14 rounded-full bg-muted/30 overflow-hidden">
                  <div
                    className={`h-full rounded-full transition-all duration-700 ${barColor} ${isGenerating ? "opacity-50" : ""}`}
                    style={{ width: `${pct}%` }}
                  />
                  {isGenerating && (
                    <div className="absolute inset-0 rounded-full bg-primary/20 animate-pulse" />
                  )}
                </div>
                {ratio > 0.95 && !isGenerating && (
                  <span className="text-[10px] text-destructive font-medium whitespace-nowrap">
                    {t("chat.context_almost_full")}
                  </span>
                )}
              </>
            )}

          </div>
        </TooltipTrigger>

        {tooltipLines.length > 0 && (
          <TooltipContent
            side="bottom"
            className="bg-popover/95 border border-border/60 text-popover-foreground backdrop-blur-sm text-[11px] font-mono max-w-[240px] whitespace-pre-line shadow-lg"
          >
            {tooltipLines.join("\n")}
          </TooltipContent>
        )}
      </Tooltip>
    </TooltipProvider>
  );
}
