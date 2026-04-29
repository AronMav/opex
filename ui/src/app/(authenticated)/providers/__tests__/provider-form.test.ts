import { describe, it, expect } from "vitest";
import { buildProviderBody } from "../page";
import type { CreateProviderInput } from "@/types/api";

// ── Shared base form ──────────────────────────────────────────────────────────

const BASE_FORM: CreateProviderInput = {
  name: "my-provider",
  type: "",
  provider_type: "openai",
  base_url: "",
  default_model: "",
  notes: "",
  enabled: true,
};

// ── buildProviderBody unit tests ──────────────────────────────────────────────

describe("buildProviderBody", () => {
  it("maps base_url '' to undefined", () => {
    const body = buildProviderBody({ ...BASE_FORM, base_url: "" }, "", "text");
    expect(body.base_url).toBeUndefined();
  });

  it("preserves non-empty base_url", () => {
    const body = buildProviderBody(
      { ...BASE_FORM, base_url: "https://api.example.com" },
      "",
      "text",
    );
    expect(body.base_url).toBe("https://api.example.com");
  });

  it("maps default_model '' to undefined", () => {
    const body = buildProviderBody({ ...BASE_FORM, default_model: "" }, "", "text");
    expect(body.default_model).toBeUndefined();
  });

  it("preserves non-empty default_model", () => {
    const body = buildProviderBody(
      { ...BASE_FORM, default_model: "gpt-4o" },
      "",
      "text",
    );
    expect(body.default_model).toBe("gpt-4o");
  });

  it("maps notes '' to undefined", () => {
    const body = buildProviderBody({ ...BASE_FORM, notes: "" }, "", "text");
    expect(body.notes).toBeUndefined();
  });

  it("does NOT include api_key when apiKeyValue is empty string", () => {
    const body = buildProviderBody(BASE_FORM, "", "text");
    expect(Object.prototype.hasOwnProperty.call(body, "api_key")).toBe(false);
  });

  it("trims whitespace-only apiKeyValue and does NOT include api_key", () => {
    const body = buildProviderBody(BASE_FORM, "   ", "text");
    expect(Object.prototype.hasOwnProperty.call(body, "api_key")).toBe(false);
  });

  it("trims whitespace from apiKeyValue and sets api_key", () => {
    const body = buildProviderBody(BASE_FORM, "  sk-xyz  ", "text");
    expect(body.api_key).toBe("sk-xyz");
  });

  it("sets api_key from apiKeyValue without whitespace", () => {
    const body = buildProviderBody(BASE_FORM, "sk-abc", "text");
    expect(body.api_key).toBe("sk-abc");
  });

  it("sets body.type from category parameter", () => {
    const body = buildProviderBody(BASE_FORM, "", "text");
    expect(body.type).toBe("text");
  });

  it("preserves form.name as-is", () => {
    const body = buildProviderBody({ ...BASE_FORM, name: "my-provider" }, "", "text");
    expect(body.name).toBe("my-provider");
  });
});
