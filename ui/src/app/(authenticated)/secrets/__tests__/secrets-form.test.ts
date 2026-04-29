import { describe, it, expect } from "vitest";
import {
  buildAddSecretBody,
  buildEditSecretBody,
  buildRevealUrl,
} from "../page";

describe("buildAddSecretBody", () => {
  it("should return null when name is empty string", () => {
    const result = buildAddSecretBody("", "value", "desc", "scope");
    expect(result).toBeNull();
  });

  it("should return null when value is empty string", () => {
    const result = buildAddSecretBody("name", "", "desc", "scope");
    expect(result).toBeNull();
  });

  it("should return null when name is only whitespace", () => {
    const result = buildAddSecretBody("  ", "value", "desc", "scope");
    expect(result).toBeNull();
  });

  it("should return null when value is only whitespace", () => {
    const result = buildAddSecretBody("name", "  ", "desc", "scope");
    expect(result).toBeNull();
  });

  it("should trim name and value", () => {
    const result = buildAddSecretBody("  MY_KEY  ", "  my-val  ", "desc", "");
    expect(result).toEqual({
      name: "MY_KEY",
      value: "my-val",
      description: "desc",
    });
  });

  it("should include description when provided", () => {
    const result = buildAddSecretBody("MY_KEY", "my-val", "A desc", "");
    expect(result).toEqual({
      name: "MY_KEY",
      value: "my-val",
      description: "A desc",
    });
  });

  it("should exclude description when empty string", () => {
    const result = buildAddSecretBody("MY_KEY", "my-val", "", "");
    expect(result).toEqual({
      name: "MY_KEY",
      value: "my-val",
    });
  });

  it("should exclude description when only whitespace", () => {
    const result = buildAddSecretBody("MY_KEY", "my-val", "  ", "");
    expect(result).toEqual({
      name: "MY_KEY",
      value: "my-val",
    });
  });

  it("should exclude scope when empty string", () => {
    const result = buildAddSecretBody("MY_KEY", "my-val", "", "");
    expect(result).toHaveProperty("name", "MY_KEY");
    expect(result).not.toHaveProperty("scope");
  });

  it("should exclude scope when __global__ sentinel", () => {
    const result = buildAddSecretBody("MY_KEY", "my-val", "", "__global__");
    expect(result).toHaveProperty("name", "MY_KEY");
    expect(result).not.toHaveProperty("scope");
  });

  it("should include scope when provided and not __global__", () => {
    const result = buildAddSecretBody("MY_KEY", "my-val", "", "agentName");
    expect(result).toEqual({
      name: "MY_KEY",
      value: "my-val",
      scope: "agentName",
    });
  });

  it("should trim description whitespace", () => {
    const result = buildAddSecretBody("MY_KEY", "my-val", "  desc text  ", "");
    expect(result).toEqual({
      name: "MY_KEY",
      value: "my-val",
      description: "desc text",
    });
  });

  it("should handle all fields together", () => {
    const result = buildAddSecretBody("  API_KEY  ", "  secret123  ", "  My API key  ", "ProductionAgent");
    expect(result).toEqual({
      name: "API_KEY",
      value: "secret123",
      description: "My API key",
      scope: "ProductionAgent",
    });
  });

  it("should not include undefined fields in returned object", () => {
    const result = buildAddSecretBody("KEY", "value", "", "");
    expect(Object.keys(result!).sort()).toEqual(["name", "value"]);
  });
});

describe("buildEditSecretBody", () => {
  it("should return null when value is empty string", () => {
    const result = buildEditSecretBody("KEY", "", "desc", "scope");
    expect(result).toBeNull();
  });

  it("should return null when value is only whitespace", () => {
    const result = buildEditSecretBody("KEY", "  ", "desc", "scope");
    expect(result).toBeNull();
  });

  it("should always include name and value", () => {
    const result = buildEditSecretBody("MY_KEY", "new-val", "", "");
    expect(result).toEqual({
      name: "MY_KEY",
      value: "new-val",
    });
  });

  it("should trim value", () => {
    const result = buildEditSecretBody("KEY", "  new-val  ", "", "");
    expect(result?.value).toBe("new-val");
  });

  it("should include description when provided", () => {
    const result = buildEditSecretBody("KEY", "new-val", "desc text", "");
    expect(result).toEqual({
      name: "KEY",
      value: "new-val",
      description: "desc text",
    });
  });

  it("should exclude description when empty string", () => {
    const result = buildEditSecretBody("KEY", "new-val", "", "");
    expect(result).not.toHaveProperty("description");
  });

  it("should exclude description when only whitespace", () => {
    const result = buildEditSecretBody("KEY", "new-val", "  ", "");
    expect(result).not.toHaveProperty("description");
  });

  it("should trim description whitespace", () => {
    const result = buildEditSecretBody("KEY", "new-val", "  desc text  ", "");
    expect(result?.description).toBe("desc text");
  });

  it("should include scope when provided", () => {
    const result = buildEditSecretBody("KEY", "new-val", "", "myAgent");
    expect(result).toEqual({
      name: "KEY",
      value: "new-val",
      scope: "myAgent",
    });
  });

  it("should exclude scope when empty string", () => {
    const result = buildEditSecretBody("KEY", "new-val", "", "");
    expect(result).not.toHaveProperty("scope");
  });

  it("should handle all fields together", () => {
    const result = buildEditSecretBody("UPDATED_KEY", "  updated-val  ", "  new desc  ", "DevAgent");
    expect(result).toEqual({
      name: "UPDATED_KEY",
      value: "updated-val",
      description: "new desc",
      scope: "DevAgent",
    });
  });

  it("should preserve editTarget name exactly", () => {
    const result = buildEditSecretBody("CASE_SENSITIVE_KEY", "val", "", "");
    expect(result?.name).toBe("CASE_SENSITIVE_KEY");
  });

  it("should not include undefined fields in returned object", () => {
    const result = buildEditSecretBody("KEY", "value", "", "");
    expect(Object.keys(result!).sort()).toEqual(["name", "value"]);
  });
});

describe("buildRevealUrl", () => {
  it("should return URL with reveal=true when scope is empty", () => {
    const result = buildRevealUrl("SIMPLE_KEY", "");
    expect(result).toBe("/api/secrets/SIMPLE_KEY?reveal=true");
  });

  it("should URL-encode name with spaces", () => {
    const result = buildRevealUrl("KEY WITH SPACES", "");
    expect(result).toBe("/api/secrets/KEY%20WITH%20SPACES?reveal=true");
  });

  it("should URL-encode special characters in name", () => {
    const result = buildRevealUrl("MY&KEY=VALUE", "");
    expect(result).toBe("/api/secrets/MY%26KEY%3DVALUE?reveal=true");
  });

  it("should append scope query parameter when provided", () => {
    const result = buildRevealUrl("MY_KEY", "agentName");
    expect(result).toBe("/api/secrets/MY_KEY?reveal=true&scope=agentName");
  });

  it("should URL-encode scope value", () => {
    const result = buildRevealUrl("MY_KEY", "scope with spaces");
    expect(result).toBe("/api/secrets/MY_KEY?reveal=true&scope=scope%20with%20spaces");
  });

  it("should URL-encode special characters in scope", () => {
    const result = buildRevealUrl("KEY", "agent&name=test");
    expect(result).toBe("/api/secrets/KEY?reveal=true&scope=agent%26name%3Dtest");
  });

  it("should handle both name and scope with special characters", () => {
    const result = buildRevealUrl("KEY & NAME", "SCOPE=VALUE");
    expect(result).toBe("/api/secrets/KEY%20%26%20NAME?reveal=true&scope=SCOPE%3DVALUE");
  });

  it("should not append scope parameter when scope is empty string", () => {
    const result = buildRevealUrl("KEY", "");
    expect(result).not.toContain("&scope");
  });

  it("should construct URL in correct order: path, reveal, scope", () => {
    const result = buildRevealUrl("MY_KEY", "myAgent");
    const parts = result.split("?");
    expect(parts[0]).toBe("/api/secrets/MY_KEY");
    expect(parts[1]).toContain("reveal=true");
    expect(parts[1]).toContain("&scope=myAgent");
  });

  it("should handle URL-unsafe characters in both fields", () => {
    const result = buildRevealUrl("KEY#HASH", "scope@email");
    expect(result).toContain("%23");
    expect(result).toContain("%40");
  });

  it("passes __global__ scope through as-is (buildRevealUrl does not filter it)", () => {
    const url = buildRevealUrl("KEY", "__global__");
    expect(url).toBe("/api/secrets/KEY?reveal=true&scope=__global__");
  });
});
