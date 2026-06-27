import { describe, it, expect } from "vitest";
import { parseCalloutHeader } from "@/components/workspace/md-decorations/callouts";

describe("parseCalloutHeader", () => {
  it("parses type, collapsible and title", () => {
    expect(parseCalloutHeader("> [!note]- Полный транскрипт")).toEqual({
      type: "note", collapsible: true, title: "Полный транскрипт",
    });
  });
  it("non-callout returns null", () => {
    expect(parseCalloutHeader("> just a quote")).toBeNull();
  });
});
