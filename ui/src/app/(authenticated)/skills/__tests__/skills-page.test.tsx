import { test, expect, vi, beforeEach } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen, fireEvent } from "@testing-library/react";

const skills = [
  {
    name: "research-task",
    description: "Deep research helper",
    triggers: ["research", "investigate"],
    tools_required: ["web_search"],
    priority: 5,
    instructions_len: 1200,
    state: "active",
    last_used_at: null,
    pinned: false,
  },
  {
    name: "stale-skill",
    description: "",
    triggers: [],
    tools_required: [],
    priority: 0,
    instructions_len: 40,
    state: "stale",
    last_used_at: null,
    pinned: false,
  },
];

vi.mock("@/lib/queries", () => ({
  useSkills: () => ({ data: skills, isLoading: false, error: null }),
  useCuratorStatus: () => ({ data: { enabled: false } }),
  useCuratorDecisions: () => ({ data: {} }),
  useSkillVersions: () => ({ data: { versions: [] }, isLoading: false }),
  useSkillCuratorDecisions: () => ({ data: { decisions: [] } }),
  qk: { skills: ["skills"], curatorStatus: ["curator", "status"], curatorRuns: ["curator", "runs"] },
}));
vi.mock("@tanstack/react-query", () => ({
  useQueryClient: () => ({ invalidateQueries: vi.fn() }),
}));
vi.mock("@/lib/api", () => ({
  apiGet: vi.fn(() => Promise.resolve({})),
  apiPut: vi.fn(() => Promise.resolve({})),
  apiPost: vi.fn(() => Promise.resolve({})),
  apiPatch: vi.fn(() => Promise.resolve({})),
  apiDelete: vi.fn(() => Promise.resolve({})),
}));
vi.mock("sonner", () => ({
  toast: Object.assign(vi.fn(), { success: vi.fn(), error: vi.fn(), warning: vi.fn() }),
}));
vi.mock("@/lib/format", () => ({ relativeTime: () => "just now" }));
vi.mock("@/hooks/use-translation", () => ({
  useTranslation: () => ({ t: (k: string) => k, locale: "en" }),
}));

import SkillsPage from "../page";

beforeEach(() => vi.clearAllMocks());

test("renders a card per skill with a StatusBadge (data-variant)", () => {
  render(<SkillsPage />);
  expect(screen.getByText("research-task")).toBeInTheDocument();
  expect(screen.getByText("stale-skill")).toBeInTheDocument();
  // active skill → StatusBadge maps to a success-tone Badge
  const activeBadge = screen.getByText("skills.state_active");
  expect(activeBadge).toHaveAttribute("data-variant", "success");
  // stale skill → warning-tone Badge
  const staleBadge = screen.getByText("skills.state_stale");
  expect(staleBadge).toHaveAttribute("data-variant", "warning");
});

test("filtering to zero results shows the no-match (not onboarding) empty state", () => {
  render(<SkillsPage />);
  const search = screen.getByPlaceholderText("skills.search_placeholder");
  fireEvent.change(search, { target: { value: "zzz-no-such-skill" } });
  // "no matches" empty state is shown...
  expect(screen.getByText("skills.no_matches")).toBeInTheDocument();
  // ...and a reset-filters action is offered
  expect(screen.getByText("skills.reset_filters")).toBeInTheDocument();
  // ...while the onboarding empty state is NOT shown.
  expect(screen.queryByText("skills.no_skills")).not.toBeInTheDocument();
});
