"use client"

import * as React from "react"
import { Search, X } from "lucide-react"
import { cn } from "@/lib/utils"
import { Input } from "@/components/ui/input"
import { Button } from "@/components/ui/button"

function SearchInput({
  value,
  onChange,
  placeholder,
  className,
  debounceMs,
}: {
  value: string
  onChange: (v: string) => void
  placeholder?: string
  className?: string
  debounceMs?: number
}) {
  const [local, setLocal] = React.useState(value)
  const timer = React.useRef<ReturnType<typeof setTimeout> | null>(null)

  React.useEffect(
    () => () => {
      if (timer.current) clearTimeout(timer.current)
    },
    [],
  )

  const emit = (v: string) => {
    setLocal(v)
    if (!debounceMs) {
      onChange(v)
      return
    }
    if (timer.current) clearTimeout(timer.current)
    timer.current = setTimeout(() => onChange(v), debounceMs)
  }

  const clear = () => {
    if (timer.current) clearTimeout(timer.current)
    setLocal("")
    onChange("")
  }

  return (
    <div className={cn("relative", className)}>
      <Search
        className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground-subtle"
        aria-hidden="true"
      />
      <Input
        value={local}
        onChange={(e) => emit(e.target.value)}
        placeholder={placeholder}
        aria-label={placeholder}
        className="pl-9 pr-9"
      />
      {local && (
        <Button
          type="button"
          variant="ghost"
          size="icon-xs"
          onClick={clear}
          aria-label="Clear search"
          className="absolute right-1.5 top-1/2 -translate-y-1/2"
        >
          <X />
        </Button>
      )}
    </div>
  )
}

export { SearchInput }
