import { vi, describe, it, expect, beforeEach } from "vitest";

const mockFetch = vi.fn();
vi.stubGlobal("fetch", mockFetch);

// Must import after stubbing fetch
import { useAuthStore } from "@/stores/auth-store";

function resetStore() {
  useAuthStore.setState({
    token: "",
    isAuthenticated: false,
    agents: [],
    agentIcons: {},
    version: "",
    lastFetched: 0,
  });
}

function jsonResponse(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

beforeEach(() => {
  mockFetch.mockReset();
  resetStore();
});

describe("initial state", () => {
  it("token is empty and isAuthenticated is false", () => {
    const state = useAuthStore.getState();
    expect(state.token).toBe("");
    expect(state.isAuthenticated).toBe(false);
    expect(state.agents).toEqual([]);
    expect(state.version).toBe("");
  });
});

describe("login", () => {
  it("sets token, isAuthenticated, agents, version on valid token", async () => {
    mockFetch
      .mockResolvedValueOnce(jsonResponse({ agents: [{ name: "Agent1", icon: "robot" }] })) // /api/agents
      .mockResolvedValueOnce(
        jsonResponse({
          version: "1.2.3",
          status: "ok",
          db: true,
        }),
      ); // /health

    const result = await useAuthStore.getState().login("valid-token");

    expect(result).toBe(true);
    const state = useAuthStore.getState();
    expect(state.token).toBe("valid-token");
    expect(state.isAuthenticated).toBe(true);
    expect(state.agents).toEqual(["Agent1"]);
    expect(state.version).toBe("1.2.3");
    expect(state.agentIcons).toEqual({ Agent1: "robot" });
  });

  it("returns 'invalid' on 401 response", async () => {
    mockFetch.mockResolvedValueOnce(new Response("", { status: 401 }));

    const result = await useAuthStore.getState().login("bad-token");
    expect(result).toBe("invalid");
    expect(useAuthStore.getState().isAuthenticated).toBe(false);
  });

  it("returns 'rate_limited' on 429 response", async () => {
    mockFetch.mockResolvedValueOnce(new Response("", { status: 429 }));

    const result = await useAuthStore.getState().login("any-token");
    expect(result).toBe("rate_limited");
  });

  it("returns 'error' on network failure", async () => {
    mockFetch.mockRejectedValueOnce(new Error("Network error"));

    const result = await useAuthStore.getState().login("any-token");
    expect(result).toBe("error");
  });

  it("returns 'error' on non-ok non-auth status", async () => {
    mockFetch.mockResolvedValueOnce(new Response("", { status: 500 }));

    const result = await useAuthStore.getState().login("any-token");
    expect(result).toBe("error");
  });

  it("handles missing agent_icons gracefully", async () => {
    mockFetch
      .mockResolvedValueOnce(jsonResponse({})) // /api/agents
      .mockResolvedValueOnce(jsonResponse({ agents: ["A"], version: "v1" })); // /health (no agent_icons)

    const result = await useAuthStore.getState().login("t");
    expect(result).toBe(true);
    expect(useAuthStore.getState().agentIcons).toEqual({});
  });
});

describe("logout", () => {
  it("clears all state", async () => {
    // Set up authenticated state first
    useAuthStore.setState({
      token: "tok",
      isAuthenticated: true,
      agents: ["A"],
      version: "v1",
      agentIcons: { A: "x" },
      lastFetched: 999,
    });

    useAuthStore.getState().logout();

    const s = useAuthStore.getState();
    expect(s.token).toBe("");
    expect(s.isAuthenticated).toBe(false);
    expect(s.agents).toEqual([]);
    expect(s.version).toBe("");
    expect(s.agentIcons).toEqual({});
    expect(s.lastFetched).toBe(0);
  });
});

describe("restore", () => {
  it("returns false when no token", async () => {
    const result = await useAuthStore.getState().restore();
    expect(result).toBe(false);
  });

  it("returns true when token is valid", async () => {
    useAuthStore.setState({ token: "saved-token" });

    mockFetch
      .mockResolvedValueOnce(jsonResponse({}))
      .mockResolvedValueOnce(jsonResponse({ agents: ["A"], version: "v1" }));

    const result = await useAuthStore.getState().restore();
    expect(result).toBe(true);
    expect(useAuthStore.getState().isAuthenticated).toBe(true);
  });

  it("clears token and returns false when token is invalid", async () => {
    useAuthStore.setState({ token: "stale-token" });
    mockFetch.mockResolvedValueOnce(new Response("", { status: 401 }));

    const result = await useAuthStore.getState().restore();
    expect(result).toBe(false);
    expect(useAuthStore.getState().token).toBe("");
  });
});

describe("refreshIfStale", () => {
  it("calls restore when lastFetched is stale (>60s)", async () => {
    useAuthStore.setState({ token: "tok", lastFetched: Date.now() - 120_000 });

    // Resolve once fetch is actually called so we don't rely on arbitrary timers
    let resolveFetch!: (v: Response) => void;
    const fetchCalled = new Promise<Response>((res) => { resolveFetch = res; });
    mockFetch.mockImplementationOnce(() => { resolveFetch(jsonResponse({ agents: [{ name: "X" }] })); return fetchCalled; });
    mockFetch.mockResolvedValueOnce(jsonResponse({ version: "v2" }));

    useAuthStore.getState().refreshIfStale();
    await fetchCalled;

    expect(mockFetch).toHaveBeenCalled();
  });

  it("does not call restore when lastFetched is fresh", () => {
    useAuthStore.setState({ token: "tok", lastFetched: Date.now() });

    useAuthStore.getState().refreshIfStale();

    expect(mockFetch).not.toHaveBeenCalled();
  });
});
