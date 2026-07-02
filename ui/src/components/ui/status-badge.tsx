import * as React from "react"
import { Badge } from "@/components/ui/badge"

type BadgeVariant =
  | "success"
  | "warning"
  | "destructive"
  | "secondary"
  | "outline"

// Semantic status → Badge variant. Extend as new statuses appear across pages.
const STATUS_VARIANT: Record<string, BadgeVariant> = {
  online: "success",
  active: "success",
  enabled: "success",
  connected: "success",
  ok: "success",
  running: "success",
  stale: "warning",
  expired: "warning",
  warn: "warning",
  pending: "warning",
  error: "destructive",
  failed: "destructive",
  interrupted: "destructive",
  offline: "secondary",
  disabled: "secondary",
  paused: "secondary",
  archived: "secondary",
}

function StatusBadge({
  status,
  children,
  size = "default",
  className,
}: {
  status: string
  children?: React.ReactNode
  size?: "default" | "sm"
  className?: string
}) {
  const variant = STATUS_VARIANT[status] ?? "secondary"
  return (
    <Badge variant={variant} size={size} className={className}>
      {children ?? status}
    </Badge>
  )
}

export { StatusBadge, STATUS_VARIANT }
