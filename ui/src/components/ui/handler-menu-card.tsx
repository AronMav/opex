"use client";

import { useState } from "react";
import { Button } from "@/components/ui/button";
import { getToken } from "@/lib/api";

interface HandlerItem {
  id: string;
  label?: string;
  description?: string;
}

/**
 * Clickable handler-selection menu (rendered from a `handler_menu` rich-card).
 * Each button deterministically enqueues the chosen handler via
 * `POST /api/files/run` — no LLM round-trip. The async result is delivered to
 * the chat when it finishes (same path as the model-driven run).
 */
export function HandlerMenuCard({ data }: { data: Record<string, unknown> }) {
  const handlers = (data.handlers as HandlerItem[]) ?? [];
  const sourceUrl = (data.source_url as string | null) ?? null;
  const uploadId = (data.upload_id as string | null) ?? null;
  const sessionId = data.session_id as string | undefined;
  const agent = data.agent as string | undefined;

  const [runningId, setRunningId] = useState<string | null>(null);
  const [doneId, setDoneId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  async function run(handlerId: string) {
    if (runningId || doneId) return;
    setRunningId(handlerId);
    setError(null);
    try {
      const res = await fetch("/api/files/run", {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
          Authorization: `Bearer ${getToken()}`,
        },
        body: JSON.stringify({
          handler_id: handlerId,
          source_url: sourceUrl,
          upload_id: uploadId,
          session_id: sessionId,
          agent,
        }),
      });
      if (!res.ok) {
        setError(`Не удалось запустить (HTTP ${res.status})`);
        setRunningId(null);
        return;
      }
      setDoneId(handlerId);
    } catch (e) {
      setError(String(e));
      setRunningId(null);
    }
  }

  if (handlers.length === 0) return null;

  const locked = Boolean(runningId || doneId);

  return (
    <div className="rounded-lg border bg-muted/20 p-3 my-1 space-y-2">
      <div className="text-sm font-medium text-muted-foreground">
        Выберите действие:
      </div>
      <div className="flex flex-col gap-2">
        {handlers.map((h) => {
          const isDone = doneId === h.id;
          const isRunning = runningId === h.id;
          return (
            <Button
              key={h.id}
              variant={isDone ? "secondary" : "outline"}
              className="justify-start h-auto py-2 text-left whitespace-normal"
              disabled={locked}
              onClick={() => run(h.id)}
            >
              <span className="flex flex-col items-start gap-0.5">
                <span className="font-medium">
                  {h.label || h.id}
                  {isRunning ? " — запускаю…" : isDone ? " ✓ запущено" : ""}
                </span>
                {h.description ? (
                  <span className="text-xs text-muted-foreground">
                    {h.description}
                  </span>
                ) : null}
              </span>
            </Button>
          );
        })}
      </div>
      {error ? <div className="text-xs text-destructive">{error}</div> : null}
      {doneId ? (
        <div className="text-xs text-muted-foreground">
          Результат появится в чате, когда обработка завершится.
        </div>
      ) : null}
    </div>
  );
}
