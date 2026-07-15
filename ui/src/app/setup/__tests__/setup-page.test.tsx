import { test, expect, vi, beforeEach } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";

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
const apiPut = vi.fn().mockResolvedValue({});
const apiDelete = vi.fn().mockResolvedValue(undefined);

vi.mock("@/lib/api", () => ({
  apiGet: (path: string) => apiGet(path),
  apiPost: (path: string, body?: unknown) => apiPost(path, body),
  apiPut: (path: string, body?: unknown) => apiPut(path, body),
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

test("step 1 seeds the Default profile's text slot; step 2 creates the agent with profile: Default", async () => {
  apiGet.mockImplementation(async (path: string) => {
    if (path === "/api/setup/requirements") return requirementsResult;
    if (path === "/api/provider-types") return { provider_types: [] };
    if (path === "/api/network/addresses")
      return { wan: null, tailscale: null, lan: [], mdns: null };
    if (path === "/api/providers/prov-1/models") return { models: [{ id: "gpt-test" }] };
    if (path === "/api/profiles")
      return { profiles: [{ id: "profile-1", name: "Default", slots: {} }] };
    return {};
  });
  apiPost.mockImplementation(async (path: string) => {
    if (path === "/api/providers") return { id: "prov-1", name: "myprov-default" };
    return {};
  });

  render(<SetupPage />);

  // requirements → provider
  fireEvent.click(await screen.findByText("common.next"));

  // fill provider type (manual input — provider-types list is empty) + model
  fireEvent.change(screen.getByPlaceholderText("openai, anthropic, ollama..."), {
    target: { value: "myprov" },
  });
  fireEvent.change(screen.getByPlaceholderText("setup.model_placeholder"), {
    target: { value: "gpt-test" },
  });
  fireEvent.click(screen.getByText("common.next"));

  // step 1: provider created, test call ok, Default profile's text slot seeded
  await waitFor(() => expect(apiPost).toHaveBeenCalledWith("/api/providers", expect.objectContaining({
    provider_type: "myprov",
    default_model: "gpt-test",
  })));
  await waitFor(() => expect(apiPut).toHaveBeenCalledWith("/api/profiles/profile-1", {
    slots: { text: [{ provider: "myprov-default", model: "gpt-test" }] },
  }));

  // now on the agent step
  fireEvent.change(await screen.findByPlaceholderText("Opex"), {
    target: { value: "TestAgent" },
  });
  fireEvent.click(screen.getByText("common.next"));

  // step 2: agent created via profile, no legacy provider/model/provider_connection fields
  await waitFor(() => expect(apiPost).toHaveBeenCalledWith("/api/agents", {
    name: "TestAgent",
    language: "ru",
    profile: "Default",
    temperature: 1.0,
  }));
});
