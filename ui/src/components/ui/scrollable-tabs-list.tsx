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
        // Cap at parent width and scroll horizontally. Triggers keep their
        // natural width (`flex-none`) instead of the base `flex-1` so the row
        // scrolls left-aligned as a group rather than stretch/shrink-clipping.
        "max-w-full justify-start flex-nowrap overflow-x-auto scrollbar-none scroll-fade-x",
        "[&>[data-slot=tabs-trigger]]:flex-none",
        className,
      )}
      {...props}
    />
  )
}

export { ScrollableTabsList }
