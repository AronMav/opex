import { describe, it, expect } from "vitest";
import { buildAuditParams } from "../audit-params";

describe("buildAuditParams", () => {
  const base = { pageSize: 50, offset: 0, agent: "_all", eventType: "_all", search: "" };

  it("always carries limit and offset", () => {
    expect(buildAuditParams({ ...base, offset: 100 })).toEqual({ limit: "50", offset: "100" });
  });

  it("omits the _all sentinel filters", () => {
    expect(buildAuditParams(base)).toEqual({ limit: "50", offset: "0" });
  });

  it("includes agent and event_type when selected", () => {
    expect(buildAuditParams({ ...base, agent: "Manager", eventType: "secret_created" })).toEqual({
      limit: "50",
      offset: "0",
      agent: "Manager",
      event_type: "secret_created",
    });
  });

  it("passes a trimmed search term to the server", () => {
    expect(buildAuditParams({ ...base, search: "  openai  " })).toEqual({
      limit: "50",
      offset: "0",
      search: "openai",
    });
  });

  it("drops a whitespace-only search", () => {
    expect(buildAuditParams({ ...base, search: "   " })).toEqual({ limit: "50", offset: "0" });
  });
});
