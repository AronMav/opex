import { describe, it, expect } from "vitest";
import { findWikiLinks } from "@/components/workspace/md-decorations/wikilinks";

describe("findWikiLinks", () => {
  it("parses target and section", () => {
    const m = findWikiLinks("see [[My Note#Intro]] now");
    expect(m).toHaveLength(1);
    expect(m[0].target).toBe("My Note");
    expect(m[0].label).toBe("My Note#Intro");
  });
  it("parses alias syntax [[Target|Alias]]", () => {
    const m = findWikiLinks("see [[My Note|Display Name]] here");
    expect(m).toHaveLength(1);
    expect(m[0].target).toBe("My Note");
    expect(m[0].label).toBe("Display Name");
  });
  it("parses alias with section [[Target#Section|Alias]]", () => {
    const m = findWikiLinks("[[Topic#Heading|Custom Label]]");
    expect(m).toHaveLength(1);
    expect(m[0].target).toBe("Topic");
    expect(m[0].label).toBe("Custom Label");
  });
  it("parses a plain link (no section, no alias)", () => {
    const m = findWikiLinks("see [[Note]] now");
    expect(m).toHaveLength(1);
    expect(m[0].target).toBe("Note");
    expect(m[0].label).toBe("Note");
  });
  it("skips empty/whitespace-only links", () => {
    const m = findWikiLinks("[[ ]]");
    expect(m).toHaveLength(0);
  });
});
