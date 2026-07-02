import { test, expect, vi, beforeEach } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen } from "@testing-library/react";

const jobs = [
  {
    id: "1",
    name: "daily_report",
    agent: "Alice",
    cron: "0 9 * * *",
    timezone: "Europe/Samara",
    task: "Send the daily report",
    silent: false,
    announce_to: null,
    jitter_secs: 0,
    run_once: false,
    run_at: null,
    enabled: true,
    tool_policy: null,
  },
  {
    id: "2",
    name: "weekly_digest",
    agent: "Bob",
    cron: "0 8 * * 1",
    timezone: "Europe/Samara",
    task: "Compile the weekly digest",
    silent: true,
    announce_to: null,
    jitter_secs: 0,
    run_once: false,
    run_at: null,
    enabled: false,
    tool_policy: null,
  },
];

vi.mock("@/lib/queries", () => ({
  useCronJobs: () => ({ data: jobs, isLoading: false, error: null }),
  useCronRuns: () => ({ data: [], isLoading: false }),
  useCreateCronJob: () => ({ mutateAsync: vi.fn(), isPending: false }),
  useUpdateCronJob: () => ({ mutateAsync: vi.fn(), isPending: false }),
  useDeleteCronJob: () => ({ mutateAsync: vi.fn(), isPending: false }),
  useRunCronJob: () => ({ mutateAsync: vi.fn(), isPending: false }),
}));
vi.mock("@tanstack/react-query", () => ({ useQueryClient: () => ({ invalidateQueries: vi.fn() }) }));
vi.mock("@/lib/api", () => ({ apiGet: vi.fn(() => Promise.resolve([])) }));
vi.mock("@/hooks/use-ws-subscription", () => ({ useWsSubscription: vi.fn() }));
vi.mock("@/stores/auth-store", () => ({
  useAuthStore: (selector: (s: { agents: string[] }) => unknown) => selector({ agents: ["Alice", "Bob"] }),
}));
vi.mock("@/components/ui/cron-schedule-picker", () => ({ CronSchedulePicker: () => null }));
vi.mock("@/hooks/use-translation", () => ({
  useTranslation: () => ({ t: (k: string) => k, locale: "en" }),
}));

import TasksPage from "../page";

beforeEach(() => vi.clearAllMocks());

test("enabled job shows a success-tone status badge", () => {
  render(<TasksPage />);
  const badge = screen.getByText("cron.active");
  expect(badge).toHaveAttribute("data-variant", "success");
});

test("disabled job shows a secondary-tone (paused) status badge", () => {
  render(<TasksPage />);
  const badge = screen.getByText("cron.paused");
  expect(badge).toHaveAttribute("data-variant", "secondary");
});

test("renders a card per scheduled job", () => {
  render(<TasksPage />);
  expect(screen.getByText("daily_report")).toBeInTheDocument();
  expect(screen.getByText("weekly_digest")).toBeInTheDocument();
});
