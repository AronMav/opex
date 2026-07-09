import type { WsEventType, WsEventOf } from "@/types/ws";

/** Generic handler type: narrows payload based on event type. */
export type WsHandler<T extends WsEventType = WsEventType> = (msg: WsEventOf<T>) => void;

/** Shape of a parsed inbound WebSocket message. */
export type WsMessage = { type: string; [key: string]: unknown };

type AnyWsHandler = (msg: WsMessage) => void;
type ConnectionListener = (connected: boolean) => void;

export class WsManager {
  private ws: WebSocket | null = null;
  private handlers = new Map<string, Set<AnyWsHandler>>();
  private connListeners = new Set<ConnectionListener>();
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private pingTimer: ReturnType<typeof setInterval> | null = null;
  private delay = 1000;
  private disposed = false;
  private earlyMessages: Array<WsMessage> = [];
  private visibilityHandler: (() => void) | null = null;
  /** Timestamp of last received message (for stale detection on wake). */
  private lastMessageAt = 0;

  connected = false;

  constructor(
    private url: string,
    private token: string,
  ) {
    // Force reconnect when mobile screen turns on (WS may be silently dead)
    this.visibilityHandler = () => {
      if (document.visibilityState !== "visible" || this.disposed) return;
      const staleMs = Date.now() - this.lastMessageAt;
      // If no message received for >30s, the connection is likely dead
      if (staleMs > 30_000 && this.ws) {
        this.ws.close();
        // onclose will trigger scheduleReconnect, but force immediate reconnect
        this.delay = 100;
      }
    };
    if (typeof document !== "undefined") {
      document.addEventListener("visibilitychange", this.visibilityHandler);
    }
  }

  async connect() {
    if (this.disposed) return;
    try {
      // Fetch a one-time WS ticket (avoids exposing static token in URL/logs)
      const resp = await fetch("/api/auth/ws-ticket", {
        method: "POST",
        headers: { Authorization: `Bearer ${this.token}` },
      });
      if (!resp.ok) {
        // F056: distinguish auth failure from a transient outage. A rotated/invalid
        // token returns 401/403 on every attempt; backing off and re-polling forever
        // leaves the real-time channel permanently dead with no re-auth. Route auth
        // failures through the shared logout+redirect instead of reconnect.
        if (resp.status === 401 || resp.status === 403) {
          console.warn("[ws] ticket endpoint returned auth failure, redirecting to login");
          this.disconnect();
          const { handleUnauthorized } = await import("@/lib/api");
          handleUnauthorized();
          return;
        }
        console.warn("[ws] ticket endpoint failed, scheduling reconnect");
        this.scheduleReconnect();
        return;
      }
      const { ticket } = await resp.json();
      const authParam = `ticket=${ticket}`;
      if (this.disposed) return; // re-check after await

      const sep = this.url.includes("?") ? "&" : "?";
      this.ws = new WebSocket(`${this.url}${sep}${authParam}`);
      this.ws.onopen = () => {
        this.connected = true;
        this.delay = 1000;
        this.lastMessageAt = Date.now();
        this.notifyConnection(true);
        this.startPing();
      };
      this.ws.onmessage = (ev) => {
        this.lastMessageAt = Date.now();
        let msg: WsMessage;
        try {
          msg = JSON.parse(ev.data);
        } catch {
          return; // ignore malformed JSON
        }
        try {
          if (msg.type === "pong") {
            this.pongReceived = true;
            return;
          }
          this.dispatch(msg.type, msg);
        } catch (e) {
          console.error("[ws] handler error:", msg.type, e);
        }
      };
      this.ws.onclose = () => {
        this.connected = false;
        this.notifyConnection(false);
        this.stopPing();
        this.scheduleReconnect();
      };
      this.ws.onerror = () => {
        this.ws?.close();
      };
    } catch (e) {
      console.error("[ws] connect error:", e);
      this.scheduleReconnect();
    }
  }

  disconnect() {
    this.disposed = true;
    this.stopPing();
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    if (this.visibilityHandler && typeof document !== "undefined") {
      document.removeEventListener("visibilitychange", this.visibilityHandler);
      this.visibilityHandler = null;
    }
    this.ws?.close();
    this.ws = null;
  }

  send(msg: Record<string, unknown>): boolean {
    if (this.ws?.readyState === WebSocket.OPEN) {
      this.ws.send(JSON.stringify(msg));
      return true;
    }
    return false;
  }

  on<T extends WsEventType>(type: T, handler: WsHandler<T>) {
    if (!this.handlers.has(type)) this.handlers.set(type, new Set());
    this.handlers.get(type)!.add(handler as unknown as AnyWsHandler);
    // Replay any buffered messages for this type
    if (this.earlyMessages.length > 0) {
      const remaining: typeof this.earlyMessages = [];
      for (const msg of this.earlyMessages) {
        if (msg.type === type) {
          (handler as unknown as AnyWsHandler)(msg);
        } else {
          remaining.push(msg);
        }
      }
      this.earlyMessages = remaining;
    }
  }

  off<T extends WsEventType>(type: T, handler: WsHandler<T>) {
    this.handlers.get(type)?.delete(handler as unknown as AnyWsHandler);
  }

  addConnectionListener(fn: ConnectionListener) {
    this.connListeners.add(fn);
  }

  removeConnectionListener(fn: ConnectionListener) {
    this.connListeners.delete(fn);
  }

  private dispatch(type: string, msg: WsMessage) {
    const handlers = this.handlers.get(type);
    if (handlers && handlers.size > 0) {
      handlers.forEach((h) => h(msg));
    } else if (this.earlyMessages.length < 100) {
      this.earlyMessages.push(msg);
    }
  }

  private notifyConnection(connected: boolean) {
    this.connListeners.forEach((fn) => fn(connected));
  }

  private scheduleReconnect() {
    if (this.disposed) return;
    this.reconnectTimer = setTimeout(() => {
      void this.connect();
    }, this.delay);
    this.delay = Math.min(this.delay * 2, 30000);
  }

  private pongReceived = true;

  private startPing() {
    this.pingTimer = setInterval(() => {
      if (!this.pongReceived) {
        // No pong received since last ping — connection is dead
        console.warn("[ws] ping timeout — no pong received, reconnecting");
        this.ws?.close(4000, "ping timeout");
        return;
      }
      this.pongReceived = false;
      this.send({ type: "ping" });
    }, 25000);
  }

  private stopPing() {
    if (this.pingTimer) {
      clearInterval(this.pingTimer);
      this.pingTimer = null;
    }
    this.pongReceived = true;
  }
}
