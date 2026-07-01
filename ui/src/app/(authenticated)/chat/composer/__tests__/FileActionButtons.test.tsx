import React from "react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";

vi.mock("sonner", () => ({
  toast: { success: vi.fn(), error: vi.fn(), info: vi.fn(), warning: vi.fn() },
}));

vi.mock("@/stores/language-store", () => ({
  useLanguageStore: (selector?: (s: { locale: string }) => unknown) => {
    const state = { locale: "ru" };
    return selector ? selector(state) : state;
  },
}));

const apiGet = vi.fn();
const apiPost = vi.fn();
vi.mock("@/lib/api", () => ({
  apiGet: (...a: unknown[]) => apiGet(...a),
  apiPost: (...a: unknown[]) => apiPost(...a),
}));

const mockInvalidateQueries = vi.fn();
vi.mock("@/lib/query-client", () => ({
  queryClient: { invalidateQueries: (...a: unknown[]) => mockInvalidateQueries(...a) },
}));

vi.mock("@/lib/queries", () => ({
  qk: {
    sessionMessages: (id: string) => ["sessions", id, "messages"],
  },
}));

import { FileActionButtons } from "../FileActionButtons";

const UPLOAD_ID = "11111111-1111-1111-1111-111111111111";
const PROPS = { uploadId: UPLOAD_ID, mime: "audio/ogg", agent: "main", sessionId: "sess-1" };

describe("FileActionButtons", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    apiGet.mockResolvedValue({
      buttons: [
        { id: "transcribe", label: "Транскрибировать", icon: "mic", params: { language: "ru" } },
        { id: "describe", label: "Описать", icon: "image", params: {} },
      ],
    });
    apiPost.mockResolvedValue({});
  });

  it("fetches actions with agent + session + lang query params on mount", async () => {
    render(<FileActionButtons {...PROPS} />);
    await waitFor(() =>
      expect(apiGet).toHaveBeenCalledWith(
        `/api/files/${UPLOAD_ID}/actions?agent=main&session=sess-1&lang=ru`,
      ),
    );
  });

  it("renders a button per returned action with its localized label", async () => {
    render(<FileActionButtons {...PROPS} />);
    expect(await screen.findByRole("button", { name: "Транскрибировать" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Описать" })).toBeInTheDocument();
  });

  it("renders nothing when no buttons are returned", async () => {
    apiGet.mockResolvedValue({ buttons: [] });
    const { container } = render(<FileActionButtons {...PROPS} />);
    await waitFor(() => expect(apiGet).toHaveBeenCalled());
    expect(container.querySelectorAll("button")).toHaveLength(0);
  });

  it("click POSTs run with handler_id, empty params, session_id, agent, lang", async () => {
    render(<FileActionButtons {...PROPS} />);
    const btn = await screen.findByRole("button", { name: "Транскрибировать" });
    fireEvent.click(btn);
    await waitFor(() =>
      expect(apiPost).toHaveBeenCalledWith(`/api/files/${UPLOAD_ID}/run`, {
        handler_id: "transcribe",
        params: {},
        session_id: "sess-1",
        agent: "main",
        lang: "ru",
      }),
    );
  });

  it("invalidates session messages on sync result (no accepted:true)", async () => {
    // Sync response: has status field, no accepted:true
    apiPost.mockResolvedValue({ status: "ok" });
    render(<FileActionButtons {...PROPS} />);
    const btn = await screen.findByRole("button", { name: "Транскрибировать" });
    fireEvent.click(btn);
    await waitFor(() => expect(mockInvalidateQueries).toHaveBeenCalledWith({
      queryKey: ["sessions", "sess-1", "messages"],
    }));
  });

  it("does NOT invalidate session messages on async ack (accepted:true + job_id)", async () => {
    // Async response: accepted:true + job_id — rely on WS terminal event for refresh
    apiPost.mockResolvedValue({ accepted: true, job_id: "abc-123" });
    render(<FileActionButtons {...PROPS} />);
    const btn = await screen.findByRole("button", { name: "Транскрибировать" });
    fireEvent.click(btn);
    await waitFor(() => expect(apiPost).toHaveBeenCalled());
    // Give a tick for the async path
    await new Promise((r) => setTimeout(r, 10));
    expect(mockInvalidateQueries).not.toHaveBeenCalled();
  });

  it("shows a spinner on the clicked button while running", async () => {
    let resolveRun: (v: unknown) => void = () => {};
    apiPost.mockImplementation(() => new Promise((r) => { resolveRun = r; }));
    render(<FileActionButtons {...PROPS} />);
    const btn = await screen.findByRole("button", { name: "Транскрибировать" });
    fireEvent.click(btn);
    await waitFor(() => expect(btn).toBeDisabled());
    expect(btn.querySelector(".animate-spin")).not.toBeNull();
    resolveRun({});
    await waitFor(() => expect(btn).not.toBeDisabled());
  });

  it("rapid double-click fires only a single POST", async () => {
    let resolveRun: (v: unknown) => void = () => {};
    apiPost.mockImplementation(() => new Promise((r) => { resolveRun = r; }));
    render(<FileActionButtons {...PROPS} />);
    const btn = await screen.findByRole("button", { name: "Транскрибировать" });
    // Two synchronous clicks before the first async tick resolves
    fireEvent.click(btn);
    fireEvent.click(btn);
    // Let the in-flight run settle
    resolveRun({});
    await waitFor(() => expect(btn).not.toBeDisabled());
    expect(apiPost).toHaveBeenCalledTimes(1);
  });
});
