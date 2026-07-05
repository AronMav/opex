"use client"

import * as React from "react"
import { cn } from "@/lib/utils"
import { TabsTrigger } from "@/components/ui/tabs"
import { ScrollableTabsList } from "@/components/ui/scrollable-tabs-list"
import { Badge } from "@/components/ui/badge"

export type FilterTabItem = {
  /** Tab value — must match the matching <TabsContent value> on the page. */
  value: string
  /** Plain-text label; also used as the trigger's aria-label. */
  label: string
  /** Required — every tab shows an icon. */
  icon: React.ReactNode
  /** Optional count → renders a Badge next to the label. */
  count?: number
}

/**
 * Single source of truth for page-level filter/tab bars.
 *
 * Renders a uniform trigger per item: `[icon] [label] [count badge]`, wrapped in
 * the scrollable tab list. On phones (< sm) the label collapses to an icon for
 * inactive tabs and is shown only for the active tab; from sm up every label is
 * visible. The count badge stays visible on all breakpoints.
 *
 * Keep `<Tabs>` and `<TabsContent>` on the page — this replaces only the trigger list.
 */
function FilterTabsList({
  items,
  className,
  ...props
}: {
  items: FilterTabItem[]
  className?: string
} & Omit<React.ComponentProps<typeof ScrollableTabsList>, "children">) {
  return (
    <ScrollableTabsList className={cn("h-9", className)} {...props}>
      {items.map((item) => (
        <TabsTrigger
          key={item.value}
          value={item.value}
          aria-label={item.label}
          className="group/ftab text-xs font-medium"
        >
          <span className="shrink-0 [&_svg]:size-4">{item.icon}</span>
          <span className="hidden sm:inline group-data-[state=active]/ftab:inline">
            {item.label}
          </span>
          {item.count != null && (
            <Badge variant="secondary" size="xs" className="ml-1.5 tabular-nums">
              {item.count}
            </Badge>
          )}
        </TabsTrigger>
      ))}
    </ScrollableTabsList>
  )
}

export { FilterTabsList }
