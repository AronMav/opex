import { describe, test, expect } from "bun:test";
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import type { ChannelInbound, ChannelOutbound } from "../types";

const __filename_for_fixtures = fileURLToPath(import.meta.url);
const FIXTURES = join(dirname(__filename_for_fixtures), "fixtures");

describe("ChannelInbound serialization", () => {
  test("serializes message", () => {
    const msg: ChannelInbound = {
      type: "message",
      request_id: "req-1",
      msg: {
        user_id: "123",
        text: "hello",
        attachments: [],
        context: { chat_id: 456, message_id: 789 },
        timestamp: "2026-01-01T00:00:00Z",
      },
    };
    const json = JSON.stringify(msg);
    const parsed = JSON.parse(json);
    expect(parsed.type).toBe("message");
    expect(parsed.request_id).toBe("req-1");
    expect(parsed.msg.user_id).toBe("123");
    expect((parsed.msg.context as Record<string, unknown>).chat_id).toBe(456);
  });

  test("serializes ping", () => {
    const ping: ChannelInbound = { type: "ping" };
    expect(JSON.stringify(ping)).toBe('{"type":"ping"}');
  });

  test("serializes ready", () => {
    const ready: ChannelInbound = { type: "ready", adapter_type: "telegram", version: "1.0.0" };
    const parsed = JSON.parse(JSON.stringify(ready));
    expect(parsed.type).toBe("ready");
    expect(parsed.adapter_type).toBe("telegram");
  });

  test("serializes ready with formatting_prompt", () => {
    const ready: ChannelInbound = { type: "ready", adapter_type: "discord", version: "2.0", formatting_prompt: "rules" };
    const parsed = JSON.parse(JSON.stringify(ready));
    expect(parsed.formatting_prompt).toBe("rules");
  });

  test("serializes cancel", () => {
    const cancel: ChannelInbound = { type: "cancel", request_id: "req-42" };
    const parsed = JSON.parse(JSON.stringify(cancel));
    expect(parsed.type).toBe("cancel");
    expect(parsed.request_id).toBe("req-42");
  });

  test("serializes access_check", () => {
    const msg: ChannelInbound = { type: "access_check", request_id: "r1", user_id: "u1" };
    const parsed = JSON.parse(JSON.stringify(msg));
    expect(parsed.type).toBe("access_check");
    expect(parsed.user_id).toBe("u1");
  });

  test("serializes pairing_create", () => {
    const msg: ChannelInbound = {
      type: "pairing_create",
      request_id: "r1",
      user_id: "u1",
      display_name: "John",
    };
    const parsed = JSON.parse(JSON.stringify(msg));
    expect(parsed.display_name).toBe("John");
  });

  test("serializes action_result", () => {
    const msg: ChannelInbound = {
      type: "action_result",
      action_id: "a1",
      success: true,
      error: null,
    };
    const parsed = JSON.parse(JSON.stringify(msg));
    expect(parsed.success).toBe(true);
    expect(parsed.error).toBeUndefined();
  });
});

describe("ChannelOutbound deserialization", () => {
  test("deserializes chunk", () => {
    const json = '{"type":"chunk","request_id":"r1","text":"hello"}';
    const msg = JSON.parse(json) as ChannelOutbound;
    expect(msg.type).toBe("chunk");
    if (msg.type === "chunk") {
      expect(msg.text).toBe("hello");
      expect(msg.request_id).toBe("r1");
    }
  });

  test("deserializes done", () => {
    const json = '{"type":"done","request_id":"r1","text":"full response"}';
    const msg = JSON.parse(json) as ChannelOutbound;
    expect(msg.type).toBe("done");
    if (msg.type === "done") {
      expect(msg.text).toBe("full response");
    }
  });

  test("deserializes error", () => {
    const json = '{"type":"error","request_id":"r1","message":"something went wrong"}';
    const msg = JSON.parse(json) as ChannelOutbound;
    expect(msg.type).toBe("error");
    if (msg.type === "error") {
      expect(msg.message).toBe("something went wrong");
    }
  });

  test("deserializes config", () => {
    const json = '{"type":"config","language":"ru","owner_id":"123456789","typing_mode":"realistic"}';
    const msg = JSON.parse(json) as ChannelOutbound;
    expect(msg.type).toBe("config");
    if (msg.type === "config") {
      expect(msg.language).toBe("ru");
      expect(msg.owner_id).toBe("123456789");
      expect(msg.typing_mode).toBe("realistic");
    }
  });

  test("deserializes action", () => {
    const json = JSON.stringify({
      type: "action",
      action_id: "a1",
      action: {
        action: "react",
        params: { emoji: "👍" },
        context: { chat_id: 123, message_id: 42 },
      },
    });
    const msg = JSON.parse(json) as ChannelOutbound;
    expect(msg.type).toBe("action");
    if (msg.type === "action") {
      expect(msg.action.action).toBe("react");
      expect((msg.action.params as Record<string, unknown>).emoji).toBe("👍");
    }
  });

  test("deserializes access_result", () => {
    const json = '{"type":"access_result","request_id":"r1","allowed":true,"is_owner":false}';
    const msg = JSON.parse(json) as ChannelOutbound;
    if (msg.type === "access_result") {
      expect(msg.allowed).toBe(true);
      expect(msg.is_owner).toBe(false);
    }
  });

  test("deserializes pong", () => {
    const msg = JSON.parse('{"type":"pong"}') as ChannelOutbound;
    expect(msg.type).toBe("pong");
  });

  test("deserializes reload", () => {
    const msg = JSON.parse('{"type":"reload"}') as ChannelOutbound;
    expect(msg.type).toBe("reload");
  });

  test("handles unknown type gracefully", () => {
    const json = '{"type":"future_type","data":123}';
    const msg = JSON.parse(json);
    expect(msg.type).toBe("future_type");
  });
});

describe("S6 Rust → TS round-trip via fixtures", () => {
  test("ChannelInbound::Message fixture parses and matches TS shape", () => {
    const raw = readFileSync(join(FIXTURES, "channel_inbound_message.json"), "utf-8");
    const parsed: ChannelInbound = JSON.parse(raw);

    expect(parsed.type).toBe("message");
    if (parsed.type !== "message") throw new Error("unreachable");

    expect(parsed.request_id).toBe("req-abc-123");
    expect(parsed.msg.user_id).toBe("user-42");
    expect(parsed.msg.display_name).toBe("Alice");
    expect(parsed.msg.text).toBe("Hello, world");
    expect(parsed.msg.attachments).toHaveLength(1);
    expect(parsed.msg.attachments[0].media_type).toBe("image");
    expect(parsed.msg.attachments[0].mime_type).toBe("image/png");
    expect(parsed.msg.attachments[0].file_name).toBe("image.png");
    expect(parsed.msg.attachments[0].file_size).toBe(12345);
    expect(parsed.msg.timestamp).toBe("2026-05-07T15:30:00Z");

    const ctx = parsed.msg.context as Record<string, unknown>;
    expect(ctx.chat_id).toBe("12345");

    const reSerialized = JSON.stringify(parsed);
    const reParsed = JSON.parse(reSerialized);
    expect(reParsed).toEqual(parsed);
  });

  test("ChannelInbound::Ping fixture", () => {
    const raw = readFileSync(join(FIXTURES, "channel_inbound_ping.json"), "utf-8");
    const parsed: ChannelInbound = JSON.parse(raw);
    expect(parsed.type).toBe("ping");
  });

  test("ChannelInbound::Cancel fixture", () => {
    const raw = readFileSync(join(FIXTURES, "channel_inbound_cancel.json"), "utf-8");
    const parsed: ChannelInbound = JSON.parse(raw);
    expect(parsed.type).toBe("cancel");
    if (parsed.type !== "cancel") throw new Error("unreachable");
    expect(parsed.request_id).toBe("req-cancel-42");
  });

  test("ChannelInbound::Ready fixture with formatting_prompt", () => {
    const raw = readFileSync(join(FIXTURES, "channel_inbound_ready.json"), "utf-8");
    const parsed: ChannelInbound = JSON.parse(raw);
    expect(parsed.type).toBe("ready");
    if (parsed.type !== "ready") throw new Error("unreachable");
    expect(parsed.adapter_type).toBe("telegram");
    expect(parsed.version).toBe("1.0.0");
    expect(parsed.formatting_prompt).toBe("Use emojis sparingly");
  });

  test("ChannelInbound::Ready fixture without formatting_prompt", () => {
    const raw = readFileSync(join(FIXTURES, "channel_inbound_ready_no_formatting.json"), "utf-8");
    const parsed: ChannelInbound = JSON.parse(raw);
    expect(parsed.type).toBe("ready");
    if (parsed.type !== "ready") throw new Error("unreachable");
    expect(parsed.adapter_type).toBe("discord");
    expect(parsed.formatting_prompt).toBeUndefined();
  });

  test("ChannelInbound::ActionResult success fixture", () => {
    const raw = readFileSync(join(FIXTURES, "channel_inbound_action_result_success.json"), "utf-8");
    const parsed: ChannelInbound = JSON.parse(raw);
    expect(parsed.type).toBe("action_result");
    if (parsed.type !== "action_result") throw new Error("unreachable");
    expect(parsed.action_id).toBe("action-001");
    expect(parsed.success).toBe(true);
    expect(parsed.error).toBeUndefined();
  });

  test("ChannelInbound::ActionResult error fixture", () => {
    const raw = readFileSync(join(FIXTURES, "channel_inbound_action_result_error.json"), "utf-8");
    const parsed: ChannelInbound = JSON.parse(raw);
    expect(parsed.type).toBe("action_result");
    if (parsed.type !== "action_result") throw new Error("unreachable");
    expect(parsed.action_id).toBe("action-002");
    expect(parsed.success).toBe(false);
    expect(parsed.error).toBe("Permission denied");
  });

  test("ChannelInbound::AccessCheck fixture", () => {
    const raw = readFileSync(join(FIXTURES, "channel_inbound_access_check.json"), "utf-8");
    const parsed: ChannelInbound = JSON.parse(raw);
    expect(parsed.type).toBe("access_check");
    if (parsed.type !== "access_check") throw new Error("unreachable");
    expect(parsed.request_id).toBe("req-access-1");
    expect(parsed.user_id).toBe("user-789");
  });

  test("ChannelInbound::PairingCreate fixture", () => {
    const raw = readFileSync(join(FIXTURES, "channel_inbound_pairing_create.json"), "utf-8");
    const parsed: ChannelInbound = JSON.parse(raw);
    expect(parsed.type).toBe("pairing_create");
    if (parsed.type !== "pairing_create") throw new Error("unreachable");
    expect(parsed.request_id).toBe("req-pair-1");
    expect(parsed.user_id).toBe("user-new");
    expect(parsed.display_name).toBe("Bob");
  });

  test("ChannelInbound::PairingCreate fixture without display_name", () => {
    const raw = readFileSync(join(FIXTURES, "channel_inbound_pairing_create_no_name.json"), "utf-8");
    const parsed: ChannelInbound = JSON.parse(raw);
    expect(parsed.type).toBe("pairing_create");
    if (parsed.type !== "pairing_create") throw new Error("unreachable");
    expect(parsed.display_name).toBeUndefined();
  });

  test("ChannelInbound::PairingApprove fixture", () => {
    const raw = readFileSync(join(FIXTURES, "channel_inbound_pairing_approve.json"), "utf-8");
    const parsed: ChannelInbound = JSON.parse(raw);
    expect(parsed.type).toBe("pairing_approve");
    if (parsed.type !== "pairing_approve") throw new Error("unreachable");
    expect(parsed.request_id).toBe("req-approve-1");
    expect(parsed.code).toBe("123456");
  });

  test("ChannelInbound::PairingReject fixture", () => {
    const raw = readFileSync(join(FIXTURES, "channel_inbound_pairing_reject.json"), "utf-8");
    const parsed: ChannelInbound = JSON.parse(raw);
    expect(parsed.type).toBe("pairing_reject");
    if (parsed.type !== "pairing_reject") throw new Error("unreachable");
    expect(parsed.request_id).toBe("req-reject-1");
    expect(parsed.code).toBe("654321");
  });

  test("ChannelOutbound::Action fixture parses and matches TS shape", () => {
    const raw = readFileSync(join(FIXTURES, "channel_outbound_action.json"), "utf-8");
    const parsed: ChannelOutbound = JSON.parse(raw);

    expect(parsed.type).toBe("action");
    if (parsed.type !== "action") throw new Error("unreachable");

    expect(parsed.action_id).toBe("action-xyz-789");
    expect(parsed.action.action).toBe("send_photo");

    const params = parsed.action.params as Record<string, unknown>;
    expect(params.url).toBe("https://example.com/x.jpg");

    const ctx = parsed.action.context as Record<string, unknown>;
    expect(ctx.chat_id).toBe("12345");
  });

  test("ChannelOutbound::Chunk fixture", () => {
    const raw = readFileSync(join(FIXTURES, "channel_outbound_chunk.json"), "utf-8");
    const parsed: ChannelOutbound = JSON.parse(raw);
    expect(parsed.type).toBe("chunk");
    if (parsed.type !== "chunk") throw new Error("unreachable");
    expect(parsed.request_id).toBe("req-stream-1");
    expect(parsed.text).toBe("Hello");
  });

  test("ChannelOutbound::Done fixture", () => {
    const raw = readFileSync(join(FIXTURES, "channel_outbound_done.json"), "utf-8");
    const parsed: ChannelOutbound = JSON.parse(raw);
    expect(parsed.type).toBe("done");
    if (parsed.type !== "done") throw new Error("unreachable");
    expect(parsed.request_id).toBe("req-done-1");
    expect(parsed.text).toBe("Final response text");
  });

  test("ChannelOutbound::Error fixture", () => {
    const raw = readFileSync(join(FIXTURES, "channel_outbound_error.json"), "utf-8");
    const parsed: ChannelOutbound = JSON.parse(raw);
    expect(parsed.type).toBe("error");
    if (parsed.type !== "error") throw new Error("unreachable");
    expect(parsed.request_id).toBe("req-err-1");
    expect(parsed.message).toBe("Tool execution failed");
  });

  test("ChannelOutbound::Phase fixture", () => {
    const raw = readFileSync(join(FIXTURES, "channel_outbound_phase.json"), "utf-8");
    const parsed: ChannelOutbound = JSON.parse(raw);
    expect(parsed.type).toBe("phase");
    if (parsed.type !== "phase") throw new Error("unreachable");
    expect(parsed.request_id).toBe("req-phase-1");
    expect(parsed.phase).toBe("thinking");
    expect(parsed.tool_name).toBe("web_search");
  });

  test("ChannelOutbound::Phase fixture without tool_name", () => {
    const raw = readFileSync(join(FIXTURES, "channel_outbound_phase_no_tool.json"), "utf-8");
    const parsed: ChannelOutbound = JSON.parse(raw);
    expect(parsed.type).toBe("phase");
    if (parsed.type !== "phase") throw new Error("unreachable");
    expect(parsed.tool_name).toBeUndefined();
  });

  test("ChannelOutbound::AccessResult fixture", () => {
    const raw = readFileSync(join(FIXTURES, "channel_outbound_access_result.json"), "utf-8");
    const parsed: ChannelOutbound = JSON.parse(raw);
    expect(parsed.type).toBe("access_result");
    if (parsed.type !== "access_result") throw new Error("unreachable");
    expect(parsed.request_id).toBe("req-acc-1");
    expect(parsed.allowed).toBe(true);
    expect(parsed.is_owner).toBe(true);
  });

  test("ChannelOutbound::PairingCode fixture", () => {
    const raw = readFileSync(join(FIXTURES, "channel_outbound_pairing_code.json"), "utf-8");
    const parsed: ChannelOutbound = JSON.parse(raw);
    expect(parsed.type).toBe("pairing_code");
    if (parsed.type !== "pairing_code") throw new Error("unreachable");
    expect(parsed.request_id).toBe("req-pair-code-1");
    expect(parsed.code).toBe("ABC123");
  });

  test("ChannelOutbound::PairingResult success fixture", () => {
    const raw = readFileSync(join(FIXTURES, "channel_outbound_pairing_result_success.json"), "utf-8");
    const parsed: ChannelOutbound = JSON.parse(raw);
    expect(parsed.type).toBe("pairing_result");
    if (parsed.type !== "pairing_result") throw new Error("unreachable");
    expect(parsed.success).toBe(true);
    expect(parsed.error).toBeUndefined();
  });

  test("ChannelOutbound::PairingResult error fixture", () => {
    const raw = readFileSync(join(FIXTURES, "channel_outbound_pairing_result_error.json"), "utf-8");
    const parsed: ChannelOutbound = JSON.parse(raw);
    expect(parsed.type).toBe("pairing_result");
    if (parsed.type !== "pairing_result") throw new Error("unreachable");
    expect(parsed.success).toBe(false);
    expect(parsed.error).toBe("Invalid code");
  });

  test("ChannelOutbound::Pong fixture", () => {
    const raw = readFileSync(join(FIXTURES, "channel_outbound_pong.json"), "utf-8");
    const parsed: ChannelOutbound = JSON.parse(raw);
    expect(parsed.type).toBe("pong");
  });

  test("ChannelOutbound::Reload fixture", () => {
    const raw = readFileSync(join(FIXTURES, "channel_outbound_reload.json"), "utf-8");
    const parsed: ChannelOutbound = JSON.parse(raw);
    expect(parsed.type).toBe("reload");
  });

  test("ChannelOutbound::Config fixture", () => {
    const raw = readFileSync(join(FIXTURES, "channel_outbound_config.json"), "utf-8");
    const parsed: ChannelOutbound = JSON.parse(raw);
    expect(parsed.type).toBe("config");
    if (parsed.type !== "config") throw new Error("unreachable");
    expect(parsed.language).toBe("ru");
    expect(parsed.owner_id).toBe("123456789");
    expect(parsed.typing_mode).toBe("thinking");
  });
});
