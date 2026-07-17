"use client"
import DOMPurify from "dompurify"
import { Maximize2, Minimize2, RotateCcw, ZoomIn, ZoomOut } from "lucide-react"
import { useCallback, useEffect, useState } from "react"
import { useTheme } from "next-themes"
import { useTranslation } from "@/hooks/use-translation"
import { Skeleton } from "@/components/ui/skeleton"
import { Button } from "@/components/ui/button";

const MIN_ZOOM = 0.5
const MAX_ZOOM = 3
const ZOOM_STEP = 0.25

export function MermaidBlock({ code }: { code: string }) {
  const { t } = useTranslation()
  const { resolvedTheme } = useTheme()
  const [svgHtml, setSvgHtml] = useState("")
  const [error, setError] = useState("")
  const [zoom, setZoom] = useState(1)
  const [expanded, setExpanded] = useState(false)

  useEffect(() => {
    let cancelled = false
    ;(async () => {
      try {
        const mermaid = (await import("mermaid")).default
        const isDark = resolvedTheme === "dark"
        mermaid.initialize({
          startOnLoad: false,
          theme: isDark ? "dark" : "neutral",
          securityLevel: "strict",
          flowchart: {
            htmlLabels: true,
            curve: "basis",
            nodeSpacing: 30,
            rankSpacing: 30,
            diagramPadding: 8,
            useMaxWidth: true,
            padding: 15,
          },
        })
        const id = `mermaid-${typeof crypto !== "undefined" && crypto.randomUUID ? crypto.randomUUID() : Math.random().toString(36).slice(2)}`
        const { svg } = await mermaid.render(id, code)
        // Sanitize with DOMPurify — allow foreignObject + style for htmlLabels
        const sanitized = DOMPurify.sanitize(svg, {
          USE_PROFILES: { svg: true, svgFilters: true },
          ADD_TAGS: ["foreignObject", "use", "switch", "style"],
          ADD_ATTR: [
            "dominant-baseline", "text-anchor", "xmlns:xlink",
            "requiredExtensions", "requiredFeatures",
          ],
        })
        if (!cancelled) setSvgHtml(sanitized)
      } catch (e) {
        if (!cancelled) setError(String(e))
      }
    })()
    return () => { cancelled = true }
  }, [code, resolvedTheme])

  const handleZoomIn = useCallback(() => setZoom(z => Math.min(z + ZOOM_STEP, MAX_ZOOM)), [])
  const handleZoomOut = useCallback(() => setZoom(z => Math.max(z - ZOOM_STEP, MIN_ZOOM)), [])
  const handleReset = useCallback(() => setZoom(1), [])
  const toggleExpand = useCallback(() => {
    setExpanded(e => !e)
    setZoom(1)
  }, [])

  const handleWheel = useCallback((e: React.WheelEvent) => {
    if (e.ctrlKey || e.metaKey) {
      e.preventDefault()
      const delta = e.deltaY > 0 ? -ZOOM_STEP : ZOOM_STEP
      setZoom(z => Math.min(Math.max(z + delta, MIN_ZOOM), MAX_ZOOM))
    }
  }, [])

  if (error) return <pre className="text-xs text-destructive">{error}</pre>
  if (!svgHtml) return <Skeleton className="h-24 w-full rounded" />

  const containerClass = expanded
    ? "fixed inset-4 z-[var(--z-modal)] flex flex-col rounded-lg border border-border bg-background shadow-2xl"
    : "relative my-4 rounded-md border border-border/50 bg-muted/20"

  return (
    <>
      {expanded && (
        // z-40 backdrop: local stacking, not layered UI
        <div className="fixed inset-0 z-40 bg-black/50" onClick={toggleExpand} />
      )}
      <div className={containerClass}>
        {/* Toolbar */}
        <div className="flex items-center justify-between border-b border-border/50 px-3 py-1.5">
          <span className="text-xs text-muted-foreground">Mermaid</span>
          <div className="flex items-center gap-1">
            <Button
              variant="ghost"
              size="icon-xs"
              onClick={handleZoomOut}
              disabled={zoom <= MIN_ZOOM}
              className="text-muted-foreground hover:text-foreground"
              title={t("common.zoom_out")}
            >
              <ZoomOut className="h-3.5 w-3.5" />
            </Button>
            <span className="min-w-12 text-center text-xs tabular-nums text-muted-foreground">
              {Math.round(zoom * 100)}%
            </span>
            <Button
              variant="ghost"
              size="icon-xs"
              onClick={handleZoomIn}
              disabled={zoom >= MAX_ZOOM}
              className="text-muted-foreground hover:text-foreground"
              title={t("common.zoom_in")}
            >
              <ZoomIn className="h-3.5 w-3.5" />
            </Button>
            <Button
              variant="ghost"
              size="icon-xs"
              onClick={handleReset}
              disabled={zoom === 1}
              className="text-muted-foreground hover:text-foreground"
              title={t("common.reset_zoom")}
            >
              <RotateCcw className="h-3.5 w-3.5" />
            </Button>
            <div className="mx-1 h-4 w-px bg-border" />
            <Button
              variant="ghost"
              size="icon-xs"
              onClick={toggleExpand}
              className="text-muted-foreground hover:text-foreground"
              title={expanded ? t("common.collapse") : t("common.expand")}
            >
              {expanded ? <Minimize2 className="h-3.5 w-3.5" /> : <Maximize2 className="h-3.5 w-3.5" />}
            </Button>
          </div>
        </div>
        {/* Diagram — SVG sanitized by DOMPurify */}
        <div
          className={`overflow-auto ${expanded ? "flex-1" : "max-h-100"}`}
          onWheel={handleWheel}
          tabIndex={0}
          role="region"
          aria-label="Diagram viewer"
        >
          <div
            role="img"
            aria-label="Mermaid flowchart or diagram"
            className="flex min-h-30 items-center justify-center p-4 [&_svg]:max-w-full"
            style={{
              transform: `scale(${zoom})`,
              transformOrigin: "top center",
              transition: "transform 0.15s ease-out",
            }}
            dangerouslySetInnerHTML={{ __html: svgHtml }}
          />
        </div>
      </div>
    </>
  )
}
