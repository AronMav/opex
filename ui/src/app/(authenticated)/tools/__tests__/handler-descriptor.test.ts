import { describe, it, expect } from "vitest";
import { renderDescriptorBlock, spliceDescriptor } from "../handler-descriptor";

const FIELDS = {
  id: "my_ocr", labels: { en: "OCR", ru: "ОЦР" }, descriptions: {},
  icon: "file", mime: ["image/*"], domains: [], max_size_mb: 20, execution: "sync" as const,
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

  it("round-trips capability, output, and params without data loss", () => {
    const fields = {
      ...FIELDS,
      capability: "vision",
      output: "file",
      params: [
        { name: "max_chars", type: "int", default: "8000", required: false },
        { name: "lang", type: "string", default: null, required: true },
      ],
    };
    const block = renderDescriptorBlock(fields);
    expect(block).toContain("#   <capability>vision</capability>");
    expect(block).toContain("#   <output>file</output>");
    expect(block).toContain("#   <params>");
    expect(block).toContain('#     <param name="max_chars" type="int" default="8000" required="false"/>');
    expect(block).toContain('#     <param name="lang" type="string" required="true"/>');
    expect(block).toContain("#   </params>");
  });

  it("spliceDescriptor replaces a block that uses #<handler> with no space after #", () => {
    // descriptor.py uses `#\s*<handler>` — no-space variant must also be replaced,
    // not duplicated.
    const src = "#<handler>\n#   <id>old</id>\n#</handler>\nasync def run(): pass\n";
    const out = spliceDescriptor(src, FIELDS);
    expect(out).toContain("<id>my_ocr</id>");
    expect(out).not.toContain("<id>old</id>");
    // Only one handler block in the result.
    expect((out.match(/# <handler>/g) ?? []).length).toBe(1);
  });

  it("omits capability block when capability is null/undefined", () => {
    const block = renderDescriptorBlock({ ...FIELDS, capability: null });
    expect(block).not.toContain("<capability>");
  });

  it("omits params block when params array is empty", () => {
    const block = renderDescriptorBlock({ ...FIELDS, params: [] });
    expect(block).not.toContain("<params>");
  });

  it("renders <domain> elements inside <match> when domains are set", () => {
    const block = renderDescriptorBlock({ ...FIELDS, domains: ["youtube.com", "youtu.be"] });
    expect(block).toContain("#     <domain>youtube.com</domain>");
    expect(block).toContain("#     <domain>youtu.be</domain>");
  });

  it("omits <domain> elements when domains array is empty", () => {
    const block = renderDescriptorBlock({ ...FIELDS, domains: [] });
    expect(block).not.toContain("<domain>");
  });
});
