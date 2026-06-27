import { describe, it, expect, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import { BinaryViewer } from "@/components/workspace/binary-viewer";

vi.mock("@/hooks/use-translation", () => ({
  useTranslation: () => ({ t: (key: string) => key, locale: "en" }),
}));

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

  it("renders a download link for other binary types", () => {
    const { container } = render(<BinaryViewer file={{ is_binary: true, mime: "application/zip", size: 2048, url: "/workspace-files/a.zip?sig=a", path: "a.zip", is_dir: false }} />);
    const a = container.querySelector("a");
    expect(a?.getAttribute("href")).toBe("/workspace-files/a.zip?sig=a");
    expect(a?.hasAttribute("download")).toBe(true);
  });
});
