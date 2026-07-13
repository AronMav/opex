"use client";

import { Suspense, useEffect, useState } from "react";
import { useSearchParams } from "next/navigation";

// Read-only public transcript view. Lives OUTSIDE the (authenticated) route
// group, so no token / auth gate applies — the unguessable share token in the
// `?token=` query is the security boundary (matches the backend's auth-exempt
// GET /api/shares/{token}). A query param (not a path segment) keeps this a
// single static page under `output: export`. Content is rendered as plain text
// (whitespace preserved, no HTML injection) since it's untrusted conversation
// text.

interface ShareMessage {
  role: string;
  content: string;
  tools: string[];
  created_at: string;
}
interface ShareData {
  title: string | null;
  agent: string;
  messages: ShareMessage[];
}

const ROLE_LABEL: Record<string, string> = {
  user: "User",
  assistant: "Assistant",
  tool: "Tool",
};

function SharedConversation() {
  const searchParams = useSearchParams();
  const token = searchParams.get("token");
  const [data, setData] = useState<ShareData | null>(null);
  const [state, setState] = useState<"loading" | "ok" | "not-found" | "error">("loading");

  useEffect(() => {
    if (!token) { setState("not-found"); return; }
    let cancelled = false;
    fetch(`/api/shares/${encodeURIComponent(token)}`)
      .then(async (r) => {
        if (r.status === 404) throw new Error("not-found");
        if (!r.ok) throw new Error("error");
        return (await r.json()) as ShareData;
      })
      .then((d) => { if (!cancelled) { setData(d); setState("ok"); } })
      .catch((e) => { if (!cancelled) setState(e.message === "not-found" ? "not-found" : "error"); });
    return () => { cancelled = true; };
  }, [token]);

  return (
    <div className="min-h-dvh bg-background text-foreground">
      <div className="mx-auto max-w-3xl px-4 py-8 sm:px-6">
        <header className="mb-6 border-b border-border pb-4">
          <div className="text-2xs font-bold uppercase tracking-widest text-muted-foreground">
            OPEX · shared conversation (read-only)
          </div>
          {state === "ok" && data && (
            <>
              <h1 className="mt-1 text-lg font-bold tracking-tight">
                {data.title || "Untitled conversation"}
              </h1>
              <div className="text-xs text-muted-foreground">{data.agent}</div>
            </>
          )}
        </header>

        {state === "loading" && <p className="text-sm text-muted-foreground">Loading…</p>}
        {state === "not-found" && (
          <p className="text-sm text-muted-foreground">
            This share link is invalid or has been revoked.
          </p>
        )}
        {state === "error" && (
          <p className="text-sm text-destructive">Failed to load the shared conversation.</p>
        )}

        {state === "ok" && data && (
          <div className="space-y-4">
            {data.messages.length === 0 && (
              <p className="text-sm text-muted-foreground">No messages in this conversation.</p>
            )}
            {data.messages.map((m, i) => (
              <div
                key={i}
                className={`rounded-lg border p-3 ${
                  m.role === "user"
                    ? "border-primary/30 bg-primary/5"
                    : m.role === "tool"
                      ? "border-border/50 bg-muted/30"
                      : "border-border bg-card"
                }`}
              >
                <div className="mb-1 text-2xs font-bold uppercase tracking-wider text-muted-foreground">
                  {ROLE_LABEL[m.role] ?? m.role}
                </div>
                {m.content && (
                  <div className="whitespace-pre-wrap break-words text-sm leading-relaxed">
                    {m.content}
                  </div>
                )}
                {m.tools.length > 0 && (
                  <div className="mt-2 flex flex-wrap gap-1">
                    {m.tools.map((tool, j) => (
                      <span
                        key={j}
                        className="rounded bg-muted px-1.5 py-0.5 font-mono text-2xs text-muted-foreground"
                      >
                        {tool}
                      </span>
                    ))}
                  </div>
                )}
              </div>
            ))}
          </div>
        )}

        <footer className="mt-10 border-t border-border pt-4 text-center text-2xs text-muted-foreground">
          Powered by OPEX
        </footer>
      </div>
    </div>
  );
}

export default function SharePage() {
  return (
    <Suspense fallback={<div className="min-h-dvh bg-background" />}>
      <SharedConversation />
    </Suspense>
  );
}
