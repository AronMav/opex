import * as React from "react"
import type { LucideIcon } from "lucide-react"
import { cn } from "@/lib/utils"

function SectionHeader({
  title,
  description,
  icon: Icon,
  actions,
  count,
  className,
}: {
  title: React.ReactNode
  description?: React.ReactNode
  icon?: LucideIcon
  actions?: React.ReactNode
  count?: number
  className?: string
}) {
  return (
    <div className={cn("mb-4 flex items-center gap-2", className)}>
      {Icon && <Icon className="size-4 text-muted-foreground" aria-hidden="true" />}
      <div className="flex min-w-0 flex-col">
        <div className="flex items-center gap-2">
          <h2 className="font-display text-sm font-bold tracking-tight text-foreground">
            {title}
          </h2>
          {typeof count === "number" && (
            <span className="rounded-full bg-muted px-1.5 text-2xs font-semibold text-muted-foreground-subtle tabular-nums">
              {count}
            </span>
          )}
        </div>
        {description && (
          <p className="text-xs text-muted-foreground-subtle">{description}</p>
        )}
      </div>
      {actions && (
        <div className="ml-auto flex items-center gap-2">{actions}</div>
      )}
    </div>
  )
}
export { SectionHeader }
