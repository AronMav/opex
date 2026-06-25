import { useAuthStore } from "@/stores/auth-store";
import type { CheckpointListDto, RestoreReportDto } from "@/types/api.generated";

const REQUEST_TIMEOUT = 30_000;

export function getToken(): string {
  return useAuthStore.getState().token;
}

let redirecting = false;
/** Reset redirect guard (for tests only). */
export function _resetRedirecting() { redirecting = false; }
export function handleUnauthorized() {
  if (redirecting) return;
  redirecting = true;
  useAuthStore.getState().logout();
  window.location.href = "/login";
}

/** Get token with validation — throws if missing. Use in one-shot fetch calls. */
export function assertToken(): string {
  if (redirecting) throw new Error("Session expired");
  const token = getToken();
  if (!token) {
    handleUnauthorized();
    throw new Error("Session expired");
  }
  return token;
}

async function apiFetch(path: string, init?: RequestInit): Promise<Response> {
  // If already redirecting to login, don't make more requests (prevents rate limit lockout)
  if (redirecting) {
    throw new Error("Session expired");
  }

  const token = getToken();
  if (!token) {
    handleUnauthorized();
    throw new Error("Session expired");
  }

  const headers: Record<string, string> = {
    "Content-Type": "application/json",
    ...(init?.headers as Record<string, string>),
  };
  headers["Authorization"] = `Bearer ${token}`;

  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), REQUEST_TIMEOUT);

  try {
    const signal = init?.signal
      ? AbortSignal.any([init.signal, controller.signal])
      : controller.signal;

    const resp = await fetch(path, {
      ...init,
      headers,
      signal,
    });
    if (resp.status === 401) {
      handleUnauthorized();
      throw new Error("Session expired");
    }
    if (resp.status === 429) {
      throw new Error("Too many failed attempts. Try again later.");
    }
    return resp;
  } finally {
    clearTimeout(timeout);
  }
}

async function extractError(resp: Response): Promise<string> {
  const text = await resp.text().catch(() => "");
  try {
    const data = JSON.parse(text);
    if (data && typeof data === "object" && "error" in data) {
      return (data as { error: string }).error;
    }
  } catch {
    // not JSON
  }
  return text || `HTTP ${resp.status}`;
}

export async function apiGet<T>(path: string): Promise<T> {
  const resp = await apiFetch(path);
  if (!resp.ok) throw new Error(await extractError(resp));
  return resp.json();
}

export async function apiPost<T>(path: string, body?: unknown, extraHeaders?: Record<string, string>): Promise<T> {
  const resp = await apiFetch(path, {
    method: "POST",
    body: body != null ? JSON.stringify(body) : undefined,
    headers: extraHeaders,
  });
  if (!resp.ok) throw new Error(await extractError(resp));
  return resp.json();
}

export async function apiPut<T>(path: string, body?: unknown): Promise<T> {
  const resp = await apiFetch(path, {
    method: "PUT",
    body: body != null ? JSON.stringify(body) : undefined,
  });
  if (!resp.ok) throw new Error(await extractError(resp));
  return resp.json();
}

export async function apiPatch<T>(path: string, body?: unknown): Promise<T> {
  const resp = await apiFetch(path, {
    method: "PATCH",
    body: body != null ? JSON.stringify(body) : undefined,
  });
  if (!resp.ok) throw new Error(await extractError(resp));
  return resp.json();
}

export async function apiDelete(path: string): Promise<void> {
  const resp = await apiFetch(path, { method: "DELETE" });
  if (!resp.ok) throw new Error(await extractError(resp));
}

/** Like apiFetch but does NOT set Content-Type — caller controls headers (FormData, blob, etc.). */
export async function apiFetchRaw(path: string, init?: RequestInit): Promise<Response> {
  if (redirecting) throw new Error("Session expired");
  const token = getToken();
  if (!token) {
    handleUnauthorized();
    throw new Error("Session expired");
  }
  const headers: Record<string, string> = {
    ...(init?.headers as Record<string, string>),
  };
  headers["Authorization"] = `Bearer ${token}`;
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), REQUEST_TIMEOUT);
  try {
    const signal = init?.signal
      ? AbortSignal.any([init.signal, controller.signal])
      : controller.signal;
    const resp = await fetch(path, { ...init, headers, signal });
    if (resp.status === 401) {
      handleUnauthorized();
      throw new Error("Session expired");
    }
    if (resp.status === 429) {
      throw new Error("Too many failed attempts. Try again later.");
    }
    return resp;
  } finally {
    clearTimeout(timeout);
  }
}

/** GET that returns a Blob (media, file downloads). */
export async function apiGetBlob(path: string, extraHeaders?: Record<string, string>): Promise<Blob> {
  const resp = await apiFetchRaw(path, { method: "GET", headers: extraHeaders });
  if (!resp.ok) throw new Error(await extractError(resp));
  return resp.blob();
}

/** POST with FormData (file uploads). Does NOT set Content-Type. */
export async function apiPostFormData<T>(path: string, formData: FormData, extraHeaders?: Record<string, string>): Promise<T> {
  const resp = await apiFetchRaw(path, { method: "POST", body: formData, headers: extraHeaders });
  if (!resp.ok) throw new Error(await extractError(resp));
  return resp.json();
}

export async function decideApproval(
  approvalId: string,
  action: "approved" | "rejected",
  modifiedInput?: Record<string, unknown>,
): Promise<{ ok: boolean; error?: string }> {
  const body: Record<string, unknown> = {
    status: action,
    resolved_by: "chat-ui",
  };
  if (modifiedInput) {
    body.modified_input = modifiedInput;
  }
  try {
    const resp = await apiFetch(`/api/approvals/${approvalId}/resolve`, {
      method: "POST",
      body: JSON.stringify(body),
    });
    if (!resp.ok) {
      const err = await extractError(resp);
      return { ok: false, error: err };
    }
    return { ok: true };
  } catch (e) {
    return { ok: false, error: e instanceof Error ? e.message : "Unknown error" };
  }
}

export async function submitClarify(
  clarifyId: string,
  response: string,
): Promise<{ ok: boolean; error?: string }> {
  try {
    const resp = await apiFetch(`/api/clarify/${clarifyId}`, {
      method: "POST",
      body: JSON.stringify({ response }),
    });
    if (!resp.ok) {
      const err = await extractError(resp);
      return { ok: false, error: err };
    }
    return { ok: true };
  } catch (e) {
    return { ok: false, error: e instanceof Error ? e.message : "Unknown error" };
  }
}

export async function inviteAgent(
  sessionId: string,
  ownerAgent: string,
  agentName: string,
): Promise<{ participants: string[] }> {
  // Audit 2026-05-08 (7th pass): backend now requires ?agent=<owner> on invite.
  // Without it the call returns 400.
  return apiPost<{ participants: string[] }>(
    `/api/sessions/${sessionId}/invite?agent=${encodeURIComponent(ownerAgent)}`,
    { agent_name: agentName },
  );
}

// ── Checkpoints ──────────────────────────────────────────────────────────────

export const listCheckpoints = (agent: string) =>
  apiGet<CheckpointListDto>(`/api/agents/${encodeURIComponent(agent)}/checkpoints`);

export const diffCheckpoint = (agent: string, n: number) =>
  apiGet<{ diff: string }>(`/api/agents/${encodeURIComponent(agent)}/checkpoints/${n}/diff`);

export const restoreCheckpoint = (agent: string, n: number, file?: string) =>
  apiPost<RestoreReportDto>(
    `/api/agents/${encodeURIComponent(agent)}/checkpoints/${n}/restore`,
    file ? { file } : {},
  );
