import * as React from "react"
import { cn } from "@/lib/utils"
import { Card } from "@/components/ui/card"

function DataRow({
  leading,
  title,
  subtitle,
  children,
  actions,
  interactive = true,
  muted = false,
  className,
}: {
  leading?: React.ReactNode
  title?: React.ReactNode
  subtitle?: React.ReactNode
  children?: React.ReactNode
  actions?: React.ReactNode
  interactive?: boolean
  muted?: boolean
  className?: string
}) {
  return (
    <Card
      interactive={interactive}
      className={cn(
        "group relative flex flex-col md:flex-row md:items-center gap-4 p-4",
        muted && "opacity-60 hover:opacity-100",
        className,
      )}
    >
      {(leading || title || subtitle) && (
        <div className="flex items-center gap-3 min-w-0 md:min-w-[13.75rem]">
          {leading}
          {(title || subtitle) && (
            <div className="flex flex-col min-w-0">
              {title && (
                <span className="font-mono text-sm font-bold text-foreground truncate">
                  {title}
                </span>
              )}
              {subtitle && (
                <span className="font-mono text-xs text-muted-foreground-subtle tabular-nums">
                  {subtitle}
                </span>
              )}
            </div>
          )}
        </div>
      )}
      {children && <div className="flex flex-1 flex-col gap-2 min-w-0">{children}</div>}
      {actions && (
        <div className="flex items-center gap-2 border-t border-border/50 pt-3 md:border-0 md:pt-0 shrink-0">
          {actions}
        </div>
      )}
    </Card>
  )
}

export { DataRow }
