/**
 * IRC channel driver using plain TCP.
 * Port of crates/hydeclaw-channel/src/channels/irc.rs
 */

import { connect, type Socket } from "net";
import type { BridgeHandle } from "../bridge";
import type { ChannelDriver } from "../session";
import type { IncomingMessageDto } from "../types";
import { getStrings } from "../localization";
import { splitText, parseDirectives, parseUserCommand, commonMarkToIrc } from "./common";

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

  return {
    start: () => new Promise<void>((resolve, reject) => {
      console.log(`[irc] connecting to ${server}:${port}...`);
      socket = connect(port, server, () => {
        if (password) sendRaw(`PASS ${password}`);
        sendRaw(`NICK ${nick}`);
        sendRaw(`USER ${nick} 0 * :HydeClaw Bot`);
        resolve();
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
              sendRaw(`NOTICE ${fromMatch[1]} :\x01VERSION HydeClaw IRC 1.0.0\x01`);
            }
          }
        }
      });

      socket.on("error", (err) => {
        console.error("[irc] socket error:", err);
        reject(err);
      });

      socket.on("close", () => {
        console.log("[irc] disconnected");
      });
    }),
    stop: async () => {
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
