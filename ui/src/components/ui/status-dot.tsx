import * as React from "react"
import { cn } from "@/lib/utils"

const STATUS_DOT = {
  success: "bg-success",
  error: "bg-destructive",
  warning: "bg-warning",
  muted: "bg-muted-foreground/40",
} as const

function StatusDot({
  status = "muted",
  pulse = false,
  className,
}: {
  status?: keyof typeof STATUS_DOT
  pulse?: boolean
  className?: string
}) {
  return (
    <span
      aria-hidden="true"
      className={cn(
        "inline-block h-2 w-2 shrink-0 rounded-full",
        STATUS_DOT[status],
        pulse && "animate-pulse",
        className
      )}
    />
  )
}

export { StatusDot }
