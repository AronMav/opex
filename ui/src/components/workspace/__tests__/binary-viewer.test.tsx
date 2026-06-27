import { describe, it, expect } from "vitest";
import { render, screen } from "@testing-library/react";
import { BinaryViewer } from "@/components/workspace/binary-viewer";

describe("BinaryViewer", () => {
  it("renders <img> for images", () => {
    render(<BinaryViewer file={{ is_binary: true, mime: "image/png", size: 1, url: "/workspace-files/x.png?sig=a", path: "x.png", is_dir: false }} />);
    const img = screen.getByRole("img");
    expect(img.getAttribute("src")).toBe("/workspace-files/x.png?sig=a");
  });

  it("renders iframe for pdf", () => {
    const { container } = render(<BinaryViewer file={{ is_binary: true, mime: "application/pdf", size: 1, url: "/workspace-files/d.pdf?sig=a", path: "d.pdf", is_dir: false }} />);
    expect(container.querySelector("iframe")?.getAttribute("src")).toBe("/workspace-files/d.pdf?sig=a");
  });
});
