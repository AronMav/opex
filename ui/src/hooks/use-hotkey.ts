import { useEffect, useRef } from "react";

interface HotkeyOptions {
  /** If true, hotkey fires even when focus is in an input/textarea. Default: false. */
  allowInInput?: boolean;
  /** Modifier keys required. */
  ctrl?: boolean;
  meta?: boolean;
  shift?: boolean;
  /** Also match the opposite modifier (ctrl matches meta on Mac, meta matches ctrl on non-Mac). */
  ctrlOrMeta?: boolean;
}

/**
 * Register a global keyboard shortcut.
 * @param key - The key to match (e.g., "k", "Escape", "/")
 * @param handler - Callback when hotkey is pressed
 * @param options - Modifier and scope options
 */
export function useHotkey(
  key: string,
  handler: (e: KeyboardEvent) => void,
  options: HotkeyOptions = {},
) {
  const handlerRef = useRef(handler);
  useEffect(() => {
    handlerRef.current = handler;
  });

  useEffect(() => {
    const listener = (e: KeyboardEvent) => {
      // Skip inputs unless explicitly allowed
      if (!options.allowInInput) {
        const tag = (e.target as HTMLElement).tagName;
        if (["INPUT", "TEXTAREA", "SELECT"].includes(tag)) return;
      }

      // Check key match (case-insensitive for letters)
      if (e.key.toLowerCase() !== key.toLowerCase() && e.key !== key) return;

      // Check modifiers
      if (options.ctrlOrMeta) {
        if (!e.ctrlKey && !e.metaKey) return;
      } else {
        if (options.ctrl && !e.ctrlKey) return;
        if (options.meta && !e.metaKey) return;
      }
      if (options.shift && !e.shiftKey) return;

      handlerRef.current(e);
    };
    document.addEventListener("keydown", listener);
    return () => document.removeEventListener("keydown", listener);
  }, [key, options.allowInInput, options.ctrl, options.meta, options.shift, options.ctrlOrMeta]);
}
