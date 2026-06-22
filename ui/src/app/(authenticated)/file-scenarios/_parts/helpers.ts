import type { FileScenario, CreateFileScenarioInput } from "@/types/api";

// Mirror of the core constant FSE_DEFAULT_ALLOWLIST. Closed-domain: the
// backend rejects any name outside this set (providers.rs-style 400).
export const FSE_ALLOWLIST_MEMBERS = [
  "transcribe",
  "describe",
  "extract_document",
  "save",
] as const;

export function groupByMatchType(
  scenarios: FileScenario[],
): { matchType: string; bindings: FileScenario[] }[] {
  const byType = new Map<string, FileScenario[]>();
  for (const s of scenarios) {
    const arr = byType.get(s.match_type) ?? [];
    arr.push(s);
    byType.set(s.match_type, arr);
  }
  return [...byType.entries()]
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([matchType, bindings]) => ({ matchType, bindings: sortBindings(bindings) }));
}

export function sortBindings(bindings: FileScenario[]): FileScenario[] {
  return [...bindings].sort((a, b) => {
    if (a.priority !== b.priority) return a.priority - b.priority;
    if (a.created_at !== b.created_at) return a.created_at.localeCompare(b.created_at);
    return a.id.localeCompare(b.id);
  });
}

export function buildScenarioBody(form: CreateFileScenarioInput): CreateFileScenarioInput {
  return {
    match_type: form.match_type.trim(),
    executor: form.executor,
    action_ref: form.action_ref.trim(),
    label: form.label.trim(),
    is_default: form.is_default ?? false,
    priority: form.priority ?? 100,
    enabled: form.enabled ?? true,
  };
}

/** True when a row would be a 0-click tool default referencing a non-allowlisted action.
 *
 * @param enabledAllowlist  The operator's currently-enabled allowlist entries.
 *   Accepts a `ReadonlySet<string>`, a `readonly string[]`, or any iterable
 *   of `action_ref` strings where `enabled === true`.
 *   Defaults to the full static domain (`FSE_ALLOWLIST_MEMBERS`) so existing
 *   callers that don't yet pass the live set keep their current behaviour.
 *   Callers that have access to `FileScenarioAllowlistRow[]` should pass
 *   the set of `action_ref` values where `enabled === true`.
 */
export function isAllowlistViolation(
  executor: string,
  is_default: boolean,
  action_ref: string,
  enabledAllowlist: ReadonlySet<string> | readonly string[] = FSE_ALLOWLIST_MEMBERS,
): boolean {
  if (executor !== "tool" || !is_default) return false;
  const set =
    enabledAllowlist instanceof Set
      ? enabledAllowlist
      : new Set(enabledAllowlist);
  return !set.has(action_ref);
}

/**
 * A binding is INELIGIBLE to be the auto-default if it's a skill (skills can
 * never be the 0-click default) OR it's a tool whose action_ref is not in the
 * currently-enabled allowlist. Single source of truth for default-eligibility
 * across the page gate and the dialog.
 *
 * @param enabledAllowlist  Defaults to the static `FSE_ALLOWLIST_MEMBERS` set,
 *   which is the correct default for the dialog. The page gate should pass the
 *   live `enabledAllowlistSet` derived from `useFileScenarioAllowlist`.
 */
export function isDefaultIneligible(
  executor: string,
  action_ref: string,
  enabledAllowlist: ReadonlySet<string> | readonly string[] = FSE_ALLOWLIST_MEMBERS,
): boolean {
  if (executor === "skill") return true;
  return isAllowlistViolation(executor, true, action_ref, enabledAllowlist);
}
