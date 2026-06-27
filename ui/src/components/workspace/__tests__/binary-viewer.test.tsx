import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
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

  it("shows error message and download link when image fails to load", () => {
    const { container } = render(<BinaryViewer file={{ is_binary: true, mime: "image/png", size: 1, url: "/workspace-files/x.png?sig=expired", path: "x.png", is_dir: false }} />);
    const img = container.querySelector("img");
    expect(img).not.toBeNull();
    fireEvent.error(img!);
    // img should be gone, error message and download link should appear
    expect(container.querySelector("img")).toBeNull();
    const errorText = container.textContent;
    expect(errorText).toContain("workspace.image_load_error");
    const a = container.querySelector("a");
    expect(a?.getAttribute("href")).toBe("/workspace-files/x.png?sig=expired");
    expect(a?.hasAttribute("download")).toBe(true);
  });

  it("renders PDF download fallback link below iframe", () => {
    const { container } = render(<BinaryViewer file={{ is_binary: true, mime: "application/pdf", size: 1, url: "/workspace-files/d.pdf?sig=a", path: "d.pdf", is_dir: false }} />);
    // iframe still present
    expect(container.querySelector("iframe")?.getAttribute("src")).toBe("/workspace-files/d.pdf?sig=a");
    // download fallback link also present
    const a = container.querySelector("a");
    expect(a?.getAttribute("href")).toBe("/workspace-files/d.pdf?sig=a");
    expect(a?.hasAttribute("download")).toBe(true);
  });
});
