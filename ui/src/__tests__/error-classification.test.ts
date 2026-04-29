import { describe, it, expect } from "vitest";
import { classifyStreamError } from "@/app/(authenticated)/chat/error/ErrorBanner";

describe("classifyStreamError", () => {
  // ── connection_lost ───────────────────────────────────────────────────────

  it("classifies 'Connection lost after retries' as connection_lost", () => {
    expect(classifyStreamError("Connection lost after retries")).toBe("connection_lost");
  });

  it("classifies 'Failed to fetch' as connection_lost", () => {
    expect(classifyStreamError("Failed to fetch")).toBe("connection_lost");
  });

  it("classifies 'network error' as connection_lost", () => {
    expect(classifyStreamError("network error")).toBe("connection_lost");
  });

  it("classifies 'disconnected from server' as connection_lost", () => {
    expect(classifyStreamError("disconnected from server")).toBe("connection_lost");
  });

  it("classifies 'request aborted' as connection_lost", () => {
    expect(classifyStreamError("request aborted")).toBe("connection_lost");
  });

  // ── timeout ───────────────────────────────────────────────────────────────

  it("classifies 'LLM provider timeout' as timeout", () => {
    expect(classifyStreamError("LLM provider timeout")).toBe("timeout");
  });

  it("classifies 'timeout' as timeout", () => {
    expect(classifyStreamError("timeout")).toBe("timeout");
  });

  it("classifies 'request timed out' as timeout", () => {
    expect(classifyStreamError("request timed out")).toBe("timeout");
  });

  it("classifies 'TIMEOUT' (uppercase) as timeout", () => {
    expect(classifyStreamError("TIMEOUT")).toBe("timeout");
  });

  // ── api_error (default) ───────────────────────────────────────────────────

  it("classifies 'HTTP 500: Internal Server Error' as api_error", () => {
    expect(classifyStreamError("HTTP 500: Internal Server Error")).toBe("api_error");
  });

  it("classifies 'Rate limited (429)' as api_error", () => {
    expect(classifyStreamError("Rate limited (429)")).toBe("api_error");
  });

  it("classifies unknown error as api_error (default)", () => {
    expect(classifyStreamError("Some unknown error")).toBe("api_error");
  });

  it("classifies empty string as api_error (default)", () => {
    expect(classifyStreamError("")).toBe("api_error");
  });

  it("classifies 'HTTP 401: Unauthorized' as api_error", () => {
    expect(classifyStreamError("HTTP 401: Unauthorized")).toBe("api_error");
  });

  it("classifies 'HTTP 403: Forbidden' as api_error", () => {
    expect(classifyStreamError("HTTP 403: Forbidden")).toBe("api_error");
  });
});
