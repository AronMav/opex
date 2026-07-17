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
  const attempt = (async () => {
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
  inflight = attempt

  // Don't cache a failed init: the old per-render pattern self-healed on the
  // next render, so a transient import/initialize failure must not brick
  // mermaid until reload. On rejection, clear the cache so the next call
  // retries — but only if `attempt` is still the current in-flight promise
  // (a theme switch may have started a newer one; don't clobber it). The
  // caller still gets the rejection via the returned `attempt`.
  attempt.catch(() => {
    if (inflight === attempt) {
      inflight = null
      activeTheme = null
    }
  })

  return attempt
}
