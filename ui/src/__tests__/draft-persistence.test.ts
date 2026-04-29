import { vi, describe, it, expect, beforeEach, afterEach } from "vitest";

import { saveDraft, loadDraft, clearDraft } from "@/app/(authenticated)/chat/composer/ChatComposer";

describe("Draft persistence helpers", () => {
  beforeEach(() => {
    localStorage.clear();
  });

  it("saveDraft writes to localStorage key hydeclaw.draft.{agent}", () => {
    saveDraft("Aria", "Hello world");
    expect(localStorage.getItem("hydeclaw.draft.Aria")).toBe("Hello world");
  });

  it("loadDraft returns stored text", () => {
    localStorage.setItem("hydeclaw.draft.Aria", "Stored text");
    expect(loadDraft("Aria")).toBe("Stored text");
  });

  it("loadDraft returns empty string when no draft stored", () => {
    expect(loadDraft("NonExistentAgent")).toBe("");
  });

  it("saveDraft with empty string removes the key", () => {
    localStorage.setItem("hydeclaw.draft.Aria", "Some text");
    saveDraft("Aria", "");
    expect(localStorage.getItem("hydeclaw.draft.Aria")).toBeNull();
  });

  it("clearDraft removes the key", () => {
    localStorage.setItem("hydeclaw.draft.Aria", "Some text");
    clearDraft("Aria");
    expect(localStorage.getItem("hydeclaw.draft.Aria")).toBeNull();
  });

  it("draft keys are per-agent (no cross-agent contamination)", () => {
    saveDraft("Aria", "Aria's draft");
    saveDraft("Bob", "Bob's draft");
    expect(loadDraft("Aria")).toBe("Aria's draft");
    expect(loadDraft("Bob")).toBe("Bob's draft");
    clearDraft("Aria");
    expect(loadDraft("Aria")).toBe("");
    expect(loadDraft("Bob")).toBe("Bob's draft");
  });

  it("handles agent names with special characters", () => {
    saveDraft("Agent/With:Special", "draft text");
    expect(loadDraft("Agent/With:Special")).toBe("draft text");
  });

  it("saveDraft propagates localStorage errors (no silent swallow)", () => {
    const setItemSpy = vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => {
      throw new DOMException("QuotaExceededError");
    });
    // saveDraft does not swallow errors — callers must guard if needed
    expect(() => saveDraft("Aria", "some text")).toThrow();
    setItemSpy.mockRestore();
  });

  it("loadDraft propagates localStorage errors (no silent swallow)", () => {
    const getItemSpy = vi.spyOn(Storage.prototype, "getItem").mockImplementation(() => {
      throw new DOMException("SecurityError");
    });
    expect(() => loadDraft("Aria")).toThrow();
    getItemSpy.mockRestore();
  });
});
