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

describe("checkpoint query keys", () => {
  beforeEach(() => vi.clearAllMocks());

  it("qk.checkpoints производит стабильный ключ", () => {
    expect(qk.checkpoints("TestAgent")).toEqual(["agents", "TestAgent", "checkpoints"]);
  });

  it("qk.checkpoints с пустой строкой", () => {
    expect(qk.checkpoints("")).toEqual(["agents", "", "checkpoints"]);
  });
});
