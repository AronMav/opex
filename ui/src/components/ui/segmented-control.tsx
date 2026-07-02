"use client"

import * as React from "react"
import { cn } from "@/lib/utils"

function SegmentedControl<T extends string>({
  value,
  onChange,
  options,
  className,
}: {
  value: T
  onChange: (v: T) => void
  options: ReadonlyArray<{ value: T; label: React.ReactNode }>
  className?: string
}) {
  return (
    <div
      role="radiogroup"
      className={cn("inline-flex gap-0.5 rounded-md border border-border bg-muted/40 p-0.5", className)}
    >
      {options.map((o) => {
        const active = o.value === value
        return (
          <button
            key={o.value}
            type="button"
            role="radio"
            aria-checked={active}
            onClick={() => onChange(o.value)}
            className={cn(
              "rounded-sm px-2.5 py-1 text-xs font-medium transition-colors",
              active
                ? "bg-primary text-primary-foreground shadow-sm"
                : "text-muted-foreground hover:text-foreground",
            )}
          >
            {o.label}
          </button>
        )
      })}
    </div>
  )
}

export { SegmentedControl }
