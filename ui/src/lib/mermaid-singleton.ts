// Single mermaid.initialize per theme. mermaid keeps global config; re-running
// initialize on every block render (the old per-render pattern) is wasted work
// and re-parses. Options below are carried verbatim from mermaid-block.tsx —
// INCLUDING the light->"neutral" mapping (mermaid's own "light" theme renders
// differently and was never used here).
import type mermaidType from "mermaid"

// `activeTheme` is set synchronously (before the first `await`) so that
// parallel calls for the same theme within the same tick see the in-flight
// promise instead of each kicking off their own `initialize` — that's the
// single-flight guarantee. A theme switch clears it, forcing re-init.
let activeTheme: "light" | "dark" | null = null
let inflight: Promise<typeof mermaidType> | null = null

export function getMermaid(resolvedTheme: "light" | "dark"): Promise<typeof mermaidType> {
  if (inflight && activeTheme === resolvedTheme) return inflight

  activeTheme = resolvedTheme
  inflight = (async () => {
    const mermaid = (await import("mermaid")).default
    mermaid.initialize({
      startOnLoad: false,
      theme: resolvedTheme === "dark" ? "dark" : "neutral",
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
    return mermaid
  })()

  return inflight
}
