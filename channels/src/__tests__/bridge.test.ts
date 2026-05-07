import { describe, test, expect, mock, beforeEach, afterEach } from "bun:test";
import { BridgeHandle } from "../bridge";
import { reUploadAttachments } from "../drivers/common";

describe("BridgeHandle", () => {
  let sent: string[];
  let bridge: BridgeHandle;

  beforeEach(() => {
    sent = [];
    bridge = new BridgeHandle(
      (msg) => sent.push(msg),
      "http://localhost:18789",
      "test-token",
      "main",
    );
  });

  // ── sendMessage ─────────────────────────────────────────────────────

  test("sendMessage sends inbound message and resolves on done", async () => {
    const { requestId, result } = bridge.sendMessage({
      user_id: "123",
      text: "hi",
      attachments: [],
      context: {},
      timestamp: new Date().toISOString(),
    });

    expect(sent).toHaveLength(1);
    const parsed = JSON.parse(sent[0]);
    expect(parsed.type).toBe("message");
    expect(parsed.request_id).toBe(requestId);
    expect(parsed.msg.user_id).toBe("123");
    expect(parsed.msg.text).toBe("hi");

    bridge.handleOutbound({ type: "done", request_id: requestId, text: "response" });
    const response = await result;
    expect(response).toBe("response");
  });

  test("sendMessage accumulates chunks via onChunk", async () => {
    const chunks: string[] = [];
    const { requestId, onChunk, result } = bridge.sendMessage({
      user_id: "123",
      text: "hi",
      attachments: [],
      context: {},
      timestamp: new Date().toISOString(),
    });

    onChunk((text) => chunks.push(text));

    bridge.handleOutbound({ type: "chunk", request_id: requestId, text: "hello " });
    bridge.handleOutbound({ type: "chunk", request_id: requestId, text: "world" });
    bridge.handleOutbound({ type: "done", request_id: requestId, text: "hello world" });

    await result;
    expect(chunks).toEqual(["hello ", "world"]);
  });

  test("sendMessage delivers phases via onPhase", async () => {
    const phases: Array<{ phase: string; toolName?: string }> = [];
    const { requestId, onPhase, result } = bridge.sendMessage({
      user_id: "123",
      text: "hi",
      attachments: [],
      context: {},
      timestamp: new Date().toISOString(),
    });

    onPhase((phase, toolName) => phases.push({ phase, toolName }));

    bridge.handleOutbound({ type: "phase", request_id: requestId, phase: "thinking", tool_name: null });
    bridge.handleOutbound({
      type: "phase",
      request_id: requestId,
      phase: "calling_tool",
      tool_name: "web_search",
    });
    bridge.handleOutbound({ type: "done", request_id: requestId, text: "done" });

    await result;
    expect(phases).toEqual([
      { phase: "thinking", toolName: undefined },
      { phase: "calling_tool", toolName: "web_search" },
    ]);
  });

  test("sendMessage rejects on error", async () => {
    const { requestId, result } = bridge.sendMessage({
      user_id: "123",
      text: "hi",
      attachments: [],
      context: {},
      timestamp: new Date().toISOString(),
    });

    bridge.handleOutbound({ type: "error", request_id: requestId, message: "fail" });

    try {
      await result;
      expect(true).toBe(false); // should not reach
    } catch (e: any) {
      expect(e.message).toBe("fail");
    }
  });

  // ── checkAccess ─────────────────────────────────────────────────────

  test("checkAccess resolves with access_result", async () => {
    const promise = bridge.checkAccess("user-1");

    // Find the sent access_check message
    expect(sent).toHaveLength(1);
    const parsed = JSON.parse(sent[0]);
    expect(parsed.type).toBe("access_check");
    expect(parsed.user_id).toBe("user-1");

    bridge.handleOutbound({
      type: "access_result",
      request_id: parsed.request_id,
      allowed: true,
      is_owner: false,
    });

    const result = await promise;
    expect(result.allowed).toBe(true);
    expect(result.isOwner).toBe(false);
  });

  test("checkAccess owner flag", async () => {
    const promise = bridge.checkAccess("owner-1");
    const parsed = JSON.parse(sent[0]);

    bridge.handleOutbound({
      type: "access_result",
      request_id: parsed.request_id,
      allowed: true,
      is_owner: true,
    });

    const result = await promise;
    expect(result.allowed).toBe(true);
    expect(result.isOwner).toBe(true);
  });

  // ── createPairingCode ───────────────────────────────────────────────

  test("createPairingCode sends pairing_create and resolves with code", async () => {
    const promise = bridge.createPairingCode("user-1", "John");

    const parsed = JSON.parse(sent[0]);
    expect(parsed.type).toBe("pairing_create");
    expect(parsed.user_id).toBe("user-1");
    expect(parsed.display_name).toBe("John");

    bridge.handleOutbound({
      type: "pairing_code",
      request_id: parsed.request_id,
      code: "ABC123",
    });

    const code = await promise;
    expect(code).toBe("ABC123");
  });

  // ── approvePairing ──────────────────────────────────────────────────

  test("approvePairing sends pairing_approve and resolves", async () => {
    const promise = bridge.approvePairing("ABC123");

    const parsed = JSON.parse(sent[0]);
    expect(parsed.type).toBe("pairing_approve");
    expect(parsed.code).toBe("ABC123");

    bridge.handleOutbound({
      type: "pairing_result",
      request_id: parsed.request_id,
      success: true,
      error: null,
    });

    const result = await promise;
    expect(result.success).toBe(true);
  });

  // ── rejectPairing ───────────────────────────────────────────────────

  test("rejectPairing sends pairing_reject", () => {
    bridge.rejectPairing("ABC123");

    const parsed = JSON.parse(sent[0]);
    expect(parsed.type).toBe("pairing_reject");
    expect(parsed.code).toBe("ABC123");
  });

  // ── cancelRequest ───────────────────────────────────────────────────

  test("cancelRequest sends cancel and rejects pending", async () => {
    const { requestId, result } = bridge.sendMessage({
      user_id: "123",
      text: "hi",
      attachments: [],
      context: {},
      timestamp: new Date().toISOString(),
    });

    bridge.cancelRequest(requestId);

    // Should have sent both message and cancel
    expect(sent).toHaveLength(2);
    const cancelMsg = JSON.parse(sent[1]);
    expect(cancelMsg.type).toBe("cancel");
    expect(cancelMsg.request_id).toBe(requestId);

    try {
      await result;
      expect(true).toBe(false);
    } catch (e: any) {
      expect(e.message).toBe("cancelled");
    }
  });

  // ── sendActionResult ────────────────────────────────────────────────

  test("sendActionResult sends action_result", () => {
    bridge.sendActionResult("a1", true);

    const parsed = JSON.parse(sent[0]);
    expect(parsed.type).toBe("action_result");
    expect(parsed.action_id).toBe("a1");
    expect(parsed.success).toBe(true);
    expect(parsed.error).toBeUndefined();
  });

  test("sendActionResult with error", () => {
    bridge.sendActionResult("a1", false, "something failed");

    const parsed = JSON.parse(sent[0]);
    expect(parsed.success).toBe(false);
    expect(parsed.error).toBe("something failed");
  });

  // ── handleOutbound edge cases ───────────────────────────────────────

  test("handleOutbound ignores unknown request_id", () => {
    // Should not throw
    bridge.handleOutbound({ type: "done", request_id: "unknown", text: "x" });
    bridge.handleOutbound({ type: "chunk", request_id: "unknown", text: "x" });
    bridge.handleOutbound({
      type: "error",
      request_id: "unknown",
      message: "x",
    });
  });

  test("handleOutbound handles pong without crash", () => {
    bridge.handleOutbound({ type: "pong" });
  });

  test("handleOutbound handles reload without crash", () => {
    bridge.handleOutbound({ type: "reload" });
  });

  test("handleOutbound handles config without crash", () => {
    bridge.handleOutbound({
      type: "config",
      language: "ru",
      owner_id: "123",
      typing_mode: "realistic",
    });
  });

  test("handleOutbound returns action for channel action", () => {
    const result = bridge.handleOutbound({
      type: "action",
      action_id: "a1",
      action: {
        action: "react",
        params: { emoji: "👍" },
        context: { chat_id: 123, message_id: 42 },
      },
    });

    expect(result).not.toBeNull();
    expect(result!.actionId).toBe("a1");
    expect(result!.action.action).toBe("react");
    expect((result!.action.params as Record<string, unknown>).emoji).toBe("👍");
  });

  // ── owner_id ────────────────────────────────────────────────────────

  test("owner_id management", () => {
    expect(bridge.ownerId).toBeUndefined();
    bridge.setOwnerId("123456789");
    expect(bridge.ownerId).toBe("123456789");
    bridge.setOwnerId(undefined);
    expect(bridge.ownerId).toBeUndefined();
  });

  // ── ping ────────────────────────────────────────────────────────────

  test("sendPing sends ping message", () => {
    bridge.sendPing();
    const parsed = JSON.parse(sent[0]);
    expect(parsed.type).toBe("ping");
  });

  // ── ready ───────────────────────────────────────────────────────────

  test("sendReady sends ready message", () => {
    bridge.sendReady("telegram", "1.0.0");
    const parsed = JSON.parse(sent[0]);
    expect(parsed.type).toBe("ready");
    expect(parsed.adapter_type).toBe("telegram");
    expect(parsed.version).toBe("1.0.0");
    expect(parsed.formatting_prompt).toBeUndefined();
  });

  test("sendReady sends formatting_prompt when provided", () => {
    bridge.sendReady("telegram", "1.0.0", "Format rules...");
    const parsed = JSON.parse(sent[0]);
    expect(parsed.type).toBe("ready");
    expect(parsed.formatting_prompt).toBe("Format rules...");
  });

  // ── listUsers error propagation ─────────────────────────────────────

  describe("listUsers error propagation", () => {
    const originalFetch = globalThis.fetch;
    afterEach(() => {
      globalThis.fetch = originalFetch;
    });

    test("throws on HTTP error status", async () => {
      globalThis.fetch = (async () => ({ ok: false, status: 403 })) as any;
      await expect(bridge.listUsers()).rejects.toThrow("listUsers failed: HTTP 403");
    });

    test("throws on fetch failure", async () => {
      globalThis.fetch = (() => Promise.reject(new Error("network down"))) as any;
      await expect(bridge.listUsers()).rejects.toThrow("network down");
    });
  });

  // ── revokeUser error propagation ────────────────────────────────────

  describe("revokeUser error propagation", () => {
    const originalFetch = globalThis.fetch;
    afterEach(() => {
      globalThis.fetch = originalFetch;
    });

    test("throws on HTTP error status", async () => {
      globalThis.fetch = (async () => ({ ok: false, status: 500 })) as any;
      await expect(bridge.revokeUser("u1")).rejects.toThrow("revokeUser failed: HTTP 500");
    });

    test("throws on fetch failure", async () => {
      globalThis.fetch = (() => Promise.reject(new Error("connection refused"))) as any;
      await expect(bridge.revokeUser("u1")).rejects.toThrow("connection refused");
    });
  });

  // ── uploadMedia error propagation ───────────────────────────────────

  describe("uploadMedia error propagation", () => {
    const originalFetch = globalThis.fetch;
    afterEach(() => {
      globalThis.fetch = originalFetch;
    });

    test("throws on download failure (non-ok status)", async () => {
      globalThis.fetch = (async () => ({ ok: false, status: 404 })) as any;
      await expect(bridge.uploadMedia("http://example.com/img.png", "img.png"))
        .rejects.toThrow("uploadMedia download failed: HTTP 404");
    });

    test("throws on upload failure (non-ok status)", async () => {
      let callCount = 0;
      globalThis.fetch = (async () => {
        callCount++;
        if (callCount === 1) {
          // Download succeeds
          return { ok: true, blob: async () => new Blob(["data"]) };
        }
        // Upload fails
        return { ok: false, status: 413 };
      }) as any;
      await expect(bridge.uploadMedia("http://example.com/img.png", "img.png"))
        .rejects.toThrow("uploadMedia upload failed: HTTP 413");
    });

    test("throws on network error", async () => {
      globalThis.fetch = (() => Promise.reject(new Error("DNS resolution failed"))) as any;
      await expect(bridge.uploadMedia("http://example.com/img.png", "img.png"))
        .rejects.toThrow("DNS resolution failed");
    });
  });

  // ── reUploadAttachments error propagation ───────────────────────────

  describe("reUploadAttachments error propagation", () => {
    test("propagates uploadMedia errors", async () => {
      const fakeBridge = {
        uploadMedia: async () => { throw new Error("uploadMedia download failed: HTTP 500"); },
      };
      const atts = [{ url: "http://example.com/a.png", media_type: "image" as const, file_name: "a.png" }];
      await expect(reUploadAttachments(fakeBridge, atts)).rejects.toThrow("uploadMedia download failed: HTTP 500");
    });
  });
});
