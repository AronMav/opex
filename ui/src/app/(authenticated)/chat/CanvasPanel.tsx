"use client";

import { useEffect, useCallback } from "react";
import { useCanvasStore } from "@/stores/canvas-store";
import type { AgentCanvas } from "@/stores/canvas-store";
import { useWsSubscription } from "@/hooks/use-ws-subscription";
import { useTranslation } from "@/hooks/use-translation";
import { useChatStore } from "@/stores/chat-store";

import { apiGet } from "@/lib/api";
import { sanitizeUrl } from "@/lib/sanitize-url";
import { Markdown } from "@/components/ui/markdown";
import { ErrorBoundary } from "@/components/ui/error-boundary";
import { TableCard } from "@/components/ui/rich-card";
import { Button } from "@/components/ui/button";
import { PanelRight, Trash2 } from "lucide-react";

interface CanvasPanelProps {
  agent: string;
}

export function CanvasPanel({ agent }: CanvasPanelProps) {
  const { t } = useTranslation();
  // Key by sessionId when available, fall back to agent name
  const activeSessionId = useChatStore((s) => s.agents[s.currentAgent]?.activeSessionId ?? null);
  const canvasKey = activeSessionId ?? agent;
  const canvas: AgentCanvas | undefined = useCanvasStore((s) => s.canvases[canvasKey]);
  const clearCanvas = useCanvasStore((s) => s.clearCanvas);

  // Load canvas state from backend on mount / agent change
  useEffect(() => {
    if (!agent) return;
    // Skip if already loaded for this key
    if (useCanvasStore.getState().canvases[canvasKey]) return;
    let cancelled = false;
    apiGet<Record<string, unknown>>(`/api/canvas/${agent}`)
      .then((data) => {
        if (cancelled) return;
        if (data.content) {
          useCanvasStore.getState().handleEvent(
            data as { action: string; agent?: string; content_type?: string; content?: string; title?: string | null },
            canvasKey,
          );
        }
      })
      .catch((e) => { console.warn("[canvas] load failed:", e); });
    return () => { cancelled = true; };
  }, [agent, canvasKey]);

  // Listen for canvas_update WS events — pass sessionId-based key
  useWsSubscription("canvas_update", useCallback((msg) => {
    const key = msg.agent ?? useChatStore.getState().currentAgent;
    useCanvasStore.getState().handleEvent(msg, key);
  }, []));

  if (!canvas?.content) {
    return (
      <div className="flex h-full flex-col items-center justify-center gap-4 text-muted-foreground p-6">
        <PanelRight size={40} strokeWidth={1.5} className="opacity-20" />
        <div className="text-center">
          <p className="text-sm font-medium">{t("canvas.empty")}</p>
          <p className="mt-1 text-xs opacity-60">{t("canvas.empty_hint")}</p>
        </div>
      </div>
    );
  }

  return (
    <div className="flex h-full flex-col">
      {/* Header */}
      <div className="flex items-center justify-between border-b border-border/50 px-4 py-2.5 shrink-0">
        <div className="flex items-center gap-2 min-w-0">
          <PanelRight size={16} className="text-primary shrink-0" />
          <span className="font-display text-sm font-bold truncate">
            {canvas.title ?? t("canvas.title")}
          </span>
          <span className="text-[10px] font-mono text-muted-foreground/50 uppercase truncate">{agent}</span>
        </div>
        <Button variant="ghost" size="xs" className="text-muted-foreground hover:text-destructive" onClick={() => clearCanvas(canvasKey)}>
          <Trash2 size={12} className="mr-1" />
          {t("canvas.clear")}
        </Button>
      </div>

      {/* Content */}
      <div className="flex-1 overflow-auto p-4">
        {canvas.contentType === "markdown" && (
          <div className="prose prose-sm dark:prose-invert max-w-4xl mx-auto [&_table]:block [&_table]:overflow-x-auto [&_table]:w-full [&_pre]:overflow-x-auto">
            <ErrorBoundary>
              <Markdown>{canvas.content}</Markdown>
            </ErrorBoundary>
          </div>
        )}

        {canvas.contentType === "html" && (
          // Intentional: sandbox="allow-scripts" without allow-same-origin is the accepted
          // security trade-off. The iframe CAN execute scripts but CANNOT access parent page
          // cookies, localStorage, or DOM. NEVER add allow-same-origin here -- that would
          // enable the sandboxed content to escape the iframe and access the parent origin.
          <iframe
            srcDoc={canvas.content}
            sandbox="allow-scripts"
            className="h-full w-full rounded-lg border bg-background"
            title={t("canvas.iframe_html")}
          />
        )}

        {canvas.contentType === "url" && (
          <iframe
            src={sanitizeUrl(canvas.content)}
            sandbox="allow-scripts allow-same-origin allow-forms"
            className="h-full w-full rounded-lg border bg-background"
            title={t("canvas.iframe_url")}
          />
        )}

        {canvas.contentType === "json" && <JsonDataView data={canvas.content} />}
      </div>
    </div>
  );
}

function JsonDataView({ data }: { data: string }) {
  let parsed: unknown;
  try {
    parsed = JSON.parse(data);
  } catch {
    return <pre className="text-sm font-mono whitespace-pre-wrap">{data}</pre>;
  }

  if (
    typeof parsed === "object" &&
    parsed !== null &&
    "columns" in parsed &&
    "rows" in parsed
  ) {
    return <TableCard data={parsed as Record<string, unknown>} />;
  }

  return (
    <pre className="rounded-lg border bg-muted/30 p-4 text-sm font-mono whitespace-pre-wrap overflow-auto">
      {JSON.stringify(parsed, null, 2)}
    </pre>
  );
}
