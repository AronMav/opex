"use client"

import * as React from "react"
import { AlertTriangle } from "lucide-react"
import { cn } from "@/lib/utils"

function ErrorState({
  message,
  action,
  icon,
  className,
}: {
  message: React.ReactNode
  action?: React.ReactNode
  icon?: React.ReactNode
  className?: string
}) {
  return (
    <div className={cn("flex flex-1 flex-col items-center justify-center gap-4 p-8 text-center", className)}>
      <span className="text-muted-foreground-subtle [&_svg]:size-8">
        {icon ?? <AlertTriangle />}
      </span>
      <p className="max-w-lg break-words font-mono text-sm text-destructive">{message}</p>
      {action}
    </div>
  )
}

export { ErrorState }
