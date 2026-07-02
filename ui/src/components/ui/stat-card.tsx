import * as React from "react"
import type { LucideIcon } from "lucide-react"
import { cn } from "@/lib/utils"
import { Card } from "@/components/ui/card"

const ACCENT = {
  1: "text-chart-1",
  2: "text-chart-2",
  3: "text-chart-3",
  4: "text-chart-4",
  5: "text-chart-5",
  6: "text-chart-6",
  7: "text-chart-7",
  8: "text-chart-8",
} as const

function StatCard({
  label,
  value,
  sub,
  icon: Icon,
  accent,
  className,
}: {
  label: React.ReactNode
  value: React.ReactNode
  sub?: React.ReactNode
  icon?: LucideIcon
  accent?: keyof typeof ACCENT
  className?: string
}) {
  return (
    <Card className={cn("relative overflow-hidden p-4", className)}>
      <div className="flex items-center justify-between gap-2">
        <span className="text-2xs font-semibold uppercase tracking-wide text-muted-foreground-subtle">
          {label}
        </span>
        {Icon && (
          <Icon
            className={cn("size-4 shrink-0", accent ? ACCENT[accent] : "text-muted-foreground-subtle")}
            aria-hidden="true"
          />
        )}
      </div>
      <div className={cn("mt-1 font-display text-2xl font-bold tabular-nums", accent && ACCENT[accent])}>
        {value}
      </div>
      {sub && <div className="mt-0.5 text-xs text-muted-foreground-subtle">{sub}</div>}
    </Card>
  )
}

export { StatCard }
