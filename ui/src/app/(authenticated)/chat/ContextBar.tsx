"use client";

// ── ContextBar.tsx ────────────────────────────────────────────────────────────
// Always-visible model badge + token usage bar in the chat header.
// Includes checkpoint history trigger button (HistoryIcon → CheckpointPanel).

import React, { useState } from "react";
import { HistoryIcon } from "lucide-react";
import { Button } from "@/components/ui/button";
import { getContextLimit } from "@/lib/model-limits";
import { useTranslation } from "@/hooks/use-translation";
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from "@/components/ui/tooltip";
import { useChatStore } from "@/stores/chat-store";
import { CheckpointPanel } from "./CheckpointPanel";

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
  /** Slim one-row variant for the mobile header: model badge + token progress +
   *  checkpoints trigger, no tooltips. */
  compact?: boolean;
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
  compact = false,
}: ContextBarProps) {
  const { t } = useTranslation();
  const currentAgent = useChatStore((s) => s.currentAgent);
  const [checkpointOpen, setCheckpointOpen] = useState(false);
  // Backend-provided limit is the single source of truth; fall back to static table.
  const limit = modelContextLimit ?? (model ? getContextLimit(model) : null);

  if (!model && tokens == null) return null;

  const hasUsage = tokens != null && limit != null;
  const ratio = hasUsage ? Math.min(1, tokens! / limit!) : 0;
  const pct   = Math.round(ratio * 100);

  const barColor =
    ratio > 0.95 ? "bg-destructive" :
    ratio > 0.8  ? "bg-warning"     :
                   "bg-primary/30";

  // ── Compact (mobile header) ────────────────────────────────────────────────
  // One tight row, no tooltip provider: model badge + token progress + the
  // checkpoint-history trigger. Kept minimal so it fits the crowded mobile bar.
  if (compact) {
    return (
      <>
        <CheckpointPanel agent={currentAgent} open={checkpointOpen} onOpenChange={setCheckpointOpen} />
        <div className="flex items-center gap-1.5 shrink-0">
          {currentAgent && (
            <Button
              variant="outline"
              size="icon"
              className="h-9 w-9 md:h-8 md:w-8 shrink-0 border-primary/30 !bg-primary/10 text-primary shadow-md active:scale-95 transition-all"
              onClick={() => setCheckpointOpen(true)}
              aria-label={t("checkpoints.history")}
              title={t("checkpoints.history")}
            >
              <HistoryIcon className="h-4 w-4 md:h-3.5 md:w-3.5" />
            </Button>
          )}
        </div>
      </>
    );
  }

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
        tooltipLines.push(t("chat.cache_write", { n: formatNum(cacheCreationTokens) }));
      if (cacheReadTokens != null && cacheReadTokens > 0)
        tooltipLines.push(t("chat.cache_read", { n: formatNum(cacheReadTokens) }));
      if (reasoningTokens != null && reasoningTokens > 0)
        tooltipLines.push(t("chat.reasoning_tokens", { n: formatNum(reasoningTokens) }));
    }

    if (isGenerating) {
      tooltipLines.push("");
      tooltipLines.push(t("chat.context_stale"));
    }
  }

  return (
    <>
    <CheckpointPanel
      agent={currentAgent}
      open={checkpointOpen}
      onOpenChange={setCheckpointOpen}
    />
    <TooltipProvider delayDuration={300}>
      <div className="flex items-center gap-2 ml-auto min-w-0 shrink">

        {/* Checkpoint history button — own Tooltip, outside model-badge trigger */}
        {currentAgent && (
          <Tooltip>
            <TooltipTrigger asChild>
              <Button
                variant="outline"
                size="icon"
                className="h-9 w-9 md:h-8 md:w-8 shrink-0 border-primary/30 !bg-primary/10 text-primary shadow-md active:scale-95 transition-all"
                onClick={() => setCheckpointOpen(true)}
                aria-label={t("checkpoints.history")}
              >
                <HistoryIcon className="h-4 w-4 md:h-3.5 md:w-3.5" />
              </Button>
            </TooltipTrigger>
            <TooltipContent side="bottom" className="text-2xs">
              {t("checkpoints.history")}
            </TooltipContent>
          </Tooltip>
        )}

        {/* Model badge + token bar — single Tooltip for usage details */}
        <Tooltip>
          <TooltipTrigger asChild>
            <div className="flex items-center gap-2 cursor-default select-none min-w-0">

              {/* Model badge */}
              {model && (
                <span className="rounded-md border border-border/30 bg-muted/30 px-2 py-0.5 font-mono text-2xs text-muted-foreground/60 whitespace-nowrap">
                  {shortModel(model)}
                </span>
              )}

              {/* Token count + progress bar */}
              {hasUsage && (
                <>
                  <span className={`text-2xs tabular-nums whitespace-nowrap transition-opacity ${isGenerating ? "text-muted-foreground/50" : "text-muted-foreground/60"}`}>
                    {formatK(tokens!)} / {formatK(limit!)}
                  </span>
                  <div className="relative h-1 w-14 rounded-full bg-muted/30 overflow-hidden">
                    <div
                      className={`h-full rounded-full transition-all duration-700 ${barColor} ${isGenerating ? "opacity-50" : ""}`}
                      style={{ width: `${pct}%` }}
                    />
                    {isGenerating && (
                      <div className="absolute inset-0 rounded-full bg-primary/20 animate-pulse" />
                    )}
                  </div>
                  {ratio > 0.95 && !isGenerating && (
                    <span className="text-3xs text-destructive font-medium whitespace-nowrap">
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
              className="bg-popover/95 border border-border/50 text-popover-foreground backdrop-blur-sm text-2xs font-mono max-w-60 whitespace-pre-line shadow-lg"
            >
              {tooltipLines.join("\n")}
            </TooltipContent>
          )}
        </Tooltip>

      </div>
    </TooltipProvider>
    </>
  );
}
