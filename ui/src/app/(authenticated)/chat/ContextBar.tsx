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
}

function formatK(n: number): string {
  return n >= 1000 ? `${Math.round(n / 1000)}k` : `${n}`;
}

function formatAbsolute(n: number): string {
  return n.toLocaleString("ru-RU");
}

export function ContextBar({ tokens, model }: ContextBarProps) {
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
  const tooltipText = `Использовано токенов контекста: ${formatAbsolute(tokens)} из ${formatAbsolute(limit)}. Осталось: ${formatAbsolute(remaining)}`;

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
        <TooltipContent side="bottom" className="text-xs max-w-xs">
          {tooltipText}
        </TooltipContent>
      </Tooltip>
    </TooltipProvider>
  );
}
