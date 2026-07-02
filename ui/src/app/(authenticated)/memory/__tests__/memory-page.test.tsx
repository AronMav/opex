import { test, expect, vi, beforeEach } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen } from "@testing-library/react";

const documents = [
  {
    id: "aaaaaaaa-1111-2222-3333-444444444444",
    source: "auto:session:demo",
    pinned: true,
    relevance_score: 0.9,
    created_at: "2026-01-01T00:00:00Z",
    preview: "hello",
    total_chars: 5,
    scope: "shared",
  },
  {
    id: "bbbbbbbb-5555-6666-7777-888888888888",
    source: "notes.md",
    pinned: false,
    relevance_score: 0.5,
    created_at: "2026-02-02T00:00:00Z",
    preview: "world",
    total_chars: 5,
    scope: "agent",
  },
];

// 60 total documents → 3 pages at limit 20 → Pagination shows "1 / 3".
vi.mock("@/lib/api", () => ({
  apiGet: vi.fn(() => Promise.resolve({ documents, total: 60 })),
  apiPatch: vi.fn(() => Promise.resolve({})),
  apiDelete: vi.fn(() => Promise.resolve({})),
}));
vi.mock("@/lib/queries", () => ({
  useMemoryStats: () => ({ data: { total: 60, pinned: 1, embed_dim: 1024 } }),
  qk: { memoryStats: ["memory", "stats"] },
}));
vi.mock("@tanstack/react-query", () => ({
  useQueryClient: () => ({ invalidateQueries: vi.fn() }),
}));
vi.mock("next/navigation", () => ({
  useRouter: () => ({ push: vi.fn(), replace: vi.fn(), back: vi.fn(), refresh: vi.fn() }),
  useSearchParams: () => new URLSearchParams(""),
}));
vi.mock("@/hooks/use-translation", () => ({
  useTranslation: () => ({ t: (k: string) => k, locale: "en" }),
}));

import MemoryPage from "../page";

beforeEach(() => vi.clearAllMocks());

test("renders a row per document (source rendered)", async () => {
  render(<MemoryPage />);
  // "auto:session:" is rewritten to "Session: " in the row title.
  expect(await screen.findByText("Session: demo")).toBeInTheDocument();
  expect(screen.getByText("notes.md")).toBeInTheDocument();
});

test("Pagination shows current page / total page count", async () => {
  render(<MemoryPage />);
  // Wait for the list (and therefore the pagination footer) to render.
  await screen.findByText("notes.md");
  // 60 total / 20 per page = 3 pages; on the first page.
  expect(screen.getByText("1 / 3")).toBeInTheDocument();
});
