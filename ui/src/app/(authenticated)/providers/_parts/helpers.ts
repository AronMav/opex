import type { CreateProviderInput, ProviderOptions, Provider } from "@/types/api";

export function sortActiveRows(
  active: { capability: string; provider_name: string | null; priority: number }[],
  capability: string,
): { provider_name: string; priority: number }[] {
  return active
    .filter((a) => a.capability === capability && a.provider_name)
    .sort((a, b) => a.priority - b.priority)
    .map((a) => ({ provider_name: a.provider_name as string, priority: a.priority }));
}

export function renumberPriorities(
  orderedNames: string[],
): { provider_name: string; priority: number }[] {
  return orderedNames.map((name, index) => ({ provider_name: name, priority: index + 1 }));
}

export function splitProviders(
  capProviders: Provider[],
  activeRows: { provider_name: string; priority: number }[],
): { active: Provider[]; inactive: Provider[] } {
  const activeNames = new Set(activeRows.map((r) => r.provider_name));
  const active = activeRows
    .map((r) => capProviders.find((p) => p.name === r.provider_name))
    .filter((p): p is Provider => !!p);
  const inactive = capProviders
    .filter((p) => !activeNames.has(p.name))
    .sort((a, b) => a.name.localeCompare(b.name));
  return { active, inactive };
}

export function buildProviderBody(
  form: CreateProviderInput,
  apiKeyValue: string,
  category: string,
): CreateProviderInput {
  const body: CreateProviderInput = {
    ...form,
    type: category,
    base_url: form.base_url || undefined,
    default_model: form.default_model || undefined,
    notes: form.notes || undefined,
  };
  const trimmedKey = apiKeyValue.trim();
  if (trimmedKey) {
    body.api_key = trimmedKey;
  }
  return body;
}

export function getOpts(f: CreateProviderInput): ProviderOptions {
  return (f.options as ProviderOptions | undefined) ?? {};
}