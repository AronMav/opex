"use client";

// ── ContextBar.tsx ────────────────────────────────────────────────────────────
// Always-visible model badge + token usage bar in the chat header.

import React from "react";
import { getContextLimit } from "@/lib/model-limits";
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from "@/components/ui/tooltip";

interface ContextBarProps {
  tokens: number | null;
  model: string | null | undefined;
  outputTokens?: number | null;
  cacheReadTokens?: number | null;
  cacheCreationTokens?: number | null;
  reasoningTokens?: number | null;
}

function formatK(n: number): string {
  return n >= 1000 ? `${Math.round(n / 1000)}k` : `${n}`;
}

function formatAbsolute(n: number): string {
  return n.toLocaleString("ru-RU");
}

// Shorten model ID to a readable label: "claude-sonnet-4-6" → "sonnet-4-6"
function shortModel(model: string): string {
  return model.replace(/^claude-/, "").replace(/-\d{8}$/, "");
}

export function ContextBar({
  tokens,
  model,
  outputTokens,
  cacheReadTokens,
  cacheCreationTokens,
  reasoningTokens,
}: ContextBarProps) {
  const limit = model ? getContextLimit(model) : null;

  // Nothing to show at all
  if (!model && tokens == null) return null;

  const hasUsage = tokens != null && limit != null;
  const ratio = hasUsage ? Math.min(1, tokens! / limit!) : 0;
  const pct   = Math.round(ratio * 100);

  const barColor =
    ratio > 0.95 ? "bg-destructive" :
    ratio > 0.8  ? "bg-warning"     :
                   "bg-muted-foreground/40";

  // Tooltip content
  const lines: string[] = [];
  if (hasUsage) {
    const remaining = Math.max(0, limit! - tokens!);
    lines.push(`Контекст: ${formatAbsolute(tokens!)} / ${formatAbsolute(limit!)} (осталось ${formatAbsolute(remaining)})`);
    lines.push("");
    lines.push(`Input: ${formatAbsolute(tokens!)}`);
    if (cacheCreationTokens != null && cacheCreationTokens > 0)
      lines.push(`  └─ cache write: ${formatAbsolute(cacheCreationTokens)} (×1.25 cost)`);
    if (cacheReadTokens != null && cacheReadTokens > 0)
      lines.push(`  └─ cache read:  ${formatAbsolute(cacheReadTokens)} (×0.1 cost)`);
    if (outputTokens != null && outputTokens > 0) {
      lines.push(`Output: ${formatAbsolute(outputTokens)}`);
      if (reasoningTokens != null && reasoningTokens > 0)
        lines.push(`  └─ reasoning:    ${formatAbsolute(reasoningTokens)}`);
    }
  }
  const tooltipText = lines.join("\n");

  return (
    <TooltipProvider delayDuration={200}>
      <Tooltip>
        <TooltipTrigger asChild>
          <div className="flex items-center gap-2 cursor-default select-none">

            {/* Model badge — always shown */}
            {model && (
              <span className="rounded-md border border-border/50 bg-muted/40 px-2 py-0.5 font-mono text-[11px] text-muted-foreground/70 whitespace-nowrap">
                {shortModel(model)}
              </span>
            )}

            {/* Token count + progress bar — shown after first usage event */}
            {hasUsage && (
              <>
                <span className="text-[11px] text-muted-foreground/60 tabular-nums whitespace-nowrap">
                  {formatK(tokens!)} / {formatK(limit!)}
                </span>
                <div className="h-[5px] w-16 rounded-full bg-muted/40 overflow-hidden">
                  <div
                    className={`h-full rounded-full transition-all duration-500 ${barColor}`}
                    style={{ width: `${pct}%` }}
                  />
                </div>
                {ratio > 0.95 && (
                  <span className="text-[10px] text-destructive font-medium whitespace-nowrap">
                    Контекст почти заполнен
                  </span>
                )}
              </>
            )}

          </div>
        </TooltipTrigger>
        {tooltipText && (
          <TooltipContent side="bottom" className="text-xs max-w-xs whitespace-pre-line font-mono">
            {tooltipText}
          </TooltipContent>
        )}
      </Tooltip>
    </TooltipProvider>
  );
}
