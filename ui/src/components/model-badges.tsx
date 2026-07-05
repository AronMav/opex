"use client";

import { Brain, Eye, Wrench } from "lucide-react";
import type { ProviderModel } from "@/lib/queries";

/** Human context-window size (decimal, base-1000 for consistency):
 *  1048576 → "1M", 262144 → "262K", 128000 → "128K". */
function fmtContext(n?: number): string | null {
  if (!n || n <= 0) return null;
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1).replace(/\.0$/, "")}M`;
  return `${Math.round(n / 1000)}K`;
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
