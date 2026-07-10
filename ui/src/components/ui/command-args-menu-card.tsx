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
 * with the chosen value. Buttons lock after a click to prevent
 * double-submission, but a FAILED run unlocks them and shows an inline error
 * so the choice can be retried (mirrors handler-menu-card).
 */
export function CommandArgsMenuCard({ data }: { data: Record<string, unknown> }) {
  const text = typeof data.text === "string" ? data.text : undefined;
  const rawOptions = data.options;
  const options: CommandArgOption[] = Array.isArray(rawOptions) ? (rawOptions as CommandArgOption[]) : [];
  const token = typeof data.token === "string" ? data.token : undefined;
  const [chosen, setChosen] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const handleClick = async (value: string) => {
    if (!token || chosen) return;
    setChosen(value);
    setError(null);
    try {
      await apiPost("/api/commands/menu-run", { token, value });
    } catch {
      // Unlock so the user can retry; surface the failure instead of leaving
      // the buttons silently disabled forever.
      setChosen(null);
      setError("Не удалось запустить команду. Попробуйте ещё раз.");
    }
  };

  // Nothing to render — don't leave an empty bordered box in the transcript.
  if (!text && options.length === 0) return null;

  return (
    <div className="rounded-md border bg-card p-3">
      {text && <p className="text-sm">{text}</p>}
      {options.length > 0 && (
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
      )}
      {error && <p className="mt-2 text-xs text-destructive">{error}</p>}
    </div>
  );
}
