"use client";
import { Fragment, useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { CommandInfo } from "@/types/api";
import type { PromptEntry } from "@/lib/prompts";
import { useTranslation } from "@/hooks/use-translation";

export const COMMAND_OPTION_ID_PREFIX = "command-option-";

/** Item handed to `onPick` — a discriminated union so the composer can tell a
 *  registry command pick (insert `/name`, or run immediately if no-arg) apart
 *  from a prompt-library pick (replace the whole composer text with the
 *  prompt body, no `/` involved, never auto-sent). */
export type AutocompleteItem =
  | { kind: "command"; name: string; description?: string }
  | { kind: "prompt"; title: string; body: string };

type Row =
  | { kind: "command"; command: CommandInfo }
  | { kind: "prompt"; prompt: PromptEntry };

interface Props {
  input: string;
  commands: CommandInfo[];
  /** Workspace prompt-library entries (workspace/prompts.md), rendered as a
   *  "Prompts" section below the matching commands. Optional — defaults to
   *  none so existing callers/tests that don't pass prompts keep working. */
  prompts?: PromptEntry[];
  onPick: (item: AutocompleteItem) => void;
  onClose: () => void;
  /** Reports the active option's DOM id (or null when closed) so the composer
   *  textarea can mirror it via aria-activedescendant (WAI-ARIA combobox). */
  onActiveChange?: (optionId: string | null) => void;
  /** id for the listbox element so the composer textarea can point at it via
   *  aria-controls. */
  listboxId?: string;
}

/** Registry-backed slash-command dropdown — the single slash menu in the composer,
 *  driven by the /api/commands registry plus the workspace prompt library
 *  (workspace/prompts.md). Keyboard nav: ArrowUp/ArrowDown moves the active
 *  item (scrolling it into view) across BOTH sections, Enter/Tab picks it,
 *  Escape closes. The active item gets a strong, distinct highlight (not just
 *  the subtle hover tint) so keyboard selection is clearly visible.
 *
 *  Prompt rows render WITHOUT the leading "/" — they aren't commands, and a
 *  prompt titled the same as a real command (e.g. "compact") must not shadow
 *  it: both rows show up side by side, and picking either behaves per its own
 *  kind. */
export function CommandAutocomplete({ input, commands, prompts = [], onPick, onClose, onActiveChange, listboxId }: Props) {
  const { t } = useTranslation();
  const [activeIdx, setActiveIdx] = useState(0);
  const listRef = useRef<HTMLDivElement>(null);
  const isSlash = input.startsWith("/");
  const q = isSlash ? input.slice(1).toLowerCase() : "";
  // Memoized so ChatComposer's per-keystroke re-render doesn't rebuild the list
  // (and tear down/re-attach the keydown listener) on every keypress.
  const commandMatches = useMemo(
    () =>
      isSlash
        ? commands.filter(
            (c) => c.name.toLowerCase().startsWith(q) || c.aliases.some((a) => a.toLowerCase().startsWith(q)),
          )
        : [],
    [isSlash, q, commands],
  );
  const promptMatches = useMemo(
    () => (isSlash ? prompts.filter((p) => p.title.toLowerCase().startsWith(q)) : []),
    [isSlash, q, prompts],
  );
  const matches = useMemo<Row[]>(
    () => [
      ...commandMatches.map((command): Row => ({ kind: "command", command })),
      ...promptMatches.map((prompt): Row => ({ kind: "prompt", prompt })),
    ],
    [commandMatches, promptMatches],
  );

  useEffect(() => { setActiveIdx(0); }, [input]);

  // Keep the composer's aria-activedescendant in sync; clear it on unmount.
  useEffect(() => {
    onActiveChange?.(
      matches.length > 0 ? `${COMMAND_OPTION_ID_PREFIX}${Math.min(activeIdx, matches.length - 1)}` : null,
    );
  }, [activeIdx, matches.length, onActiveChange]);
  useEffect(() => () => onActiveChange?.(null), [onActiveChange]);

  // Keep the keyboard-selected item scrolled into view — the list can overflow its
  // max-height, so ArrowDown past the fold must reveal the newly-active row.
  useEffect(() => {
    const el = listRef.current?.querySelector<HTMLElement>(`#${COMMAND_OPTION_ID_PREFIX}${activeIdx}`);
    el?.scrollIntoView?.({ block: "nearest" });
  }, [activeIdx]);

  const pickRow = useCallback((row: Row) => {
    if (row.kind === "command") {
      onPick({ kind: "command", name: row.command.name, description: row.command.description });
    } else {
      onPick({ kind: "prompt", title: row.prompt.title, body: row.prompt.body });
    }
  }, [onPick]);

  useEffect(() => {
    if (matches.length === 0) return;
    const handler = (e: KeyboardEvent) => {
      // Only act while the keystroke originates inside the composer. A stray "/"
      // left in the textarea must NOT let this menu hijack Arrow/Enter/Escape in
      // an unrelated control elsewhere on the page once focus has moved away.
      // (In unit tests the event is dispatched on `window`, whose target isn't an
      // HTMLElement, so the guard is skipped and the menu stays testable.)
      if (e.target instanceof HTMLElement && !e.target.closest("[data-composer-input]")) return;
      if (e.key === "ArrowDown") { e.preventDefault(); setActiveIdx((i) => (i + 1) % matches.length); }
      else if (e.key === "ArrowUp") { e.preventDefault(); setActiveIdx((i) => (i - 1 + matches.length) % matches.length); }
      else if (e.key === "Enter" || (e.key === "Tab" && !e.shiftKey)) {
        // Shift+Tab остаётся обратной навигацией фокуса — перехват выбором
        // команды ломал бы клавиатурную доступность.
        e.preventDefault();
        const safeIdx = Math.min(activeIdx, matches.length - 1);
        if (matches[safeIdx]) pickRow(matches[safeIdx]);
      } else if (e.key === "Escape") {
        onClose();
      }
    };
    window.addEventListener("keydown", handler, { capture: true });
    return () => window.removeEventListener("keydown", handler, { capture: true });
  }, [matches, activeIdx, onClose, pickRow]);

  if (!isSlash || matches.length === 0) return null;

  return (
    <div
      ref={listRef}
      role="listbox"
      id={listboxId}
      aria-label="Slash commands"
      className="absolute bottom-full mb-1 w-full max-h-64 overflow-y-auto rounded-md border bg-popover shadow-md"
    >
      {matches.map((row, i) => {
        const active = i === activeIdx;
        // The header precedes the FIRST prompt row. `i === commandMatches.length`
        // alone is correct in both layouts: with command matches the first prompt
        // sits right after them; with zero command matches it sits at i === 0.
        const showPromptsHeader = row.kind === "prompt" && i === commandMatches.length;
        if (row.kind === "command") {
          const c = row.command;
          return (
            <button
              key={`cmd-${c.name}`}
              type="button"
              role="option"
              aria-selected={active}
              id={`${COMMAND_OPTION_ID_PREFIX}${i}`}
              className={`flex w-full items-baseline gap-2 border-l-2 px-3 py-1.5 text-left transition-colors ${
                active
                  ? "border-primary bg-accent font-medium text-accent-foreground"
                  : "border-transparent hover:bg-accent/60"
              }`}
              onMouseEnter={() => setActiveIdx(i)}
              onMouseDown={(e) => { e.preventDefault(); pickRow(row); }}
            >
              <span className="font-mono text-sm">/{c.name}</span>
              {c.args.length > 0 && (
                <span className={`font-mono text-xs ${active ? "text-accent-foreground/70" : "text-muted-foreground"}`}>
                  {c.args.map((a) => `<${a.name}>`).join(" ")}
                </span>
              )}
              <span className={`ml-auto truncate text-xs ${active ? "text-accent-foreground/80" : "text-muted-foreground"}`}>
                {c.description}
              </span>
            </button>
          );
        }
        const p = row.prompt;
        // Fragment (not a wrapper div) keeps prompt rows direct children of the
        // listbox, same as command rows — the header div is just a sibling.
        return (
          <Fragment key={`prompt-${p.title}`}>
            {showPromptsHeader && (
              <div className="border-t border-border/50 px-3 pt-1.5 pb-1 text-xs font-medium uppercase tracking-wide text-muted-foreground">
                {t("chat.prompts_section")}
              </div>
            )}
            <button
              type="button"
              role="option"
              aria-selected={active}
              id={`${COMMAND_OPTION_ID_PREFIX}${i}`}
              className={`flex w-full items-baseline gap-2 border-l-2 px-3 py-1.5 text-left transition-colors ${
                active
                  ? "border-primary bg-accent font-medium text-accent-foreground"
                  : "border-transparent hover:bg-accent/60"
              }`}
              onMouseEnter={() => setActiveIdx(i)}
              onMouseDown={(e) => { e.preventDefault(); pickRow(row); }}
            >
              <span className="text-sm">{p.title}</span>
              <span className={`ml-auto truncate text-xs ${active ? "text-accent-foreground/80" : "text-muted-foreground"}`}>
                {p.body}
              </span>
            </button>
          </Fragment>
        );
      })}
    </div>
  );
}
