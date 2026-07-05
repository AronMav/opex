"use client";

import { Brain, Eye, Wrench } from "lucide-react";
import type { ProviderModel } from "@/lib/queries";

/** Human context-window size: 1048576 → "1M", 262144 → "256K". */
function fmtContext(n?: number): string | null {
  if (!n || n <= 0) return null;
  if (n >= 1_000_000) {
    const m = n / 1_048_576;
    return `${m >= 10 || Number.isInteger(m) ? Math.round(m) : m.toFixed(1)}M`;
  }
  if (n >= 1000) return `${Math.round(n / 1024)}K`;
  return String(n);
}

type Meta = Pick<ProviderModel, "context_window" | "vision" | "reasoning" | "tools">;

/** Compact catalog-metadata badges for a model: context window + capability
 *  icons (vision / reasoning / tools). Renders nothing when unknown. */
export function ModelBadges({ m, className = "" }: { m: Meta; className?: string }) {
  const ctx = fmtContext(m.context_window);
  if (!ctx && !m.vision && !m.reasoning && !m.tools) return null;
  return (
    <span className={`flex items-center gap-1.5 text-2xs text-muted-foreground-subtle ${className}`}>
      {ctx && <span className="tabular-nums" title="context window">{ctx}</span>}
      {m.vision && <Eye className="h-3 w-3" aria-label="vision" />}
      {m.reasoning && <Brain className="h-3 w-3" aria-label="reasoning" />}
      {m.tools && <Wrench className="h-3 w-3" aria-label="tools" />}
    </span>
  );
}
