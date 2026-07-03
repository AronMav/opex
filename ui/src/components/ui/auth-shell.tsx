"use client"

import * as React from "react"
import { Nut } from "lucide-react"
import { cn } from "@/lib/utils"
import { IconTile } from "@/components/ui/icon-tile"

function AuthShell({
  children,
  glow = false,
  className,
}: {
  children: React.ReactNode
  glow?: boolean
  className?: string
}) {
  return (
    <div className="relative flex min-h-dvh w-full items-start justify-center overflow-y-auto bg-background p-4 py-8 selection:bg-primary/30 sm:items-center">
      {glow && (
        <div aria-hidden="true" className="pointer-events-none absolute inset-0 overflow-hidden">
          <div className="absolute left-1/2 top-1/2 h-[600px] w-[600px] -translate-x-1/2 -translate-y-1/2 rounded-full bg-primary/5 opacity-50 blur-[120px]" />
        </div>
      )}
      <div className={cn("relative z-10 w-full", className)}>{children}</div>
    </div>
  )
}

function AuthBrand({
  orientation = "vertical",
  subtitle,
  className,
}: {
  orientation?: "vertical" | "horizontal"
  subtitle?: React.ReactNode
  className?: string
}) {
  if (orientation === "horizontal") {
    return (
      <div className={cn("flex items-center justify-center gap-3", className)}>
        <Nut className="h-8 w-8 text-primary" />
        <span className="font-display text-2xl font-bold tracking-wide">OPEX</span>
      </div>
    )
  }
  return (
    <div className={cn("flex flex-col items-center gap-3", className)}>
      <IconTile tone="primary" size="lg" className="rounded-2xl">
        <Nut />
      </IconTile>
      <div className="flex flex-col items-center">
        <h1 className="font-display text-2xl font-bold tracking-wide text-foreground">OPEX</h1>
        {subtitle && <span className="mt-1 text-sm text-muted-foreground">{subtitle}</span>}
      </div>
    </div>
  )
}

export { AuthShell, AuthBrand }
