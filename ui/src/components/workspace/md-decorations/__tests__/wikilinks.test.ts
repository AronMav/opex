import { describe, it, expect } from "vitest";
import { findWikiLinks } from "@/components/workspace/md-decorations/wikilinks";

describe("findWikiLinks", () => {
  it("parses target and section", () => {
    const m = findWikiLinks("see [[My Note#Intro]] now");
    expect(m).toHaveLength(1);
    expect(m[0].target).toBe("My Note");
    expect(m[0].label).toBe("My Note#Intro");
  });
});
