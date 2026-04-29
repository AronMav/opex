import { create } from "zustand";
import { devtools, persist, subscribeWithSelector, createJSONStorage } from "zustand/middleware";

export type LoginResult = true | "invalid" | "rate_limited" | "error";

interface AuthState {
  token: string;
  isAuthenticated: boolean;
  version: string;
  agents: string[];
  agentIcons: Record<string, string | null>;
  lastFetched: number;
  login: (token: string) => Promise<LoginResult>;
  logout: () => void;
  restore: () => Promise<boolean>;
  refreshIfStale: () => void;
}

export const useAuthStore = create<AuthState>()(
  devtools(
    subscribeWithSelector(
      persist(
        (set, get) => ({
          token: "",
          isAuthenticated: false,
          version: "",
          agents: [],
          agentIcons: {},
          lastFetched: 0,

          login: async (token: string): Promise<LoginResult> => {
            try {
              const authHeaders = { Authorization: `Bearer ${token}` };

              // Fetch agent list (authenticated) — also provides icons, replaces /health agent data
              const resp = await fetch("/api/agents", { headers: authHeaders });
              if (resp.status === 429) return "rate_limited";
              if (resp.status === 401) return "invalid";
              if (!resp.ok) return "error";

              const agentsData = await resp.json();
              const agentList: Array<{ name: string; icon?: string | null }> =
                Array.isArray(agentsData.agents) ? agentsData.agents : [];

              const agentNames = agentList.map((a) => a.name);
              const icons: Record<string, string | null> = {};
              for (const a of agentList) {
                icons[a.name] = a.icon || null;
              }

              // Fetch version from /health (unauthenticated — only status/version exposed there)
              const healthResp = await fetch("/health");
              const healthData = healthResp.ok ? await healthResp.json() : { version: "" };

              set({
                token,
                isAuthenticated: true,
                version: healthData.version || "",
                agents: agentNames,
                agentIcons: icons,
                lastFetched: Date.now(),
              });
              return true;
            } catch (e) {
              console.error("[auth] login failed:", e);
              return "error";
            }
          },

          logout: () => {
            set({
              token: "",
              isAuthenticated: false,
              version: "",
              agents: [],
              agentIcons: {},
              lastFetched: 0,
            });
            // Clear cached API data from IndexedDB to prevent data leakage after logout
            import("idb-keyval").then(({ del }) => del("hydeclaw-rq")).catch((e) => console.warn("[auth] IDB cache clear failed:", e));
          },

          restore: async () => {
            const token = get().token;
            if (!token) return false;
            const result = await get().login(token);
            if (result === "invalid") {
              // Token changed (e.g. after reinstall) — clear stale token
              // to avoid burning rate-limiter attempts on every page load.
              get().logout();
            }
            return result === true;
          },

          refreshIfStale: () => {
            if (Date.now() - get().lastFetched > 60_000) {
              get().restore();
            }
          },
        }),
        {
          name: "hydeclaw.auth.token",
          partialize: (state) => ({ token: state.token }),
          // Security: auth token stored in localStorage for cross-tab login persistence.
          // Accepted trade-off for a personal home server: XSS would expose the token
          // persistently, but the alternative (sessionStorage) requires re-login on every tab.
          // Legacy migration below moves any old sessionStorage token to localStorage.
          storage: createJSONStorage(() => {
            // Migrate any token that was previously stored in sessionStorage back to localStorage.
            // localStorage keeps the user logged in across tabs and page refreshes, which is
            // appropriate for a personal home server where the user is the only operator.
            const key = "hydeclaw.auth.token";
            const sessionVal = sessionStorage.getItem(key);
            if (sessionVal) {
              if (!localStorage.getItem(key)) localStorage.setItem(key, sessionVal);
              sessionStorage.removeItem(key);
            }
            return localStorage;
          }),
        },
      ),
    ),
    { name: "AuthStore", enabled: process.env.NODE_ENV !== "production" },
  ),
);
