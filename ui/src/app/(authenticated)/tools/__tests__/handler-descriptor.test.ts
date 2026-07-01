import { describe, it, expect } from "vitest";
import { renderDescriptorBlock, spliceDescriptor } from "../handler-descriptor";

const FIELDS = {
  id: "my_ocr", labels: { en: "OCR", ru: "ОЦР" }, descriptions: {},
  icon: "file", mime: ["image/*"], max_size_mb: 20, execution: "sync" as const,
  order: 100, enabled: true,
};

describe("descriptor block", () => {
  it("renders a # <handler> comment block with the fields", () => {
    const b = renderDescriptorBlock(FIELDS);
    expect(b).toMatch(/^# <handler>/);
    expect(b).toContain("#   <id>my_ocr</id>");
    expect(b).toContain('#   <label lang="en">OCR</label>');
    expect(b).toContain("#     <mime>image/*</mime>");
    expect(b).toContain("#     <max_size_mb>20</max_size_mb>");
    expect(b).toContain("#   <execution>sync</execution>");
    expect(b).toContain("# </handler>");
  });

  it("splices over an existing block, preserving the code body", () => {
    const src = "# <handler>\n#   <id>old</id>\n# </handler>\nasync def run(): pass\n";
    const out = spliceDescriptor(src, FIELDS);
    expect(out).toContain("<id>my_ocr</id>");
    expect(out).toContain("async def run(): pass");
    expect(out).not.toContain("<id>old</id>");
  });
});
