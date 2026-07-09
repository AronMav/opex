/**
 * Session loop — manages WS connection lifecycle per agent with reconnect.
 * Port of spawn_session_loop() + run_session() from lib.rs
 */

import { BridgeHandle, type OutboundAction } from "./bridge";
import type { ChannelOutbound } from "./types";
import { wsToHttp } from "./config";

const MAX_BACKOFF_MS = 300_000; // 5 minutes
const PING_INTERVAL_MS = 30_000;

/** Calculate reconnect delay in ms with exponential backoff, capped at 300s. */
export function calcBackoff(
  consecutiveFailures: number,
  baseIntervalSecs: number,
): number {
  if (consecutiveFailures <= 1) {
    return baseIntervalSecs * 1000;
  }
  const multiplier = 1 << Math.min(consecutiveFailures - 1, 6);
  return Math.min(baseIntervalSecs * multiplier * 1000, MAX_BACKOFF_MS);
}

export interface SessionConfig {
  agentName: string;
  channelType: string;
  credential: string;
  coreWs: string;
  authToken: string;
  reconnectInterval: number; // seconds
  channelConfig?: Record<string, unknown>;
  /** Channel-specific formatting instructions for the LLM system prompt. */
  formattingPrompt?: string;
}

export interface ChannelDriver {
  start: () => Promise<void>;
  stop: () => Promise<void>;
  onAction?: (action: OutboundAction) => Promise<void>;
}

export type CreateDriverFn = (
  bridge: BridgeHandle,
  credential: string,
  channelConfig: Record<string, unknown> | undefined,
  language: string,
  typingMode: string,
) => ChannelDriver;

/**
 * Spawn a reconnecting session loop for one agent channel.
 * Returns an AbortController that can be used to stop the loop.
 */
export function spawnSessionLoop(
  config: SessionConfig,
  createDriver: CreateDriverFn,
): AbortController {
  const controller = new AbortController();

  (async () => {
    let consecutiveFailures = 0;

    while (!controller.signal.aborted) {
      try {
        await runSession(config, createDriver, controller.signal);
        consecutiveFailures = 0;
      } catch (err) {
        consecutiveFailures++;
        console.error(
          `[${config.agentName}] session error (failures=${consecutiveFailures}):`,
          err,
        );
      }

      if (controller.signal.aborted) break;

      const delay = calcBackoff(
        consecutiveFailures,
        config.reconnectInterval,
      );
      console.log(
        `[${config.agentName}] reconnecting in ${delay / 1000}s...`,
      );
      await sleep(delay, controller.signal);
    }
  })();

  return controller;
}

async function runSession(
  config: SessionConfig,
  createDriver: CreateDriverFn,
  signal: AbortSignal,
): Promise<void> {
  const httpBase = wsToHttp(config.coreWs);

  // Fetch a one-time WS ticket (avoids exposing static token in URL/logs).
  // F083: bound the fetch by both the loop's abort signal (so shutdown/reconnect
  // cancels an in-flight ticket request) and a hard timeout (so a stalled
  // half-open connection cannot wedge session startup forever).
  const ticketResp = await fetch(`${httpBase}/api/auth/ws-ticket`, {
    method: "POST",
    headers: { Authorization: `Bearer ${config.authToken}` },
    signal: AbortSignal.any([signal, AbortSignal.timeout(15_000)]),
  });
  if (!ticketResp.ok) {
    throw new Error(`WS ticket request failed: ${ticketResp.status}`);
  }
  const { ticket } = (await ticketResp.json()) as { ticket: string };
  const wsUrl = `${config.coreWs}/ws/channel/${config.agentName}?ticket=${ticket}`;
  // Audit 2026-05-08: redact the ticket query param when logging — even
  // though tickets are one-time and short-lived, journald/systemd logs are
  // searchable history and the URL fragment makes hand-crafted lookalikes
  // easier to spot during forensic review.
  const wsUrlSafe = `${config.coreWs}/ws/channel/${config.agentName}?ticket=***`;
  console.log(`[${config.agentName}] connecting to ${wsUrlSafe}...`);
  const ws = new WebSocket(wsUrl);

  const bridge = new BridgeHandle(
    (msg) => {
      if (ws.readyState === WebSocket.OPEN) {
        ws.send(msg);
      }
    },
    httpBase,
    config.authToken,
    config.agentName,
  );

  return new Promise<void>((resolve, reject) => {
    let driver: ChannelDriver | null = null;
    let pingTimer: ReturnType<typeof setInterval> | null = null;
    // F121: declared here (not just in the message scope) so cleanup() can clear
    // it — a pre-handshake close/error otherwise leaves this 30s timer armed to
    // fire a misleading "handshake timeout" and re-close the connection.
    let handshakeTimer: ReturnType<typeof setTimeout> | null = null;
    let handshakeComplete = false;
    let cleaned = false;

    const cleanup = async () => {
      if (cleaned) return;
      cleaned = true;
      if (pingTimer) clearInterval(pingTimer);
      if (handshakeTimer) clearTimeout(handshakeTimer);
      bridge.clearAll();
      if (driver) {
        try {
          await driver.stop();
        } catch (e) {
          console.warn(`[${config.agentName}] driver stop failed:`, (e as Error).message);
        }
      }
      if (ws.readyState === WebSocket.OPEN || ws.readyState === WebSocket.CONNECTING) {
        ws.close();
      }
    };

    ws.addEventListener("open", () => {
      console.log(`[${config.agentName}] connected, sending ready...`);
      bridge.sendReady(config.channelType, "1.0.0", config.formattingPrompt);

      pingTimer = setInterval(() => {
        if (ws.readyState === WebSocket.OPEN) {
          bridge.sendPing();
        }
      }, PING_INTERVAL_MS);
    });

    const HANDSHAKE_TIMEOUT_MS = 30_000;
    handshakeTimer = setTimeout(() => {
      if (!handshakeComplete) {
        console.error(`[${config.agentName}] handshake timeout — no config received in 30s, closing`);
        ws.close();
      }
    }, HANDSHAKE_TIMEOUT_MS);

    ws.addEventListener("message", async (event) => {
      try {
        const msg = JSON.parse(
          typeof event.data === "string" ? event.data : event.data.toString(),
        ) as ChannelOutbound;


        if (!handshakeComplete && msg.type === "config") {
          handshakeComplete = true;
          clearTimeout(handshakeTimer);
          bridge.setOwnerId(msg.owner_id ?? undefined);

          driver = createDriver(
            bridge,
            config.credential,
            config.channelConfig,
            msg.language,
            msg.typing_mode,
          );

          try {
            await driver.start();
            console.log(`[${config.agentName}] driver started`);
          } catch (err) {
            console.error(`[${config.agentName}] driver start failed:`, err);
            await cleanup();
            reject(err);
          }
          return;
        }

        if (msg.type === "reload") {
          console.log(`[${config.agentName}] received reload signal`);
          await cleanup();
          resolve();
          return;
        }

        // Dispatch to bridge
        const action = bridge.handleOutbound(msg);
        if (action && driver) {
          // Execute channel action via driver, then report result
          try {
            if (driver.onAction) {
              const ACTION_TIMEOUT_MS = 30_000;
              await Promise.race([
                driver.onAction(action),
                new Promise<never>((_, reject) =>
                  setTimeout(() => reject(new Error("action execution timeout (30s)")), ACTION_TIMEOUT_MS),
                ),
              ]);
            }
            bridge.sendActionResult(action.actionId, true);
          } catch (err) {
            bridge.sendActionResult(
              action.actionId,
              false,
              String(err),
            );
          }
        }
      } catch (err) {
        console.error(`[${config.agentName}] message parse error:`, err);
      }
    });

    ws.addEventListener("close", async () => {
      console.log(`[${config.agentName}] WS closed`);
      await cleanup();
      resolve();
    });

    ws.addEventListener("error", async (err) => {
      console.error(`[${config.agentName}] WS error:`, err);
      await cleanup();
      reject(new Error("WebSocket error"));
    });

    signal.addEventListener("abort", async () => {
      await cleanup();
      resolve();
    }, { once: true });
  });
}

function sleep(ms: number, signal?: AbortSignal): Promise<void> {
  return new Promise((resolve) => {
    const timer = setTimeout(resolve, ms);
    signal?.addEventListener("abort", () => {
      clearTimeout(timer);
      resolve();
    }, { once: true });
  });
}
