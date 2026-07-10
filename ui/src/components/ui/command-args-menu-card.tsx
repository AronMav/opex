"use client";

interface CommandArgOption {
  value: string;
  label: string;
}

/**
 * Prompt card for a chat-command that is missing a required source
 * (e.g. `/summarize_video` with no url), rendered from a
 * `command_args_menu` rich-card. MVP (Phase 2a): renders the prompt text
 * and, when the backend supplies `options`, a row of display-only value
 * buttons. Click-to-run wiring lands in Phase 2b.
 */
export function CommandArgsMenuCard({ data }: { data: Record<string, unknown> }) {
  const text = data.text as string | undefined;
  const options = (data.options as CommandArgOption[] | undefined) ?? [];

  return (
    <div className="rounded-md border bg-card p-3">
      {text && <p className="text-sm">{text}</p>}
      {options.length ? (
        <div className="mt-2 flex flex-wrap gap-2">
          {options.map((o) => (
            <button key={o.value} type="button" className="rounded-md border px-2 py-1 text-xs hover:bg-accent">
              {o.label}
            </button>
          ))}
        </div>
      ) : null}
    </div>
  );
}
