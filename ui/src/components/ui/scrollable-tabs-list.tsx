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
        // One row, left-aligned. Triggers keep their natural width (`flex-none`
        // vs the base `flex-1`). FilterTabsList collapses inactive labels to
        // icons (via ResizeObserver) before this ever needs to scroll; the
        // overflow-x-auto is only a last-resort fallback for extreme cases.
        "max-w-full justify-start flex-nowrap overflow-x-auto scrollbar-none",
        "[&>[data-slot=tabs-trigger]]:flex-none",
        className,
      )}
      {...props}
    />
  )
}

export { ScrollableTabsList }
