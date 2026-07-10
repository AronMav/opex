"use client";

import { useState } from "react";
import { apiPost } from "@/lib/api";

interface CommandArgOption {
  value: string;
  label: string;
}

/**
 * Prompt card for a chat-command that is missing a required source
 * (e.g. `/summarize_video` with no url), rendered from a
 * `command_args_menu` rich-card. Renders the prompt text and, when the
 * backend supplies `options`, a row of value buttons. Clicking a button
 * POSTs `{token, value}` to `/api/commands/menu-run` to run the command
 * with the chosen value; buttons are disabled after the first click to
 * prevent double-submission.
 */
export function CommandArgsMenuCard({ data }: { data: Record<string, unknown> }) {
  const text = data.text as string | undefined;
  const options = (data.options as CommandArgOption[] | undefined) ?? [];
  const token = data.token as string | undefined;
  const [chosen, setChosen] = useState<string | null>(null);

  const handleClick = (value: string) => {
    if (!token || chosen) return;
    setChosen(value);
    apiPost("/api/commands/menu-run", { token, value }).catch(() => {});
  };

  return (
    <div className="rounded-md border bg-card p-3">
      {text && <p className="text-sm">{text}</p>}
      {options.length ? (
        <div className="mt-2 flex flex-wrap gap-2">
          {options.map((o) => (
            <button
              key={o.value}
              type="button"
              disabled={chosen !== null}
              aria-pressed={chosen === o.value}
              onClick={() => handleClick(o.value)}
              className="rounded-md border px-2 py-1 text-xs hover:bg-accent disabled:cursor-not-allowed disabled:opacity-50 aria-pressed:bg-accent"
            >
              {o.label}
            </button>
          ))}
        </div>
      ) : null}
    </div>
  );
}
