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
      className={cn("max-w-full justify-start overflow-x-auto scrollbar-none", className)}
      {...props}
    />
  )
}

export { ScrollableTabsList }
