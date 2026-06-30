// ui/src/__tests__/file-action-types.test.ts
import { describe, it, expect } from "vitest";
import type { FileActionButton, FileActionsResponse } from "@/types/api";

describe("file-handler action types", () => {
  it("FileActionButton has id/label/icon/params", () => {
    const btn: FileActionButton = {
      id: "transcribe",
      label: "Транскрибировать",
      icon: "mic",
      params: { language: "ru" },
    };
    expect(btn.id).toBe("transcribe");
    expect(btn.label).toBe("Транскрибировать");
    expect(btn.icon).toBe("mic");
    expect(btn.params).toEqual({ language: "ru" });
  });

  it("FileActionsResponse wraps a buttons array", () => {
    const resp: FileActionsResponse = {
      buttons: [{ id: "describe", label: "Describe", icon: "image", params: {} }],
    };
    expect(resp.buttons).toHaveLength(1);
    expect(resp.buttons[0].id).toBe("describe");
  });
});
