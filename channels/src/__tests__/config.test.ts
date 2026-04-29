import { describe, test, expect } from "bun:test";
import { buildEnvConfig, wsToHttp } from "../config";

describe("buildEnvConfig", () => {
  test("reads defaults", () => {
    const env = buildEnvConfig({ HYDECLAW_AUTH_TOKEN: "token123" });
    expect(env.coreWs).toBe("ws://localhost:18789");
    expect(env.reconnectInterval).toBe(5);
    expect(env.authToken).toBe("token123");
  });

  test("reads overrides", () => {
    const env = buildEnvConfig({
      HYDECLAW_CORE_WS: "ws://localhost:9999",
      HYDECLAW_AUTH_TOKEN: "mytoken",
      RECONNECT_INTERVAL: "10",
    });
    expect(env.coreWs).toBe("ws://localhost:9999");
    expect(env.reconnectInterval).toBe(10);
  });

  test("requires HYDECLAW_AUTH_TOKEN", () => {
    expect(() => buildEnvConfig({})).toThrow("HYDECLAW_AUTH_TOKEN");
  });
});

describe("wsToHttp", () => {
  test("converts ws to http", () => {
    expect(wsToHttp("ws://localhost:18789")).toBe("http://localhost:18789");
  });

  test("converts wss to https", () => {
    expect(wsToHttp("wss://example.com/path")).toBe("https://example.com/path");
  });

  test("preserves path and query", () => {
    expect(wsToHttp("ws://host:8080/api?q=1")).toBe("http://host:8080/api?q=1");
  });

  test("passes http:// through unchanged", () => {
    expect(wsToHttp("http://localhost:18789")).toBe("http://localhost:18789");
  });

  test("passes https:// through unchanged", () => {
    expect(wsToHttp("https://example.com")).toBe("https://example.com");
  });
});
