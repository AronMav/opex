import { test, expect, vi, beforeEach } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen } from "@testing-library/react";

// ── @/lib/api: route apiGet by path; everything else resolves ─────────────
const requirementsResult = {
  ok: true,
  checks: {
    docker: { status: "ok", message: "x" },
    postgresql: { status: "ok", message: "x" },
    disk_space: { status: "ok", message: "x" },
  },
};

const apiGet = vi.fn(async (path: string) => {
  if (path === "/api/setup/requirements") return requirementsResult;
  if (path === "/api/provider-types") return { provider_types: [] };
  if (path === "/api/network/addresses")
    return { wan: null, tailscale: null, lan: [], mdns: null };
  return {};
});
const apiPost = vi.fn().mockResolvedValue({});
const apiDelete = vi.fn().mockResolvedValue(undefined);

vi.mock("@/lib/api", () => ({
  apiGet: (path: string) => apiGet(path),
  apiPost: (path: string, body?: unknown) => apiPost(path, body),
  apiDelete: (path: string) => apiDelete(path),
}));

// use-translation: identity so keys render verbatim.
vi.mock("@/hooks/use-translation", () => ({
  useTranslation: () => ({ t: (k: string) => k }),
}));

// sonner: stub toast surface.
vi.mock("sonner", () => ({
  toast: { error: vi.fn(), success: vi.fn(), message: vi.fn() },
}));

import SetupPage from "../page";

beforeEach(() => {
  localStorage.clear();
  vi.clearAllMocks();
});

test("renders the OPEX brand", async () => {
  render(<SetupPage />);
  expect(await screen.findByText("OPEX")).toBeInTheDocument();
});

test("renders 4 stepper circles", () => {
  const { container } = render(<SetupPage />);
  const circles = container.querySelectorAll('[class*="rounded-full"]');
  expect(circles.length).toBe(4);
});

test("shows the requirements step heading", async () => {
  render(<SetupPage />);
  expect(
    await screen.findByText("setup.step_requirements"),
  ).toBeInTheDocument();
});
