export interface EnvConfig {
  coreWs: string;
  authToken: string;
  reconnectInterval: number; // seconds
}

export function buildEnvConfig(env: Record<string, string | undefined>): EnvConfig {
  const authToken = env.OPEX_AUTH_TOKEN;
  if (!authToken) {
    throw new Error("OPEX_AUTH_TOKEN is required");
  }

  return {
    coreWs: env.OPEX_CORE_WS ?? "ws://localhost:18789",
    authToken,
    reconnectInterval: Number(env.RECONNECT_INTERVAL ?? "5"),
  };
}

export function wsToHttp(wsUrl: string): string {
  return wsUrl.replace(/^ws:/, "http:").replace(/^wss:/, "https:");
}
