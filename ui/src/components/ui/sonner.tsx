"use client"

import { useEffect, useRef } from "react"
import {
  CircleCheckIcon,
  InfoIcon,
  Loader2Icon,
  OctagonXIcon,
  TriangleAlertIcon,
} from "lucide-react"
import { useTheme } from "next-themes"
import { toast, Toaster as Sonner, type ToasterProps } from "sonner"

const Toaster = ({ ...props }: ToasterProps) => {
  const { theme = "system" } = useTheme()
  const ref = useRef<HTMLElement>(null)

  // Click-to-dismiss: sonner v2 has no built-in body onClick, so delegate at
  // the toaster root. Map the clicked toast's DOM position to its id via
  // getToasts() (rendered order == state order) and dismiss it. Clicks on the
  // close button / action button / links are left alone (they keep their own
  // behaviour).
  useEffect(() => {
    const root = ref.current
    if (!root) return
    const handler = (e: MouseEvent) => {
      const target = e.target as HTMLElement | null
      if (!target) return
      if (target.closest("[data-close-button], [data-button], [data-action], a, button")) return
      const li = target.closest<HTMLElement>("[data-sonner-toast]")
      if (!li || li.dataset.dismissible === "false" || li.dataset.disabled === "true") return
      const items = Array.from(root.querySelectorAll<HTMLElement>("[data-sonner-toast]"))
      const idx = items.indexOf(li)
      if (idx < 0) return
      const id = toast.getToasts()[idx]?.id
      if (id !== undefined) toast.dismiss(id)
    }
    root.addEventListener("click", handler)
    return () => root.removeEventListener("click", handler)
  }, [])

  return (
    <Sonner
      ref={ref}
      theme={theme as ToasterProps["theme"]}
      className="toaster group"
      icons={{
        success: <CircleCheckIcon className="size-4" />,
        info: <InfoIcon className="size-4" />,
        warning: <TriangleAlertIcon className="size-4" />,
        error: <OctagonXIcon className="size-4" />,
        loading: <Loader2Icon className="size-4 animate-spin" />,
      }}
      style={
        {
          "--normal-bg": "var(--popover)",
          "--normal-text": "var(--popover-foreground)",
          "--normal-border": "var(--border)",
          "--border-radius": "var(--radius)",
        } as React.CSSProperties
      }
      {...props}
    />
  )
}

export { Toaster }
