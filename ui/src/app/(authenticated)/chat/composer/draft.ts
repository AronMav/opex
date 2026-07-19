// ── composer/draft.ts ──────────────────────────────────────────────────────
// Composer draft persistence + programmatic fill. Pure DOM/localStorage helpers
// with no React imports, so lightweight callers (e.g. the welcome screen) can
// drop text into the composer without pulling in the composer's whole module
// graph (voice hooks, autocomplete, etc.).

const DRAFT_PREFIX = "opex.draft.";

export function saveDraft(agent: string, text: string) {
  if (text) localStorage.setItem(DRAFT_PREFIX + agent, text);
  else localStorage.removeItem(DRAFT_PREFIX + agent);
}

export function loadDraft(agent: string): string {
  return localStorage.getItem(DRAFT_PREFIX + agent) ?? "";
}

export function clearDraft(agent: string) {
  localStorage.removeItem(DRAFT_PREFIX + agent);
}

/**
 * Drop `text` into the composer WITHOUT sending — used by welcome-screen starter
 * chips so a click lands the prompt as an editable draft (user tweaks + hits
 * Enter) instead of firing it immediately.
 *
 * Persists the draft first, so a composer that is unmounted or remounting picks
 * it up via its agent-switch restore effect; then, when the textarea is already
 * mounted (the usual case — the composer sits under the welcome screen), updates
 * it live via the native value setter + a bubbling `input` event. That event is
 * what the composer's own `onInput` listens for, so hasInput/autoresize/draft
 * all update exactly as if the user had typed — identical to the slash-menu
 * prompt-pick mechanic.
 */
export function fillComposer(agent: string, text: string) {
  saveDraft(agent, text);
  if (typeof document === "undefined") return;
  const ta = document.querySelector<HTMLTextAreaElement>('form[data-composer-input] textarea');
  if (!ta) return;
  const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")?.set;
  setter?.call(ta, text);
  ta.dispatchEvent(new Event("input", { bubbles: true }));
  ta.focus();
  ta.setSelectionRange(text.length, text.length);
}
