"use client"

import * as React from "react"
import { cn } from "@/lib/utils"
import { TabsList } from "@/components/ui/tabs"

function ScrollableTabsList({
  className,
  ...props
}: React.ComponentProps<typeof TabsList>) {
  return (
    <div className="relative max-w-full">
      <TabsList
        className={cn("max-w-full justify-start overflow-x-auto scrollbar-none", className)}
        {...props}
      />
      <div
        aria-hidden="true"
        className="pointer-events-none absolute inset-y-0 right-0 w-6 bg-gradient-to-l from-muted to-transparent sm:hidden"
      />
    </div>
  )
}

export { ScrollableTabsList }
