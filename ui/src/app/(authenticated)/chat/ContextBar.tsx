"use client";

// ── ContextBar.tsx ────────────────────────────────────────────────────────────
// Compact progress bar showing context window usage after each LLM response.
// Hidden when model is unknown or no usage data has been received.

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
  /** Output token count from the most recent LLM response. Optional. */
  outputTokens?: number | null;
  /** Cache-read tokens (subset of input). Anthropic ×0.1, OpenAI ×0.5 cost. */
  cacheReadTokens?: number | null;
  /** Cache-creation/write tokens (subset of input). Anthropic ×1.25 cost. */
  cacheCreationTokens?: number | null;
  /** Hidden reasoning tokens (subset of output). OpenAI o1/o3, Gemini thinking. */
  reasoningTokens?: number | null;
}

function formatK(n: number): string {
  return n >= 1000 ? `${Math.round(n / 1000)}k` : `${n}`;
}

function formatAbsolute(n: number): string {
  return n.toLocaleString("ru-RU");
}

export function ContextBar({
  tokens,
  model,
  outputTokens,
  cacheReadTokens,
  cacheCreationTokens,
  reasoningTokens,
}: ContextBarProps) {
  const limit = getContextLimit(model);

  // Hide when either piece of data is missing.
  if (tokens == null || limit == null) return null;

  const ratio = Math.min(1, tokens / limit);
  const pct = Math.round(ratio * 100);

  const barColor =
    ratio > 0.95
      ? "bg-red-500"
      : ratio > 0.8
        ? "bg-yellow-500"
        : "bg-neutral-400";

  const remaining = Math.max(0, limit - tokens);

  // Build a multi-line breakdown when extended fields are present.
  // Note: extended fields are SUBSETS of input/output (NOT additive).
  const lines: string[] = [];
  lines.push(`Контекст: ${formatAbsolute(tokens)} / ${formatAbsolute(limit)} (осталось ${formatAbsolute(remaining)})`);
  lines.push("");
  lines.push(`Input: ${formatAbsolute(tokens)}`);
  if (cacheCreationTokens != null && cacheCreationTokens > 0) {
    lines.push(`  └─ cache write: ${formatAbsolute(cacheCreationTokens)} (×1.25 cost)`);
  }
  if (cacheReadTokens != null && cacheReadTokens > 0) {
    lines.push(`  └─ cache read:  ${formatAbsolute(cacheReadTokens)} (×0.1 cost)`);
  }
  if (outputTokens != null && outputTokens > 0) {
    lines.push(`Output: ${formatAbsolute(outputTokens)}`);
    if (reasoningTokens != null && reasoningTokens > 0) {
      lines.push(`  └─ reasoning:    ${formatAbsolute(reasoningTokens)}`);
    }
  }
  const tooltipText = lines.join("\n");

  return (
    <TooltipProvider delayDuration={200}>
      <Tooltip>
        <TooltipTrigger asChild>
          <div className="flex items-center gap-1.5 cursor-default select-none">
            {/* Label */}
            <span className="text-[11px] text-muted-foreground/60 tabular-nums whitespace-nowrap">
              {formatK(tokens)} / {formatK(limit)}
            </span>
            {/* Progress bar */}
            <div className="h-[6px] w-16 rounded-full bg-muted/40 overflow-hidden">
              <div
                className={`h-full rounded-full transition-all duration-500 ${barColor}`}
                style={{ width: `${pct}%` }}
              />
            </div>
            {/* Warning label when context almost full */}
            {ratio > 0.95 && (
              <span className="text-[10px] text-red-500 font-medium whitespace-nowrap">
                Контекст почти заполнен
              </span>
            )}
          </div>
        </TooltipTrigger>
        <TooltipContent side="bottom" className="text-xs max-w-xs whitespace-pre-line font-mono">
          {tooltipText}
        </TooltipContent>
      </Tooltip>
    </TooltipProvider>
  );
}
