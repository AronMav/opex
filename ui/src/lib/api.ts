import { useAuthStore } from "@/stores/auth-store";
import type { CheckpointListDto, RestoreReportDto } from "@/types/api.generated";
import type { WorkspaceFile } from "@/types/api";

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
  const trimmed = text.trim();
  // HTML error pages (dev-server 404s, proxy/gateway errors) are noise in a
  // banner/toast — collapse them to the status code.
  if (!trimmed || /^<!doctype\b|^<html\b/i.test(trimmed)) return `HTTP ${resp.status}`;
  return trimmed.length > 300 ? `${trimmed.slice(0, 300)}…` : trimmed;
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

/** Add a tool (or `*`-glob pattern) to an agent's approval allowlist, so future
 *  matching calls skip the approval prompt. Backs the "Always allow" action. */
export async function addApprovalAllowlist(
  agentId: string,
  toolPattern: string,
): Promise<{ ok: boolean; error?: string }> {
  try {
    const resp = await apiFetch(`/api/approvals/allowlist`, {
      method: "POST",
      body: JSON.stringify({ agent_id: agentId, tool_pattern: toolPattern }),
    });
    if (!resp.ok) {
      return { ok: false, error: await extractError(resp) };
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

// ── Workspace API helpers ─────────────────────────────────────────────────────

export function isBinaryFile(
  r: WorkspaceFile,
): r is Extract<WorkspaceFile, { is_binary: true }> {
  return "is_binary" in r && r.is_binary === true;
}

export const signWorkspacePaths = (paths: string[]) =>
  apiPost<{ url_by_path: Record<string, string> }>("/api/workspace/sign", { paths }).then((r) => r.url_by_path);

export const wsMkdir = (path: string) => apiPost("/api/workspace/mkdir", { path });
export const wsRename = (from: string, to: string) => apiPost("/api/workspace/rename", { from, to });
export const wsDeleteRecursive = (path: string) =>
  apiDelete(`/api/workspace/${path.split("/").map(encodeURIComponent).join("/")}?recursive=true`);

export function wsUpload(dir: string, files: File[]) {
  const fd = new FormData();
  fd.append("dir", dir); // MUST be appended before files (backend reads dir first)
  for (const f of files) fd.append("file", f);
  return apiPostFormData<{ ok: boolean; saved: string[]; errors: string[] }>("/api/workspace/upload", fd);
}
