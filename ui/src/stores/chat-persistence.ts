// ── chat-persistence.ts ─────────────────────────────────────────────────────
// localStorage helpers for persisting last-selected agent and session.

const LAST_SESSION_KEY = "opex.chat.lastSession";

interface LastSessionData {
  agent?: string;
  sessions?: Record<string, string>;
  /** Legacy: global session ID (pre-per-agent). */
  sessionId?: string;
}

function loadLastSession(): LastSessionData {
  try {
    const saved = localStorage.getItem(LAST_SESSION_KEY);
    if (saved) return JSON.parse(saved);
  } catch { /* ignore */ }
  return {};
}

export function saveLastSession(agent: string, sessionId?: string) {
  try {
    const data = loadLastSession();
    data.agent = agent;
    if (sessionId) {
      data.sessions = { ...data.sessions, [agent]: sessionId };
    } else {
      delete data.sessions?.[agent];
    }
    localStorage.setItem(LAST_SESSION_KEY, JSON.stringify(data));
  } catch { /* ignore */ }
}

export function getInitialAgent(agents: string[]): string {
  const { agent: savedAgent } = loadLastSession();
  if (savedAgent && agents.includes(savedAgent)) return savedAgent;
  return agents[0] || "";
}

export function getLastSessionId(agent?: string): string | undefined {
  const data = loadLastSession();
  // When an agent is specified, return only that agent's per-agent session ID.
  // Do NOT fall back to the legacy global sessionId — it belongs to a different
  // agent and would trigger the cross-agent URL resolver to switch back to that agent.
  if (agent) return data.sessions?.[agent];
  return data.sessionId;
}
