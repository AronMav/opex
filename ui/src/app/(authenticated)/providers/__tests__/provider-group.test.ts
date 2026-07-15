import { describe, it, expect } from "vitest";
import {
  ALL_CATEGORIES,
  ALL_CAPABILITIES,
  sortActiveRows,
  renumberPriorities,
  splitProviders,
  buildProviderBody,
} from "../page";
import type { CreateProviderInput, Provider } from "@/types/api";

// ── websearch presence ────────────────────────────────────────────────────────

describe("ALL_CATEGORIES / ALL_CAPABILITIES — profiles own capability routing", () => {
  it("includes websearch in ALL_CATEGORIES (still a provider-record category)", () => {
    expect(ALL_CATEGORIES).toContain("websearch");
  });

  it("ALL_CAPABILITIES contains only 'embedding' — profiles own routing for the rest", () => {
    expect(ALL_CAPABILITIES).toEqual(["embedding"]);
  });

  it("ALL_CAPABILITIES does not include 'text' (text providers are LLM, not capability-selectable)", () => {
    expect(ALL_CAPABILITIES).not.toContain("text");
  });

  it("ALL_CAPABILITIES does not include stt/tts/vision/imagegen/websearch (moved to profile-owned routing)", () => {
    for (const cap of ["stt", "tts", "vision", "imagegen", "websearch"]) {
      expect(ALL_CAPABILITIES).not.toContain(cap);
    }
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

// ── renumberPriorities ────────────────────────────────────────────────────────

describe("renumberPriorities", () => {
  it("returns empty array for empty input", () => {
    expect(renumberPriorities([])).toEqual([]);
  });

  it("assigns 1-based priority following array order", () => {
    expect(renumberPriorities(["a", "b", "c"])).toEqual([
      { provider_name: "a", priority: 1 },
      { provider_name: "b", priority: 2 },
      { provider_name: "c", priority: 3 },
    ]);
  });
});

// ── splitProviders ────────────────────────────────────────────────────────────

describe("splitProviders", () => {
  const mk = (name: string): Provider =>
    ({ id: name, name, type: "stt", provider_type: "whisper", enabled: true } as Provider);
  const capProviders = [mk("zeta"), mk("alpha"), mk("mid")];

  it("returns active in activeRows order, inactive alphabetically", () => {
    const activeRows = [
      { provider_name: "mid", priority: 1 },
      { provider_name: "zeta", priority: 2 },
    ];
    const { active, inactive } = splitProviders(capProviders, activeRows);
    expect(active.map((p) => p.name)).toEqual(["mid", "zeta"]);
    expect(inactive.map((p) => p.name)).toEqual(["alpha"]);
  });

  it("all providers inactive (alphabetical) when activeRows empty", () => {
    const { active, inactive } = splitProviders(capProviders, []);
    expect(active).toHaveLength(0);
    expect(inactive.map((p) => p.name)).toEqual(["alpha", "mid", "zeta"]);
  });

  it("skips active rows whose provider is missing from capProviders", () => {
    const activeRows = [{ provider_name: "ghost", priority: 1 }];
    const { active, inactive } = splitProviders(capProviders, activeRows);
    expect(active).toHaveLength(0);
    expect(inactive.map((p) => p.name)).toEqual(["alpha", "mid", "zeta"]);
  });
});

// ── group_hint visibility predicate (I-1 regression) ─────────────────────────

describe("group_hint predicate — only embedding renders the active-provider group now", () => {
  const hintVisible = (cap: string) =>
    (ALL_CAPABILITIES as readonly string[]).includes(cap);

  it("returns false for websearch (moved to profile-owned routing)", () => {
    expect(hintVisible("websearch")).toBe(false);
  });

  it("returns false for stt (moved to profile-owned routing)", () => {
    expect(hintVisible("stt")).toBe(false);
  });

  it("returns false for tts (moved to profile-owned routing)", () => {
    expect(hintVisible("tts")).toBe(false);
  });

  it("returns false for vision (moved to profile-owned routing)", () => {
    expect(hintVisible("vision")).toBe(false);
  });

  it("returns false for imagegen (moved to profile-owned routing)", () => {
    expect(hintVisible("imagegen")).toBe(false);
  });

  it("returns true for embedding (the sole remaining global active-provider capability)", () => {
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
