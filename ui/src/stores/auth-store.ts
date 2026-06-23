import { create } from "zustand";
import { devtools, persist, subscribeWithSelector, createJSONStorage } from "zustand/middleware";
import { readWithLegacy } from "@/stores/ls-migration";

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
              const agentList: Array<{ name: string; icon?: string | null; icon_url?: string | null }> =
                Array.isArray(agentsData.agents) ? agentsData.agents : [];

              const agentNames = agentList.map((a) => a.name);
              // Stores the pre-signed `/uploads/{filename}?sig=&exp=` URL produced
              // by the backend with HISTORICAL_URL_TTL_SECS. Plain `/uploads/{name}`
              // returns 403 when `uploads.require_signature = true`.
              const icons: Record<string, string | null> = {};
              for (const a of agentList) {
                icons[a.name] = a.icon_url || null;
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
            import("idb-keyval").then(({ del }) => del("opex-rq")).catch((e) => console.warn("[auth] IDB cache clear failed:", e));
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
          name: "opex.auth.token",
          partialize: (state) => ({ token: state.token }),
          // Security: auth token stored in localStorage for cross-tab login persistence.
          // Accepted trade-off for a personal home server: XSS would expose the token
          // persistently, but the alternative (sessionStorage) requires re-login on every tab.
          // Legacy migration: moves sessionStorage or old hydeclaw.* key to new opex.* key.
          storage: createJSONStorage(() => {
            // Migrate token from sessionStorage (old pattern) to localStorage.
            const oldKey = "hydeclaw.auth.token";
            const newKey = "opex.auth.token";
            const sessionVal = sessionStorage.getItem(oldKey);
            if (sessionVal) {
              if (!localStorage.getItem(newKey)) localStorage.setItem(newKey, sessionVal);
              sessionStorage.removeItem(oldKey);
            }
            // Migrate from legacy localStorage key (hydeclaw → opex) via shim.
            readWithLegacy(newKey, oldKey);
            return localStorage;
          }),
        },
      ),
    ),
    { name: "AuthStore", enabled: process.env.NODE_ENV !== "production" },
  ),
);
