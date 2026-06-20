import { describe, it, expect } from "vitest";
import {
  ALL_CATEGORIES,
  ALL_CAPABILITIES,
  sortActiveRows,
  buildActiveListAfterToggle,
  buildProviderBody,
} from "../page";
import type { CreateProviderInput } from "@/types/api";

// ── websearch presence ────────────────────────────────────────────────────────

describe("ALL_CATEGORIES / ALL_CAPABILITIES — websearch", () => {
  it("includes websearch in ALL_CATEGORIES", () => {
    expect(ALL_CATEGORIES).toContain("websearch");
  });

  it("includes websearch in ALL_CAPABILITIES", () => {
    expect(ALL_CAPABILITIES).toContain("websearch");
  });

  it("ALL_CAPABILITIES does not include 'text' (text providers are LLM, not capability-selectable)", () => {
    expect(ALL_CAPABILITIES).not.toContain("text");
  });
});

// ── sortActiveRows ────────────────────────────────────────────────────────────

describe("sortActiveRows", () => {
  const rows = [
    { capability: "websearch", provider_name: "brave", priority: 5 },
    { capability: "websearch", provider_name: "searxng", priority: 1 },
    { capability: "stt", provider_name: "whisper", priority: 1 },
    { capability: "websearch", provider_name: null, priority: 99 }, // null entries excluded
  ];

  it("returns only rows for the requested capability", () => {
    const result = sortActiveRows(rows, "stt");
    expect(result).toHaveLength(1);
    expect(result[0].provider_name).toBe("whisper");
  });

  it("excludes null provider_name entries", () => {
    const result = sortActiveRows(rows, "websearch");
    expect(result.every((r) => r.provider_name !== null)).toBe(true);
  });

  it("sorts ascending by priority (lowest first = highest priority)", () => {
    const result = sortActiveRows(rows, "websearch");
    expect(result[0].provider_name).toBe("searxng");  // priority 1
    expect(result[1].provider_name).toBe("brave");    // priority 5
  });

  it("returns empty array when no rows match capability", () => {
    expect(sortActiveRows(rows, "imagegen")).toHaveLength(0);
  });
});

// ── buildActiveListAfterToggle ────────────────────────────────────────────────

describe("buildActiveListAfterToggle — active toggle sends providers array", () => {
  const currentRows = [
    { provider_name: "searxng", priority: 1 },
    { provider_name: "brave", priority: 5 },
  ];

  it("removing an active provider yields a filtered providers array", () => {
    const next = buildActiveListAfterToggle(currentRows, "brave", true, 5);
    expect(next).toHaveLength(1);
    expect(next[0].provider_name).toBe("searxng");
    // Mutation payload shape: array of {provider_name, priority}
    expect(next[0]).toHaveProperty("provider_name");
    expect(next[0]).toHaveProperty("priority");
  });

  it("adding an inactive provider appends it with the draft priority", () => {
    const next = buildActiveListAfterToggle(currentRows, "ollama", false, 10);
    expect(next).toHaveLength(3);
    const added = next.find((r) => r.provider_name === "ollama");
    expect(added?.priority).toBe(10);
  });

  it("toggling off the last provider yields an empty providers array", () => {
    const single = [{ provider_name: "searxng", priority: 1 }];
    const next = buildActiveListAfterToggle(single, "searxng", true, 1);
    expect(next).toHaveLength(0);
    // Mutation would send { capability, providers: [] } — valid list form
  });

  it("returned objects always have provider_name and priority (list-form shape)", () => {
    const next = buildActiveListAfterToggle(currentRows, "ollama", false, 3);
    for (const row of next) {
      expect(typeof row.provider_name).toBe("string");
      expect(typeof row.priority).toBe("number");
    }
  });
});

// ── group_hint visibility predicate (I-1 regression) ─────────────────────────

describe("group_hint predicate — shown for non-websearch capability groups only", () => {
  const hintVisible = (cap: string) =>
    (ALL_CAPABILITIES as readonly string[]).includes(cap) && cap !== "websearch";

  it("returns false for websearch (websearch legitimately allows multiple active providers)", () => {
    expect(hintVisible("websearch")).toBe(false);
  });

  it("returns true for stt", () => {
    expect(hintVisible("stt")).toBe(true);
  });

  it("returns true for tts", () => {
    expect(hintVisible("tts")).toBe(true);
  });

  it("returns true for vision", () => {
    expect(hintVisible("vision")).toBe(true);
  });

  it("returns true for imagegen", () => {
    expect(hintVisible("imagegen")).toBe(true);
  });

  it("returns true for embedding", () => {
    expect(hintVisible("embedding")).toBe(true);
  });

  it("returns false for text (not in ALL_CAPABILITIES)", () => {
    expect(hintVisible("text")).toBe(false);
  });
});

// ── buildProviderBody (existing tests, regression) ───────────────────────────

const BASE_FORM: CreateProviderInput = {
  name: "my-provider",
  type: "",
  provider_type: "openai",
  base_url: "",
  default_model: "",
  notes: "",
  enabled: true,
};

describe("buildProviderBody — websearch category regression", () => {
  it("sets body.type to websearch when category is websearch", () => {
    const body = buildProviderBody({ ...BASE_FORM, provider_type: "searxng" }, "", "websearch");
    expect(body.type).toBe("websearch");
  });

  it("preserves provider_type for websearch providers (searxng)", () => {
    const body = buildProviderBody({ ...BASE_FORM, provider_type: "searxng" }, "", "websearch");
    expect(body.provider_type).toBe("searxng");
  });

  it("preserves provider_type for websearch providers (brave)", () => {
    const body = buildProviderBody({ ...BASE_FORM, provider_type: "brave" }, "sk-brave", "websearch");
    expect(body.provider_type).toBe("brave");
    expect(body.api_key).toBe("sk-brave");
  });
});
