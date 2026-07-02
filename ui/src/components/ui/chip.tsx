import * as React from "react"
import { cva, type VariantProps } from "class-variance-authority"
import { cn } from "@/lib/utils"

const chipVariants = cva(
  "inline-flex items-center gap-1 rounded-md border px-2 py-0.5 text-xs font-medium [&>svg]:size-3 [&>svg]:shrink-0",
  {
    variants: {
      tone: {
        default: "border-border bg-muted/30 text-foreground/80",
        primary: "border-primary/20 bg-primary/5 text-primary",
      },
    },
    defaultVariants: { tone: "default" },
  }
)

function Chip({
  className,
  tone,
  ...props
}: React.ComponentProps<"span"> & VariantProps<typeof chipVariants>) {
  return <span data-slot="chip" className={cn(chipVariants({ tone }), className)} {...props} />
}

export { Chip, chipVariants }
