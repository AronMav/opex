"use client"

import * as React from "react"
import { cn } from "@/lib/utils"

interface PageHeaderProps {
  title: string
  description?: React.ReactNode
  actions?: React.ReactNode
  className?: string
}

export function PageHeader({ title, description, actions, className }: PageHeaderProps) {
  return (
    <div className={cn("mb-8 flex flex-col gap-2 md:flex-row md:items-center md:justify-between", className)}>
      <div className="flex flex-col gap-1">
        <h1 className="font-display text-lg font-bold tracking-tight text-foreground">
          {title}
        </h1>
        {description && (
          <p className="text-sm text-muted-foreground">{description}</p>
        )}
      </div>
      {actions && <div className="flex flex-wrap items-center gap-2 min-w-0">{actions}</div>}
    </div>
  )
}