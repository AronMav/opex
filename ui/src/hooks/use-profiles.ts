import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { apiDelete, apiGet, apiPost, apiPut } from "@/lib/api";

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

export interface ProfileRow {
  id: string;
  name: string;
  slots: ProfileSlots;
  agents: string[];
  created_at: string;
  updated_at: string;
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
    mutationFn: (data: CreateProfileInput) => apiPost<ProfileRow>("/api/profiles", data),
    onSuccess: () => qc.invalidateQueries({ queryKey: profilesKey }),
    onError: (e: Error) => toast.error(e.message),
  });
}

export interface UpdateProfileInput {
  id: string;
  name?: string;
  slots?: ProfileSlots;
  agents?: string[];
}

/** PUT /api/profiles/{id} — update an existing profile. */
export function useUpdateProfile() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ id, ...body }: UpdateProfileInput) =>
      apiPut<ProfileRow>(`/api/profiles/${encodeURIComponent(id)}`, body),
    onSuccess: () => qc.invalidateQueries({ queryKey: profilesKey }),
    onError: (e: Error) => toast.error(e.message),
  });
}

export interface CopyProfileInput {
  id: string;
  name?: string;
}

/** POST /api/profiles/{id}/copy — duplicate a profile (optionally renamed). */
export function useCopyProfile() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ id, name }: CopyProfileInput) =>
      apiPost<ProfileRow>(`/api/profiles/${encodeURIComponent(id)}/copy`, name ? { name } : undefined),
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
