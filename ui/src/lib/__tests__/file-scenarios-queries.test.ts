import { describe, it, expect, vi, beforeEach } from "vitest";

vi.mock("@/lib/api", () => ({
  apiGet: vi.fn(),
  apiPost: vi.fn(),
  apiPut: vi.fn(),
  apiDelete: vi.fn(),
  apiPatch: vi.fn(),
}));
vi.mock("sonner", () => ({ toast: { error: vi.fn(), success: vi.fn() } }));
vi.mock("@/stores/notification-store", () => ({
  useNotificationStore: vi.fn(() => vi.fn()),
}));
vi.mock("@/hooks/use-ws-subscription", () => ({
  useWsSubscription: vi.fn(),
}));

import { qk } from "@/lib/queries";

describe("file-scenarios query keys", () => {
  beforeEach(() => vi.clearAllMocks());

  it("exposes stable query keys", () => {
    expect(qk.fileScenarios).toEqual(["file-scenarios"]);
    expect(qk.fileScenarioAllowlist).toEqual(["file-scenarios", "allowlist"]);
  });
});
