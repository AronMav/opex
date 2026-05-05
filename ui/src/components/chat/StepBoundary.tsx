"use client";

/**
 * Visual divider between two LLM tool-loop iterations within one assistant turn.
 *
 * Replaces the old "merge everything + heuristic text dedup" approach: when a
 * model repeats its intro narration on every iteration, those repetitions are
 * no longer rendered as confusing duplicates — each lives in its own slice
 * separated by this boundary.
 */
export function StepBoundary() {
  return (
    <div
      role="separator"
      aria-label="Step boundary"
      className="my-2 flex items-center gap-2 text-muted-foreground/30 select-none"
    >
      <div className="h-px flex-1 bg-border/40" />
      <span className="h-1 w-1 rounded-full bg-muted-foreground/30" />
      <div className="h-px flex-1 bg-border/40" />
    </div>
  );
}
