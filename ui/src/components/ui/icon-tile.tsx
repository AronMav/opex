import * as React from "react"
import { cva, type VariantProps } from "class-variance-authority"
import { cn } from "@/lib/utils"

const iconTileVariants = cva(
  "inline-flex shrink-0 items-center justify-center border [&_svg]:shrink-0",
  {
    variants: {
      tone: {
        primary: "bg-primary/10 border-primary/20 text-primary",
        muted: "bg-muted/50 border-border text-muted-foreground",
        success: "bg-success/10 border-success/20 text-success",
        warning: "bg-warning/10 border-warning/20 text-warning",
        destructive: "bg-destructive/10 border-destructive/20 text-destructive",
      },
      size: {
        sm: "h-8 w-8 [&_svg]:size-4",
        md: "h-10 w-10 [&_svg]:size-5",
        lg: "h-11 w-11 [&_svg]:size-5",
      },
      shape: {
        square: "rounded-lg",
        round: "rounded-full",
      },
    },
    defaultVariants: { tone: "primary", size: "md", shape: "square" },
  }
)

function IconTile({
  className,
  tone,
  size,
  shape,
  ...props
}: React.ComponentProps<"div"> & VariantProps<typeof iconTileVariants>) {
  return (
    <div
      data-slot="icon-tile"
      className={cn(iconTileVariants({ tone, size, shape }), className)}
      {...props}
    />
  )
}

export { IconTile, iconTileVariants }
