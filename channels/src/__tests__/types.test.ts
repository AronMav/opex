import { describe, test, expect } from "bun:test";
import type { ChannelInbound, ChannelOutbound } from "../types";

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
