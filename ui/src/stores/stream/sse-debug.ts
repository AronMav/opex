/**
 * Lightweight SSE event tracer for debugging duplication / ordering issues
 * in live streams. Disabled by default — incurs zero cost when off.
 *
 * Enable in the browser console once:
 *   localStorage.setItem("hydeclaw_debug_sse", "1"); location.reload();
 *
 * Disable:
 *   localStorage.removeItem("hydeclaw_debug_sse"); location.reload();
 *
 * What it logs:
 *  • Every SSE event received (type, key fields, raw delta text up to 80 chars)
 *  • Buffer state snapshots after non-text events (parts count + types)
 *  • Commit calls (which message id, parts count)
 *
 * Logs go to console with `[SSE]` prefix so they can be filtered. The last
 * N entries are also retained in memory and accessible via:
 *   window.__hydeclawDebugSSE() — returns the in-memory log array
 *   window.__hydeclawDebugSSECopy() — copies the JSON dump to clipboard
 */

const RING_BUFFER_SIZE = 1000;

interface DebugEntry {
  ts: number;
  agent: string;
  msg: string;
  data?: unknown;
}

let enabled: boolean | null = null;
const ring: DebugEntry[] = [];

function isEnabled(): boolean {
  if (enabled === null) {
    if (typeof window === "undefined") {
      enabled = false;
    } else {
      try {
        enabled = window.localStorage.getItem("hydeclaw_debug_sse") === "1";
      } catch {
        enabled = false;
      }
      if (enabled) {
        installGlobals();
      }
    }
  }
  return enabled;
}

function installGlobals() {
  if (typeof window === "undefined") return;
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  (window as any).__hydeclawDebugSSE = () => ring.slice();
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  (window as any).__hydeclawDebugSSECopy = async () => {
    const text = JSON.stringify(ring, null, 2);
    try {
      await navigator.clipboard.writeText(text);
      // eslint-disable-next-line no-console
      console.log("[SSE] copied", ring.length, "entries");
    } catch (e) {
      // eslint-disable-next-line no-console
      console.warn("[SSE] clipboard failed", e);
    }
  };
  // eslint-disable-next-line no-console
  console.log("[SSE] debug enabled. window.__hydeclawDebugSSECopy() to copy log.");
}

export function sseLog(agent: string, msg: string, data?: unknown): void {
  if (!isEnabled()) return;
  const entry: DebugEntry = { ts: performance.now(), agent, msg, data };
  ring.push(entry);
  if (ring.length > RING_BUFFER_SIZE) ring.shift();
  // eslint-disable-next-line no-console
  console.log(`[SSE ${agent} +${entry.ts.toFixed(0)}ms]`, msg, data ?? "");
}
