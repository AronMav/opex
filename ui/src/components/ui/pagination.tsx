import * as React from "react"
import { ChevronLeft, ChevronRight } from "lucide-react"
import { cn } from "@/lib/utils"
import { Button } from "@/components/ui/button"

function Pagination({
  page,
  total,
  onPrev,
  onNext,
  className,
  label,
}: {
  page: number
  total: number
  onPrev: () => void
  onNext: () => void
  className?: string
  label?: React.ReactNode
}) {
  return (
    <div className={cn("flex items-center justify-center gap-3", className)}>
      <Button
        variant="outline"
        size="sm"
        onClick={onPrev}
        disabled={page <= 1}
        aria-label="Previous page"
      >
        <ChevronLeft />
      </Button>
      <span className="text-xs tabular-nums text-muted-foreground-subtle">
        {label ?? `${page} / ${total}`}
      </span>
      <Button
        variant="outline"
        size="sm"
        onClick={onNext}
        disabled={page >= total}
        aria-label="Next page"
      >
        <ChevronRight />
      </Button>
    </div>
  )
}

export { Pagination }
