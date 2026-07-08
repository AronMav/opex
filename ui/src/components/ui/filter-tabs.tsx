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
 * Renders a uniform trigger per item: `[icon] [label] [count badge]` in a single
 * row. When the full-label row would exceed the available width, inactive tabs
 * COLLAPSE to their icon (the active tab keeps its label) so every tab stays
 * reachable in one row instead of wrapping or clipping off-screen. The collapse
 * is measured with a ResizeObserver against the actual container width, so it
 * adapts to any tab count without a hand-tuned breakpoint.
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
  const wrapRef = React.useRef<HTMLDivElement>(null)
  // Width (px) the row needs with ALL labels shown. Measured while expanded and
  // used as the stable threshold for re-expanding — prevents collapse↔expand
  // oscillation (collapsing hides labels, which would otherwise shrink the
  // measured width and immediately re-expand).
  const fullWidthRef = React.useRef(0)
  const [compact, setCompact] = React.useState(false)

  React.useLayoutEffect(() => {
    const wrap = wrapRef.current
    if (!wrap) return
    // Measure the LIST element (it owns the overflow-x-auto that would otherwise
    // absorb the overflow and hide it from the wrapper's own scrollWidth).
    const list = wrap.querySelector<HTMLElement>('[data-slot="tabs-list"]')
    if (!list) return
    const measure = () => {
      // Available width = the block wrapper (always the full row width). Do NOT
      // use the list's own clientWidth: TabsList is `w-fit`, so once collapsed it
      // shrinks to the icons' width (~300px) and would never report enough room
      // to expand again. `list.scrollWidth` is the width the labels NEED.
      const avail = wrap.clientWidth
      if (!compact) {
        fullWidthRef.current = list.scrollWidth
        if (list.scrollWidth > avail + 1) setCompact(true)
      } else if (fullWidthRef.current > 0 && avail >= fullWidthRef.current) {
        setCompact(false)
      }
    }
    measure()
    // No ResizeObserver (jsdom/tests, very old browsers) → measure once, keep
    // labels shown. The page stays functional; only the adaptive collapse is off.
    if (typeof ResizeObserver === "undefined") return
    const ro = new ResizeObserver(measure)
    ro.observe(wrap)
    return () => ro.disconnect()
  }, [compact, items])

  return (
    <div ref={wrapRef} className="min-w-0">
      <ScrollableTabsList className={cn("h-9", className)} {...props}>
        {items.map((item) => (
          <TabsTrigger
            key={item.value}
            value={item.value}
            aria-label={item.label}
            title={item.label}
            className="group/ftab text-xs font-medium"
          >
            <span className="shrink-0 [&_svg]:size-4">{item.icon}</span>
            <span
              className={cn(
                "truncate",
                // Roomy: every label visible. Tight: only the active tab's label.
                compact ? "hidden group-data-[state=active]/ftab:inline" : "inline",
              )}
            >
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
    </div>
  )
}

export { FilterTabsList }
