"use client"

import * as React from "react"
import { Check } from "lucide-react"
import { cn } from "@/lib/utils"

interface StepperProps {
  steps: ReadonlyArray<{
    key: string
    icon?: React.ComponentType<{ className?: string }>
  }>
  currentIndex: number
  className?: string
}

function Stepper({ steps, currentIndex, className }: StepperProps) {
  return (
    <div className={cn("flex items-center justify-center gap-2", className)}>
      {steps.map((s, i) => {
        const done = i < currentIndex
        const active = i === currentIndex
        const Icon = s.icon
        return (
          <div key={s.key} className="flex items-center gap-2">
            {i > 0 && <div className={cn("h-px w-8", done ? "bg-primary" : "bg-border")} />}
            <div
              aria-current={active ? "step" : undefined}
              className={cn(
                "flex h-9 w-9 items-center justify-center rounded-full border-2 transition-all",
                done
                  ? "border-primary bg-primary text-primary-foreground"
                  : active
                    ? "border-primary bg-primary/10 text-primary"
                    : "border-border text-muted-foreground",
              )}
            >
              {done ? (
                <Check className="h-4 w-4" />
              ) : Icon ? (
                <Icon className="h-4 w-4" />
              ) : (
                <span className="text-sm">{i + 1}</span>
              )}
            </div>
          </div>
        )
      })}
    </div>
  )
}

export { Stepper }
