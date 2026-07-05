import * as React from "react"
import { cn } from "@/lib/utils"

// `scroll` — the default page shell: fills the main area and scrolls its own
// overflow with the standard responsive padding.
// `fill` — a full-height shell that does NOT scroll itself; its children own
// their scroll regions (tabbed / split / editor pages like chat, workspace).
const PAGE_VARIANT = {
  scroll: "flex-1 overflow-y-auto p-4 md:p-6 lg:p-8 selection:bg-primary/20",
  fill: "flex flex-col h-full min-h-0 overflow-hidden",
} as const

/**
 * Single source of truth for a page's outer container sizing/overflow. Replaces
 * the copy-pasted `flex-1 overflow-y-auto p-4 md:p-6 lg:p-8` string so page
 * shells can't drift.
 */
function PageContainer({
  variant = "scroll",
  className,
  ...props
}: React.ComponentProps<"div"> & {
  variant?: keyof typeof PAGE_VARIANT
}) {
  return (
    <div
      data-slot="page-container"
      className={cn(PAGE_VARIANT[variant], className)}
      {...props}
    />
  )
}

export { PageContainer }
