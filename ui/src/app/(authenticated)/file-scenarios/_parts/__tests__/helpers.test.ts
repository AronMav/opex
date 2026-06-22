import { describe, it, expect } from "vitest";
import {
  FSE_ALLOWLIST_MEMBERS,
  groupByMatchType,
  sortBindings,
  buildScenarioBody,
  isAllowlistViolation,
} from "../helpers";
import type { FileScenario } from "@/types/api";

const mk = (o: Partial<FileScenario>): FileScenario => ({
  id: "id1", match_type: "image/*", executor: "tool", action_ref: "describe",
  label: "Describe", is_default: false, priority: 100, enabled: true,
  scope: "global", created_by: "system",
  created_at: "2026-06-22T00:00:00Z", updated_at: "2026-06-22T00:00:00Z",
  ...o,
});

describe("file-scenarios helpers", () => {
  it("FSE_ALLOWLIST_MEMBERS is the closed v1 set", () => {
    expect([...FSE_ALLOWLIST_MEMBERS].sort()).toEqual(
      ["describe", "extract_document", "save", "transcribe"],
    );
  });

  it("groupByMatchType groups and sorts groups alphabetically", () => {
    const groups = groupByMatchType([
      mk({ id: "b", match_type: "image/*" }),
      mk({ id: "a", match_type: "audio/*", action_ref: "transcribe" }),
    ]);
    expect(groups.map((g) => g.matchType)).toEqual(["audio/*", "image/*"]);
  });

  it("sortBindings orders by priority then created_at then id", () => {
    const out = sortBindings([
      mk({ id: "z", priority: 100, created_at: "2026-06-22T00:00:02Z" }),
      mk({ id: "a", priority: 50 }),
      mk({ id: "m", priority: 100, created_at: "2026-06-22T00:00:01Z" }),
    ]);
    expect(out.map((b) => b.id)).toEqual(["a", "m", "z"]);
  });

  it("buildScenarioBody fills defaults for priority/enabled", () => {
    const body = buildScenarioBody({
      match_type: ".mp4", executor: "skill", action_ref: "video_summary", label: "Summarize",
    });
    expect(body.priority).toBe(100);
    expect(body.enabled).toBe(true);
    expect(body.is_default).toBe(false);
  });

  it("isAllowlistViolation flags default+tool with non-allowlisted action_ref", () => {
    expect(isAllowlistViolation("tool", true, "code_exec")).toBe(true);
    expect(isAllowlistViolation("tool", true, "describe")).toBe(false);
    expect(isAllowlistViolation("tool", false, "code_exec")).toBe(false);
    expect(isAllowlistViolation("skill", true, "anything")).toBe(false);
  });
});
