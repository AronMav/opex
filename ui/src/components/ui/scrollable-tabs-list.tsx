"use client"

import * as React from "react"
import { cn } from "@/lib/utils"
import { TabsList } from "@/components/ui/tabs"

function ScrollableTabsList({
  className,
  ...props
}: React.ComponentProps<typeof TabsList>) {
  return (
    <TabsList
      className={cn(
        // WRAP, don't clip: when the triggers exceed the available width they
        // flow onto a second row so every tab stays visible (horizontal scroll
        // hid tabs off-screen with no desktop affordance). Triggers keep their
        // natural width (`flex-none` instead of the base `flex-1`) and the list
        // grows in height. `h-auto` overrides the base fixed `h-9`.
        "h-auto max-w-full flex-wrap justify-start gap-1",
        "[&>[data-slot=tabs-trigger]]:flex-none",
        className,
      )}
      {...props}
    />
  )
}

export { ScrollableTabsList }
