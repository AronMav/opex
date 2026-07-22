"use client"

import { cn } from "@/lib/utils"

export interface LoaderProps {
  variant?: "circular" | "pulse-dot"
  size?: "sm" | "md" | "lg"
  className?: string
}

export function CircularLoader({
  className,
  size = "md",
}: {
  className?: string
  size?: "sm" | "md" | "lg"
}) {
  const sizeClasses = {
    sm: "size-4",
    md: "size-5",
    lg: "size-6",
  }

  return (
    <div
      className={cn(
        "border-primary animate-spin rounded-full border-2 border-t-transparent",
        sizeClasses[size],
        className
      )}
    >
      <span className="sr-only">Loading</span>
    </div>
  )
}

export function PulseDotLoader({
  className,
  size = "md",
}: {
  className?: string
  size?: "sm" | "md" | "lg"
}) {
  const sizeClasses = {
    sm: "size-1",
    md: "size-2",
    lg: "size-3",
  }

  return (
    <div
      className={cn(
        "bg-primary animate-[pulse-dot_1.2s_ease-in-out_infinite] rounded-full",
        sizeClasses[size],
        className
      )}
    >
      <span className="sr-only">Loading</span>
    </div>
  )
}

/** Unified thinking indicator — a horizontal wave with a sweeping gradient pulse.
 *  Centered, stretched horizontally, with a soft glow. */
export function ThinkingWave({ className }: { className?: string }) {
  return (
    <div
      className={cn("flex items-center justify-center", className)}
      role="status"
      aria-live="polite"
    >
      <div
        className="relative h-[6px] w-40 overflow-hidden rounded-full"
        style={{
          background: "color-mix(in srgb, var(--primary) 20%, transparent)",
        }}
      >
        {/* Breathing track */}
        <div
          className="absolute inset-0 rounded-full"
          style={{
            background:
              "linear-gradient(90deg, transparent, color-mix(in srgb, var(--primary) 40%, transparent), transparent)",
            animation: "thinking-wave-breathe 2s ease-in-out infinite",
          }}
        />
        {/* Sweeping pulse */}
        <div
          className="absolute inset-y-0 w-1/2 rounded-full"
          style={{
            background:
              "linear-gradient(90deg, transparent 0%, var(--primary) 50%, transparent 100%)",
            boxShadow: `0 0 12px var(--primary)`,
            animation: "thinking-wave-sweep 1.6s ease-in-out infinite",
          }}
        />
      </div>
      <span className="sr-only">Thinking</span>
    </div>
  )
}

/** Inline blinking caret shown at the end of actively streaming text.
 *  Deliberately NOT the ThinkingWave: the wave means "thinking", the caret
 *  means "text is arriving right here". */
export function StreamingCaret() {
  return (
    <span
      data-testid="streaming-cursor"
      aria-hidden="true"
      className="ml-0.5 inline-block h-[1em] w-[2px] translate-y-[0.125em] rounded-sm bg-primary/70 animate-pulse"
    />
  )
}

/** Quiet placeholder for an assistant part that exists but has no content yet. */
export function PartSkeleton() {
  return (
    <div data-testid="part-skeleton" className="py-1">
      <span className="sr-only">Loading</span>
      <div className="h-3 w-24 animate-pulse rounded bg-muted/50" />
    </div>
  )
}

function Loader({ variant = "circular", size = "md", className }: LoaderProps) {
  if (variant === "pulse-dot") {
    return <PulseDotLoader size={size} className={className} />
  }
  return <CircularLoader size={size} className={className} />
}

export { Loader }