/**
 * BridgeHandle — manages pending WS requests between channel drivers and core.
 * Port of crates/hydeclaw-channel/src/bridge.rs
 */

import type {
  ChannelOutbound,
  ChannelActionDto,
  IncomingMessageDto,
} from "./types";

interface PendingRequest {
  chunkCb?: (text: string) => void;
  phaseCb?: (phase: string, toolName?: string) => void;
  resolve: (text: string) => void;
  reject: (err: Error) => void;
}

export interface UserEntry {
  channel_user_id: string;
  display_name?: string;
  approved_at?: string;
}

export interface OutboundAction {
  actionId: string;
  action: ChannelActionDto;
}

type SendFn = (json: string) => void;

let idCounter = 0;
function genRequestId(): string {
  return `req-${Date.now()}-${++idCounter}`;
}

export class BridgeHandle {
  private sendFn: SendFn;
  private coreHttpUrl: string;
  private authToken: string;
  private agentName: string;

  private pendingRequests = new Map<string, PendingRequest>();
  private pendingAccess = new Map<
    string,
    (result: { allowed: boolean; isOwner: boolean }) => void
  >();
  private pendingPairing = new Map<string, (code: string) => void>();
  private pendingPairingOps = new Map<
    string,
    (result: { success: boolean; error?: string }) => void
  >();

  ownerId: string | undefined;

  constructor(
    sendFn: SendFn,
    coreHttpUrl: string,
    authToken: string,
    agentName: string,
  ) {
    this.sendFn = sendFn;
    this.coreHttpUrl = coreHttpUrl;
    this.authToken = authToken;
    this.agentName = agentName;
  }

  setOwnerId(id: string | undefined): void {
    this.ownerId = id;
  }

  /** Send a message to core. Returns requestId, chunk/phase listeners, and result promise. */
  sendMessage(msg: IncomingMessageDto): {
    requestId: string;
    onChunk: (cb: (text: string) => void) => void;
    onPhase: (cb: (phase: string, toolName?: string) => void) => void;
    result: Promise<string>;
  } {
    const requestId = genRequestId();

    let pendingRef: PendingRequest;
    const result = new Promise<string>((resolve, reject) => {
      pendingRef = { resolve, reject };
      this.pendingRequests.set(requestId, pendingRef);

      // Timeout: reject if no response within 5 minutes
      setTimeout(() => {
        if (this.pendingRequests.has(requestId)) {
          this.pendingRequests.delete(requestId);
          reject(new Error("Request timeout (300s)"));
        }
      }, 300_000);
    });

    this.send({
      type: "message",
      request_id: requestId,
      msg,
    });

    return {
      requestId,
      onChunk: (cb) => {
        const pending = this.pendingRequests.get(requestId);
        if (pending) pending.chunkCb = cb;
      },
      onPhase: (cb) => {
        const pending = this.pendingRequests.get(requestId);
        if (pending) pending.phaseCb = cb;
      },
      result,
    };
  }

  /** Check access for a user via core WebSocket. Fail-closed after 10 s if Core doesn't respond. */
  checkAccess(userId: string): Promise<{ allowed: boolean; isOwner: boolean }> {
    const requestId = genRequestId();

    const promise = new Promise<{ allowed: boolean; isOwner: boolean }>((resolve) => {
      this.pendingAccess.set(requestId, resolve);

      setTimeout(() => {
        if (this.pendingAccess.has(requestId)) {
          this.pendingAccess.delete(requestId);
          console.warn(`[bridge] access check timeout for user ${userId}`);
          resolve({ allowed: false, isOwner: false }); // fail-closed
        }
      }, 10_000);
    });

    this.send({
      type: "access_check",
      request_id: requestId,
      user_id: userId,
    });

    return promise;
  }

  /** Create a pairing code via core WebSocket. */
  createPairingCode(
    userId: string,
    displayName?: string,
  ): Promise<string> {
    const requestId = genRequestId();

    const promise = new Promise<string>((resolve) => {
      this.pendingPairing.set(requestId, resolve);
    });

    this.send({
      type: "pairing_create",
      request_id: requestId,
      user_id: userId,
      display_name: displayName,
    });

    return promise;
  }

  /** Approve a pairing via core WebSocket. */
  approvePairing(
    code: string,
  ): Promise<{ success: boolean; error?: string }> {
    const requestId = genRequestId();

    const promise = new Promise<{ success: boolean; error?: string }>(
      (resolve) => {
        this.pendingPairingOps.set(requestId, resolve);
      },
    );

    this.send({
      type: "pairing_approve",
      request_id: requestId,
      code,
    });

    return promise;
  }

  /** Reject a pairing via core WebSocket. */
  rejectPairing(code: string): void {
    this.send({
      type: "pairing_reject",
      request_id: genRequestId(),
      code,
    });
  }

  /** Cancel an in-flight request. */
  cancelRequest(requestId: string): void {
    this.send({
      type: "cancel",
      request_id: requestId,
    });

    const pending = this.pendingRequests.get(requestId);
    if (pending) {
      this.pendingRequests.delete(requestId);
      pending.reject(new Error("cancelled"));
    }
  }

  /** Send action result back to core. */
  sendActionResult(
    actionId: string,
    success: boolean,
    error?: string,
  ): void {
    this.send({
      type: "action_result",
      action_id: actionId,
      success,
      error,
    });
  }

  /** Send ping. */
  sendPing(): void {
    this.send({ type: "ping" });
  }

  /** Send ready handshake with optional channel-specific formatting prompt. */
  sendReady(adapterType: string, version: string, formattingPrompt?: string): void {
    this.send({
      type: "ready",
      adapter_type: adapterType,
      version,
      ...(formattingPrompt ? { formatting_prompt: formattingPrompt } : {}),
    });
  }

  /**
   * Dispatch an outbound message from core.
   * Returns an OutboundAction if the message is a channel action, null otherwise.
   */
  handleOutbound(msg: ChannelOutbound): OutboundAction | null {
    switch (msg.type) {
      case "chunk": {
        const pending = this.pendingRequests.get(msg.request_id);
        if (pending?.chunkCb) pending.chunkCb(msg.text);
        return null;
      }
      case "phase": {
        const pending = this.pendingRequests.get(msg.request_id);
        if (pending?.phaseCb) pending.phaseCb(msg.phase, msg.tool_name);
        return null;
      }
      case "done": {
        const pending = this.pendingRequests.get(msg.request_id);
        if (pending) {
          this.pendingRequests.delete(msg.request_id);
          pending.resolve(msg.text);
        }
        return null;
      }
      case "error": {
        const pending = this.pendingRequests.get(msg.request_id);
        if (pending) {
          this.pendingRequests.delete(msg.request_id);
          pending.reject(new Error(msg.message));
        }
        return null;
      }
      case "action":
        return { actionId: msg.action_id, action: msg.action };
      case "access_result": {
        const resolve = this.pendingAccess.get(msg.request_id);
        if (resolve) {
          this.pendingAccess.delete(msg.request_id);
          resolve({ allowed: msg.allowed, isOwner: msg.is_owner });
        }
        return null;
      }
      case "pairing_code": {
        const resolve = this.pendingPairing.get(msg.request_id);
        if (resolve) {
          this.pendingPairing.delete(msg.request_id);
          resolve(msg.code);
        }
        return null;
      }
      case "pairing_result": {
        const resolve = this.pendingPairingOps.get(msg.request_id);
        if (resolve) {
          this.pendingPairingOps.delete(msg.request_id);
          resolve({ success: msg.success, error: msg.error });
        }
        return null;
      }
      case "pong":
      case "reload":
      case "config":
        return null;
    }
  }

  /** Reject all pending promises and clear maps (called on session disconnect). */
  clearAll(): void {
    const err = new Error("session closed");
    for (const [, p] of this.pendingRequests) p.reject(err);
    // Fail-closed: deny access on disconnect rather than hanging forever.
    for (const [, resolve] of this.pendingAccess) resolve({ allowed: false, isOwner: false });
    this.pendingRequests.clear();
    this.pendingAccess.clear();
    this.pendingPairing.clear();
    this.pendingPairingOps.clear();
  }

  /** List allowed users via core HTTP API. */
  async listUsers(): Promise<UserEntry[]> {
    const url = `${this.coreHttpUrl}/api/access/${this.agentName}/users`;
    const resp = await fetch(url, {
      headers: { Authorization: `Bearer ${this.authToken}` },
    });
    if (!resp.ok) throw new Error(`listUsers failed: HTTP ${resp.status}`);
    return (await resp.json()) as UserEntry[];
  }

  /** Revoke a user via core HTTP API. */
  async revokeUser(userId: string): Promise<boolean> {
    const url = `${this.coreHttpUrl}/api/access/${this.agentName}/users/${userId}`;
    const resp = await fetch(url, {
      method: "DELETE",
      headers: { Authorization: `Bearer ${this.authToken}` },
    });
    if (!resp.ok) throw new Error(`revokeUser failed: HTTP ${resp.status}`);
    return true;
  }

  /** Upload media: download from sourceUrl then upload to core. */
  async uploadMedia(
    sourceUrl: string,
    filename: string,
    authHeader?: string,
  ): Promise<string> {
    const headers: Record<string, string> = {};
    if (authHeader) headers.Authorization = authHeader;

    const dlResp = await fetch(sourceUrl, { headers });
    if (!dlResp.ok) throw new Error(`uploadMedia download failed: HTTP ${dlResp.status}`);
    const blob = await dlResp.blob();

    const form = new FormData();
    form.append("file", blob, filename);

    const uploadResp = await fetch(
      `${this.coreHttpUrl}/api/media/upload`,
      {
        method: "POST",
        headers: { Authorization: `Bearer ${this.authToken}` },
        body: form,
      },
    );
    if (!uploadResp.ok) throw new Error(`uploadMedia upload failed: HTTP ${uploadResp.status}`);

    const data = (await uploadResp.json()) as { url?: string };
    return data.url ?? "";
  }

  private send(msg: Record<string, unknown>): void {
    this.sendFn(JSON.stringify(msg));
  }
}
