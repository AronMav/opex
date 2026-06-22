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

/** True when a row would be a 0-click tool default referencing a non-allowlisted action. */
export function isAllowlistViolation(
  executor: string,
  is_default: boolean,
  action_ref: string,
): boolean {
  if (executor !== "tool" || !is_default) return false;
  return !(FSE_ALLOWLIST_MEMBERS as readonly string[]).includes(action_ref);
}
