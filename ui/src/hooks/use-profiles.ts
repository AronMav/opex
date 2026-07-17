import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { apiDelete, apiGet, apiPost, apiPut } from "@/lib/api";
import { useAgents, useProviders, useProviderModelsDetailed } from "@/lib/queries";
import type { ProviderModel } from "@/lib/queries";

/** One provider/model(/voice) binding for a single capability slot. A
 *  capability can have several entries (fallback chain), hence the array. */
export interface SlotEntry {
  provider: string;
  model?: string;
  voice?: string;
}

/** Capability name -> ordered list of `SlotEntry` (fallback chain). Keyed by
 *  (a subset of) {@link PROFILE_CAPABILITIES}. */
export type ProfileSlots = Record<string, SlotEntry[]>;

/** Fields returned by every profile endpoint (create/update/copy/get-one). The
 *  list endpoint additionally splices in `agents` — see {@link ProfileRow}. */
export interface ProfileBase {
  id: string;
  name: string;
  slots: ProfileSlots;
  created_at: string;
  updated_at: string;
}

/** A profile row as returned by GET /api/profiles (list) ONLY — the list
 *  handler splices in `agents`. Other endpoints (create/update/copy/get-one)
 *  return {@link ProfileBase} without it. */
export interface ProfileRow extends ProfileBase {
  agents: string[];
}

/** Fixed capability set a profile's `slots` map may key into. */
export const PROFILE_CAPABILITIES = [
  "text",
  "compaction",
  "stt",
  "tts",
  "vision",
  "imagegen",
  "websearch",
] as const;

export type ProfileCapability = (typeof PROFILE_CAPABILITIES)[number];

const profilesKey = ["profiles"] as const;

/** GET /api/profiles — list all provider-binding profiles. */
export function useProfiles() {
  return useQuery({
    queryKey: profilesKey,
    queryFn: () => apiGet<{ profiles: ProfileRow[] }>("/api/profiles"),
  });
}

export interface CreateProfileInput {
  name: string;
  slots?: ProfileSlots;
}

/** POST /api/profiles — create a new profile. */
export function useCreateProfile() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (data: CreateProfileInput) => apiPost<ProfileBase>("/api/profiles", data),
    onSuccess: () => qc.invalidateQueries({ queryKey: profilesKey }),
    onError: (e: Error) => toast.error(e.message),
  });
}

export interface UpdateProfileInput {
  id: string;
  name?: string;
  slots?: ProfileSlots;
}

/** PUT /api/profiles/{id} — update an existing profile. */
export function useUpdateProfile() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ id, ...body }: UpdateProfileInput) =>
      apiPut<ProfileBase>(`/api/profiles/${encodeURIComponent(id)}`, body),
    onSuccess: () => qc.invalidateQueries({ queryKey: profilesKey }),
    onError: (e: Error) => toast.error(e.message),
  });
}

/** POST /api/profiles/{id}/copy — duplicate a profile. The backend takes no
 *  request body: it auto-generates the copy name ("{name} (copy)", "{name}
 *  (copy 2)", ...) — any body sent would be ignored. */
export function useCopyProfile() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (id: string) => apiPost<ProfileBase>(`/api/profiles/${encodeURIComponent(id)}/copy`),
    onSuccess: () => qc.invalidateQueries({ queryKey: profilesKey }),
    onError: (e: Error) => toast.error(e.message),
  });
}

/** DELETE /api/profiles/{id} — remove a profile. */
export function useDeleteProfile() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (id: string) => apiDelete(`/api/profiles/${encodeURIComponent(id)}`),
    onSuccess: () => qc.invalidateQueries({ queryKey: profilesKey }),
    onError: (e: Error) => toast.error(e.message),
  });
}

/** Resolved text-capability binding for an agent (replaces the removed
 *  `AgentInfoDto.provider_connection` / `.model` fields). */
export interface AgentTextModel {
  /** Text-capability provider name (was `AgentInfoDto.provider_connection`). */
  providerConnection: string | undefined;
  /** Default text model (was `AgentInfoDto.model`): the profile's text slot
   *  model if set, else the resolved provider's own `default_model`. */
  defaultModel: string;
}

/** Resolve an agent's default text provider + model from its profile's
 *  `slots.text[0]` entry. `agentProfileName` is `AgentInfoDto.profile` /
 *  `AgentDetailDto.profile`. */
export function useAgentTextModel(agentProfileName: string | undefined): AgentTextModel {
  const { data: profilesData } = useProfiles();
  const { data: providersData = [] } = useProviders();
  const profile = profilesData?.profiles.find((p) => p.name === agentProfileName);
  const entry = profile?.slots?.text?.[0];
  const providerConnection = entry?.provider;
  const defaultModel =
    (entry?.model && entry.model.length > 0
      ? entry.model
      : providersData.find((p) => p.name === providerConnection)?.default_model) ?? "";
  return { providerConnection, defaultModel };
}

/** Resolved model picker options for an agent's text capability: the model
 *  list of its active text provider plus the profile's default model. Shared
 *  by `composer/ModelDropdown.tsx` (persistent override) and the regenerate
 *  split-button model picker (13a, one-off override) so the fetch/derivation
 *  logic lives in exactly one place. */
export interface AgentModelOptions {
  models: ProviderModel[];
  defaultModel: string;
}

export function useAgentModelOptions(agent: string): AgentModelOptions {
  const { data: allAgents } = useAgents();
  const { data: allProviders = [] } = useProviders();
  const agentInfo = allAgents?.find((a) => a.name === agent);
  const { providerConnection, defaultModel } = useAgentTextModel(agentInfo?.profile);
  const selectedProvider = allProviders.filter((p) => p.type === "text").find((p) => p.name === providerConnection);
  const { data: models } = useProviderModelsDetailed(selectedProvider?.id ?? null);
  return { models: models ?? [], defaultModel };
}
