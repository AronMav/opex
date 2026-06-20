import type { CreateProviderInput, ProviderOptions } from "@/types/api";

export function sortActiveRows(
  active: { capability: string; provider_name: string | null; priority: number }[],
  capability: string,
): { provider_name: string; priority: number }[] {
  return active
    .filter((a) => a.capability === capability && a.provider_name)
    .sort((a, b) => a.priority - b.priority)
    .map((a) => ({ provider_name: a.provider_name as string, priority: a.priority }));
}

export function buildActiveListAfterToggle(
  currentRows: { provider_name: string; priority: number }[],
  providerName: string,
  isCurrentlyActive: boolean,
  draftPriority: number,
): { provider_name: string; priority: number }[] {
  if (isCurrentlyActive) {
    return currentRows.filter((r) => r.provider_name !== providerName);
  }
  return [...currentRows, { provider_name: providerName, priority: draftPriority }];
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