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

const HOLO_BARS: { height: number; color: string; delay: string }[] = [
  { height: 10, color: "var(--chart-1)", delay: "0s" },
  { height: 18, color: "var(--chart-5)", delay: "0.18s" },
  { height: 13, color: "var(--chart-4)", delay: "0.36s" },
]

export function CometLoader({ className }: { className?: string }) {
  return (
    <div className={cn("flex items-end gap-[3px]", className)} style={{ height: 20 }}>
      {HOLO_BARS.map((bar, i) => (
        <div
          key={i}
          style={{
            width: 4,
            height: bar.height,
            borderRadius: 2,
            background: bar.color,
            boxShadow: `0 0 6px ${bar.color}`,
            animation: `holo-wave 1.2s ease-in-out infinite`,
            animationDelay: bar.delay,
            transformOrigin: "bottom",
          }}
        />
      ))}
      <span className="sr-only">Loading</span>
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