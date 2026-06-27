import { describe, it, expect } from "vitest";
import { render } from "@testing-library/react";
import { ObsidianEditor } from "@/components/workspace/obsidian-editor";

describe("ObsidianEditor", () => {
  // Lossless invariant: CM shows the raw markdown source verbatim (no WYSIWYG
  // transform), so frontmatter/callout markup is never mangled. `@testing-library/
  // user-event` is NOT installed and CM6 typing in jsdom is flaky — assert the
  // mounted DOM contains the raw source instead of simulating keystrokes.
  it("renders raw markdown source verbatim (no WYSIWYG transform)", () => {
    const src = "---\ntitle: x\n---\n\n# H\n\n> [!note]- T\n> line\n";
    const { container } = render(<ObsidianEditor value={src} onChange={() => {}} noteDir="" />);
    const content = container.querySelector(".cm-content");
    expect(content?.textContent).toContain("title: x");   // frontmatter shown as source
    expect(content?.textContent).toContain("[!note]-");   // callout markup verbatim
  });
});
