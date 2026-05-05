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
      className="my-3 h-px bg-border/60 select-none"
    />
  );
}
