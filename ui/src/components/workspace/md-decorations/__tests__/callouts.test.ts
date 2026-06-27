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
  it("uppercased type is lowercased", () => {
    expect(parseCalloutHeader("> [!NOTE]")).toEqual({
      type: "note", collapsible: false, title: "",
    });
  });
  it("+ suffix marks collapsible", () => {
    expect(parseCalloutHeader("> [!tip]+ Expand me")).toEqual({
      type: "tip", collapsible: true, title: "Expand me",
    });
  });
});
