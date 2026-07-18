"use client"

import * as React from "react"
import { cn } from "@/lib/utils"

export type DialogTabItem<T extends string> = {
  value: T
  label: string
  icon: React.ComponentType<{ className?: string }>
}

/**
 * Single source of truth for in-dialog "folder" tab bars (agent edit, provider
 * edit, …). Renders only the tab strip — each dialog keeps its own content
 * switching. Icon-only on phones for inactive tabs; the active tab always shows
 * its label. Defaults to the dialog header bleed (`-mx-5 px-5`); override via
 * `className`.
 */
function DialogTabs<T extends string>({
  items,
  value,
  onChange,
  className,
}: {
  items: ReadonlyArray<DialogTabItem<T>>
  value: T
  onChange: (v: T) => void
  className?: string
}) {
  // With many tabs (e.g. the 8-tab agent editor) the label strip overflows the
  // dialog width and the last tab gets clipped. Past a threshold, show the label
  // only for the ACTIVE tab and render inactive tabs icon-only (the mobile
  // pattern, extended to all widths) so every tab stays reachable without a
  // horizontal scroll. `title` still surfaces each label on hover. Few-tab
  // dialogs (provider editor, …) keep their labels unchanged.
  const iconOnlyInactive = items.length > 6
  return (
    <div
      role="tablist"
      className={cn("flex gap-0.5 overflow-x-auto scrollbar-none -mx-5 px-5 pb-0", className)}
    >
      {items.map((item) => {
        const Icon = item.icon
        const isActive = item.value === value
        return (
          <button
            key={item.value}
            type="button"
            role="tab"
            aria-selected={isActive}
            onClick={() => onChange(item.value)}
            title={item.label}
            className={cn(
              "relative flex items-center gap-1.5 px-2.5 sm:px-3 py-2 text-xs font-medium whitespace-nowrap transition-colors rounded-t-md shrink-0 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset",
              isActive
                ? "text-foreground bg-background border-t border-l border-r border-border"
                : "text-muted-foreground hover:text-foreground hover:bg-muted/30",
            )}
          >
            <Icon className="h-3.5 w-3.5 shrink-0 sm:h-3 sm:w-3" />
            <span className={isActive ? "inline" : iconOnlyInactive ? "hidden" : "hidden sm:inline"}>{item.label}</span>
            {isActive && <span className="absolute bottom-0 left-0 right-0 h-px bg-background" />}
          </button>
        )
      })}
    </div>
  )
}

export { DialogTabs }
