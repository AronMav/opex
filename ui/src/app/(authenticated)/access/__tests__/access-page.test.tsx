import { test, expect, vi, beforeEach } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen, fireEvent } from "@testing-library/react";

const agentDetail = {
  name: "atlas",
  access: { mode: "restricted", owner_id: "owner-1" },
};

const pending = [
  {
    code: "ABC123",
    channel_user_id: "tg:42",
    display_name: "Alice",
    created_at: "2026-01-01T00:00:00Z",
  },
];

const users = [
  {
    channel_user_id: "tg:7",
    display_name: "Bob",
    approved_at: "2026-01-02T00:00:00Z",
  },
];

// Route apiGet by URL: agent detail, pending pairings, authorized users.
vi.mock("@/lib/api", () => ({
  apiGet: vi.fn((url: string) => {
    if (url.endsWith("/pending")) return Promise.resolve({ pending });
    if (url.endsWith("/users")) return Promise.resolve({ users });
    return Promise.resolve(agentDetail); // /api/agents/{agent}
  }),
  apiPost: vi.fn(() => Promise.resolve({})),
  apiPut: vi.fn(() => Promise.resolve({})),
  apiDelete: vi.fn(() => Promise.resolve({})),
}));
vi.mock("@/lib/queries", () => ({
  useAgents: () => ({
    data: [{ name: "atlas" }],
    isLoading: false,
    error: null,
    refetch: vi.fn(),
  }),
  qk: { agents: ["agents"] },
}));
vi.mock("@tanstack/react-query", () => ({
  useQueryClient: () => ({ invalidateQueries: vi.fn() }),
}));
vi.mock("sonner", () => ({
  toast: Object.assign(vi.fn(), { success: vi.fn(), error: vi.fn(), warning: vi.fn() }),
}));
vi.mock("@/lib/format", () => ({ formatDate: () => "2026-01-02" }));
vi.mock("@/hooks/use-translation", () => ({
  useTranslation: () => ({ t: (k: string) => k, locale: "en" }),
}));

import AccessPage from "../page";

beforeEach(() => vi.clearAllMocks());

test("renders the agent card", async () => {
  render(<AccessPage />);
  expect(await screen.findByText("atlas")).toBeInTheDocument();
});

test("expanding exposes the open/restricted SegmentedControl radios and Approve/Reject", async () => {
  render(<AccessPage />);
  // Wait for the agent + its access data to load.
  const header = await screen.findByText("atlas");
  // Expand the accordion.
  fireEvent.click(header);

  // SegmentedControl renders role=radio for open + restricted.
  const radios = await screen.findAllByRole("radio");
  expect(radios.length).toBe(2);
  const radioLabels = radios.map((r) => r.textContent);
  expect(radioLabels).toContain("access.open");
  expect(radioLabels).toContain("access.restricted");

  // Pending pairing exposes Approve + Reject actions.
  expect(await screen.findByText("access.approve")).toBeInTheDocument();
  expect(screen.getByText("access.reject")).toBeInTheDocument();
});
