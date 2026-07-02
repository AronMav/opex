"use client"

import * as React from "react"
import { Check, Copy } from "lucide-react"
import { copyText } from "@/lib/clipboard"
import { cn } from "@/lib/utils"
import { Button } from "@/components/ui/button"

function CopyableCode({
  value,
  display,
  className,
  onCopied,
}: {
  value: string
  display?: React.ReactNode
  className?: string
  onCopied?: () => void
}) {
  const [copied, setCopied] = React.useState(false)
  const timer = React.useRef<ReturnType<typeof setTimeout> | null>(null)

  React.useEffect(
    () => () => {
      if (timer.current) clearTimeout(timer.current)
    },
    [],
  )

  const copy = () => {
    copyText(value).then(() => {
      setCopied(true)
      onCopied?.()
      if (timer.current) clearTimeout(timer.current)
      timer.current = setTimeout(() => setCopied(false), 2000)
    })
  }

  return (
    <div
      className={cn(
        "flex items-center gap-2 rounded-lg bg-muted/30 border border-border/50 px-3 py-2",
        className,
      )}
    >
      <code className="flex-1 text-xs font-mono text-primary/80 break-all select-all">
        {display ?? value}
      </code>
      <Button
        type="button"
        variant="ghost"
        size="icon-xs"
        onClick={copy}
        aria-label={copied ? "Copied" : "Copy"}
      >
        {copied ? <Check className="text-success" /> : <Copy />}
      </Button>
    </div>
  )
}

export { CopyableCode }
