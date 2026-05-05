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
      className="my-3 flex items-center gap-2 text-muted-foreground/60 select-none"
    >
      <div className="h-px flex-1 bg-border" />
      <span className="text-[10px] uppercase tracking-wider text-muted-foreground/60">
        next step
      </span>
      <div className="h-px flex-1 bg-border" />
    </div>
  );
}
