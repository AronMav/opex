import { describe, it, expect, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";

// H2: the lightbox is a custom (non-Radix) modal, so it must trap focus on open
// and restore focus to the trigger when dismissed.

vi.mock("@/hooks/use-translation", () => ({
  useTranslation: () => ({ t: (k: string) => k, locale: "en" }),
}));

import { ImageLightbox } from "../ImageLightbox";

describe("ImageLightbox focus management (H2)", () => {
  it("moves focus into the dialog on open and restores it to the trigger on Escape", async () => {
    render(<ImageLightbox src="/pic.png" alt="pic" />);
    const trigger = screen.getByRole("button", { name: "chat.lightbox_open" });
    fireEvent.click(trigger);

    const dialog = screen.getByRole("dialog");
    await waitFor(() => expect(document.activeElement).toBe(dialog));

    fireEvent.keyDown(dialog, { key: "Escape" });
    await waitFor(() => {
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
      expect(document.activeElement).toBe(trigger);
    });
  });

  it("Tab from the dialog container pulls focus to the first control instead of escaping", async () => {
    render(<ImageLightbox src="/pic.png" alt="pic" />);
    fireEvent.click(screen.getByRole("button", { name: "chat.lightbox_open" }));
    const dialog = screen.getByRole("dialog");
    await waitFor(() => expect(document.activeElement).toBe(dialog));

    // Focus sits on the dialog container (tabIndex=-1); forward Tab must land on
    // the first toolbar control, not fall through to the page behind the modal.
    fireEvent.keyDown(dialog, { key: "Tab" });
    expect(document.activeElement).toBe(
      screen.getByRole("button", { name: "chat.lightbox_zoom_out" }),
    );
  });
});
