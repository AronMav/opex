/**
 * IRC channel driver using plain TCP.
 * Port of crates/opex-channel/src/channels/irc.rs
 */

import { connect, type Socket } from "net";
import type { BridgeHandle } from "../bridge";
import type { ChannelDriver } from "../session";
import type { IncomingMessageDto } from "../types";
import { getStrings } from "../localization";
import { splitText, parseDirectives, parseUserCommand, commonMarkToIrc } from "./common";
import { isOwnerCommand, runOwnerCommand } from "../owner-commands";

const MAX_IRC_LEN = 450;

export function createIrcDriver(
  bridge: BridgeHandle,
  credential: string,
  channelConfig: Record<string, unknown> | undefined,
  language: string,
  _typingMode: string,
): ChannelDriver {
  const strings = getStrings(language);
  const server = (channelConfig?.server as string) ?? "irc.libera.chat";
  const port = (channelConfig?.port as number) ?? 6667;
  const nick = credential;
  const password = channelConfig?.password as string | undefined;
  const ircChannels = (channelConfig?.channels as string[]) ?? [];

  let socket: Socket | null = null;
  const activeRequests = new Map<string, string>();
  // F022: driver-level reconnect state. The session loop only reconnects on the
  // CORE WebSocket dropping, never on the IRC TCP socket, so a server-side
  // disconnect used to leave the bot silently deaf until a full process restart.
  let stopped = false;
  let reconnectAttempts = 0;

  function sendRaw(line: string): void {
    socket?.write(`${line}\r\n`);
  }

  function sendPrivmsg(target: string, text: string): void {
    const parts = splitText(text, MAX_IRC_LEN, false);
    for (const part of parts) {
      sendRaw(`PRIVMSG ${target} :${part}`);
    }
  }

  async function handleMessage(from: string, target: string, text: string): Promise<void> {
    const replyTo = target.startsWith("#") ? target : from;

    // Access control
    const { allowed, isOwner } = await bridge.checkAccess(from);
    if (!allowed && !isOwner) {
      const code = await bridge.createPairingCode(from, from);
      sendPrivmsg(replyTo, strings.accessRestricted(code));
      return;
    }

    // Owner commands (audit 2026-05-08, group DD): without this branch the
    // bot has no way to bootstrap pairing requests over IRC — every fresh
    // user gets a code, but the owner cannot /approve it.
    //
    // 6th pass: owner-command replies are ALWAYS sent privately to the
    // owner's nick (`from`) instead of `replyTo`. If the owner runs
    // `/users` in a public channel like `#help`, replyTo would be `#help`
    // and the approved-user list would leak to every channel member.
    if (isOwner && isOwnerCommand(text)) {
      const reply = await runOwnerCommand(text, bridge, strings);
      if (reply) {
        sendPrivmsg(from, reply);
      }
      return;
    }

    // User commands
    const cmd = parseUserCommand(text);
    if (cmd === "stop") {
      const reqId = activeRequests.get(from);
      if (reqId) {
        bridge.cancelRequest(reqId);
        activeRequests.delete(from);
        sendPrivmsg(replyTo, strings.stopped);
      }
      return;
    }

    if (!text) return;

    const { text: cleanText, directives } = parseDirectives(text);
    const finalText = cleanText || text;

    const dto: IncomingMessageDto = {
      user_id: from,
      text: finalText,
      attachments: [],
      context: { channel: target, nick: from, directives },
      timestamp: new Date().toISOString(),
    };

    const { requestId, result } = bridge.sendMessage(dto);
    activeRequests.set(from, requestId);

    // No streaming for IRC — wait for full response
    try {
      const response = await result;
      if (response) {
        sendPrivmsg(replyTo, response);
      }
    } catch (err: any) {
      if (err.message !== "cancelled") {
        sendPrivmsg(replyTo, strings.errorMessage(err.message));
      }
    }

    activeRequests.delete(from);
  }

  function connectSocket(resolve?: () => void, reject?: (e: unknown) => void): void {
    console.log(`[irc] connecting to ${server}:${port}...`);
    let connected = false;
    socket = connect(port, server, () => {
      connected = true;
      reconnectAttempts = 0; // reset backoff on a good connection
      if (password) sendRaw(`PASS ${password}`);
      sendRaw(`NICK ${nick}`);
      sendRaw(`USER ${nick} 0 * :OPEX Bot`);
      resolve?.();
    });

    let buffer = "";
    socket.on("data", (data) => {
        buffer += data.toString();
        const lines = buffer.split("\r\n");
        buffer = lines.pop() ?? "";

        for (const line of lines) {
          if (!line) continue;

          // PING/PONG
          if (line.startsWith("PING")) {
            sendRaw(line.replace("PING", "PONG"));
            continue;
          }

          // RPL_WELCOME (001) — join channels
          if (line.includes(" 001 ")) {
            for (const ch of ircChannels) {
              sendRaw(`JOIN ${ch}`);
            }
            console.log(`[irc] connected as ${nick}, joining ${ircChannels.join(", ")}`);
            continue;
          }

          // PRIVMSG
          const privmsgMatch = line.match(/^:([^!]+)!\S+ PRIVMSG (\S+) :(.+)/);
          if (privmsgMatch) {
            const [, from, target, text] = privmsgMatch;
            handleMessage(from, target, text).catch((err) => {
              console.error("[irc] message error:", err);
            });
            continue;
          }

          // CTCP VERSION
          if (line.includes("\x01VERSION\x01")) {
            const fromMatch = line.match(/^:([^!]+)/);
            if (fromMatch) {
              sendRaw(`NOTICE ${fromMatch[1]} :\x01VERSION OPEX IRC 1.0.0\x01`);
            }
          }
        }
      });

    socket.on("error", (err) => {
      console.error("[irc] socket error:", err);
      // Only fail the initial start() promise; a drop AFTER connecting is
      // handled by 'close' → reconnect (F022).
      if (!connected) reject?.(err);
    });

    socket.on("close", () => {
      console.log("[irc] disconnected");
      socket = null;
      activeRequests.clear(); // drop stale in-flight request state
      if (!stopped) {
        // F022: reconnect with capped exponential backoff instead of going
        // silently deaf until a full channels-process restart.
        reconnectAttempts++;
        const delay = Math.min(2 ** Math.min(reconnectAttempts, 6) * 1000, 60_000);
        console.log(`[irc] reconnecting in ${delay}ms (attempt ${reconnectAttempts})`);
        setTimeout(() => {
          if (!stopped) connectSocket();
        }, delay);
      }
    });
  }

  return {
    start: () => new Promise<void>((resolve, reject) => {
      stopped = false;
      reconnectAttempts = 0;
      connectSocket(resolve, reject);
    }),
    stop: async () => {
      stopped = true;
      if (socket) {
        sendRaw("QUIT :Goodbye");
        socket.end();
        socket = null;
      }
    },
    onAction: async (action: import("../bridge").OutboundAction) => {
      const context = action.action.context as Record<string, unknown>;
      const params = action.action.params as Record<string, unknown>;
      const target = context.channel as string;
      if (action.action.action === "send_message" || action.action.action === "reply") {
        const text = commonMarkToIrc(params.text as string);
        for (const part of splitText(text, 450)) {
          sendRaw(`PRIVMSG ${target} :${part}`);
        }
      }
    },
  };
}
