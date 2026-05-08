/**
 * Matrix channel driver using fetch (Matrix Client-Server API).
 * Port of crates/hydeclaw-channel/src/channels/matrix.rs
 */

import type { BridgeHandle } from "../bridge";
import type { ChannelDriver } from "../session";
import type { IncomingMessageDto } from "../types";
import { getStrings } from "../localization";
import { splitText, parseDirectives, parseUserCommand } from "./common";
import { isOwnerCommand, runOwnerCommand } from "../owner-commands";

const SYNC_TIMEOUT_MS = 30000;
const MAX_MESSAGE_LEN = 4000;

export function createMatrixDriver(
  bridge: BridgeHandle,
  credential: string,
  channelConfig: Record<string, unknown> | undefined,
  language: string,
  _typingMode: string,
): ChannelDriver {
  const strings = getStrings(language);
  const homeserver = (channelConfig?.homeserver as string) ?? "https://matrix.org";
  const botUserId = (channelConfig?.user_id as string) ?? "";
  const accessToken = credential;

  let running = true;
  let syncToken: string | null = null;

  const activeRequests = new Map<string, string>();

  async function matrixApi(method: string, path: string, body?: unknown): Promise<any> {
    const url = `${homeserver.replace(/\/$/, "")}${path}`;
    const resp = await fetch(url, {
      method,
      headers: {
        Authorization: `Bearer ${accessToken}`,
        "Content-Type": "application/json",
      },
      body: body ? JSON.stringify(body) : undefined,
    });
    if (!resp.ok) throw new Error(`Matrix API ${resp.status}: ${await resp.text()}`);
    return resp.json();
  }

  async function sendMessage(roomId: string, text: string, replyTo?: string): Promise<string> {
    const txnId = `${Date.now()}-${Math.random().toString(36).slice(2)}`;
    const content: any = {
      msgtype: "m.text",
      body: text,
    };
    if (replyTo) {
      content["m.relates_to"] = { "m.in_reply_to": { event_id: replyTo } };
    }
    const encoded = encodeURIComponent(roomId);
    const data = await matrixApi(
      "PUT",
      `/_matrix/client/v3/rooms/${encoded}/send/m.room.message/${txnId}`,
      content,
    );
    return data.event_id;
  }

  async function editMessage(roomId: string, eventId: string, newText: string): Promise<void> {
    const txnId = `${Date.now()}-${Math.random().toString(36).slice(2)}`;
    const encoded = encodeURIComponent(roomId);
    await matrixApi(
      "PUT",
      `/_matrix/client/v3/rooms/${encoded}/send/m.room.message/${txnId}`,
      {
        msgtype: "m.text",
        body: `* ${newText}`,
        "m.new_content": { msgtype: "m.text", body: newText },
        "m.relates_to": { rel_type: "m.replace", event_id: eventId },
      },
    );
  }

  async function sendReaction(roomId: string, eventId: string, emoji: string): Promise<void> {
    const txnId = `${Date.now()}-${Math.random().toString(36).slice(2)}`;
    const encoded = encodeURIComponent(roomId);
    await matrixApi(
      "PUT",
      `/_matrix/client/v3/rooms/${encoded}/send/m.reaction/${txnId}`,
      {
        "m.relates_to": {
          rel_type: "m.annotation",
          event_id: eventId,
          key: emoji,
        },
      },
    ).catch(() => {});
  }

  async function handleRoomMessage(roomId: string, event: any): Promise<void> {
    if (event.sender === botUserId) return;
    if (event.content?.msgtype !== "m.text") return;

    const userId = event.sender;
    const text = event.content.body as string;
    const eventId = event.event_id;
    const key = `${userId}:${roomId}`;

    // Access control
    const { allowed, isOwner } = await bridge.checkAccess(userId);
    if (!allowed && !isOwner) {
      const code = await bridge.createPairingCode(userId, userId);
      await sendMessage(roomId, strings.accessRestricted(code), eventId).catch(() => {});
      return;
    }

    // Owner commands (audit 2026-05-08, group DD).
    //
    // 7th pass: owner-command replies are gated to rooms with ≤ 2 joined
    // members (i.e. DM with the bot). Running `/users` in a public room
    // would otherwise leak the full approved-user list to every member.
    // For safety we ping the joined-members count first; on any error we
    // refuse to reply (fail-closed).
    if (isOwner && isOwnerCommand(text)) {
      const reply = await runOwnerCommand(text, bridge, strings);
      if (reply) {
        let isDm = false;
        try {
          const encoded = encodeURIComponent(roomId);
          const members = await matrixApi(
            "GET",
            `/_matrix/client/v3/rooms/${encoded}/joined_members`,
          );
          isDm = Object.keys(members.joined ?? {}).length <= 2;
        } catch {
          isDm = false;
        }
        if (isDm) {
          await sendMessage(roomId, reply, eventId).catch(() => {});
        }
        // In a multi-user room we silently refuse rather than DM the owner —
        // creating an ad-hoc DM room is an architectural change for later.
      }
      return;
    }

    // User commands
    const cmd = parseUserCommand(text);
    if (cmd === "stop") {
      const reqId = activeRequests.get(key);
      if (reqId) {
        bridge.cancelRequest(reqId);
        activeRequests.delete(key);
        await sendReaction(roomId, eventId, "🛑");
      }
      return;
    }

    if (!text) return;

    const { text: cleanText, directives } = parseDirectives(text);
    const finalText = cleanText || text;

    const dto: IncomingMessageDto = {
      user_id: userId,
      text: finalText,
      attachments: [],
      context: { room_id: roomId, event_id: eventId, directives },
      timestamp: new Date().toISOString(),
    };

    const { requestId, onChunk, result } = bridge.sendMessage(dto);
    activeRequests.set(key, requestId);

    // Streaming via m.replace
    let streamEventId: string | null = null;
    let fullText = "";

    onChunk((chunk) => { fullText += chunk; });

    const streamTimer = setInterval(async () => {
      if (!fullText || fullText.length > MAX_MESSAGE_LEN) return;
      try {
        if (!streamEventId) {
          streamEventId = await sendMessage(roomId, `${fullText}\u258C`, eventId);
        } else {
          await editMessage(roomId, streamEventId, `${fullText}\u258C`);
        }
      } catch {}
    }, 1000);

    try {
      const response = await result;
      clearInterval(streamTimer);

      if (response) {
        if (streamEventId) {
          await editMessage(roomId, streamEventId, response);
        } else {
          const parts = splitText(response, MAX_MESSAGE_LEN, true);
          for (const part of parts) {
            await sendMessage(roomId, part, eventId).catch(() => {});
          }
        }
      }
    } catch (err: any) {
      clearInterval(streamTimer);
      await sendReaction(roomId, eventId, "❌");
    }

    activeRequests.delete(key);
  }

  return {
    start: async () => {
      console.log(`[matrix] starting sync for ${botUserId}...`);

      // Initial sync to get since token
      const initial = await matrixApi("GET", `/_matrix/client/v3/sync?timeout=0`);
      syncToken = initial.next_batch;

      // Long-poll sync loop
      while (running) {
        try {
          const url = `/_matrix/client/v3/sync?timeout=${SYNC_TIMEOUT_MS}${syncToken ? `&since=${syncToken}` : ""}`;
          const data = await matrixApi("GET", url);
          syncToken = data.next_batch;

          // Process room events
          const rooms = data.rooms?.join ?? {};
          for (const [roomId, room] of Object.entries(rooms) as any[]) {
            const events = room.timeline?.events ?? [];
            for (const event of events) {
              if (event.type === "m.room.message") {
                await handleRoomMessage(roomId, event).catch((err) => {
                  console.error(`[matrix] message error:`, err);
                });
              }
            }
          }
        } catch (err) {
          console.error("[matrix] sync error:", err);
          await Bun.sleep(5000);
        }
      }
    },
    stop: async () => {
      running = false;
    },
    onAction: async (action: import("../bridge").OutboundAction) => {
      const context = action.action.context as Record<string, unknown>;
      const params = action.action.params as Record<string, unknown>;
      const roomId = context.room_id as string;
      switch (action.action.action) {
        case "send_message":
          await matrixApi("PUT", `/_matrix/client/v3/rooms/${encodeURIComponent(roomId)}/send/m.room.message/${Date.now()}`, {
            msgtype: "m.text",
            body: params.text as string,
          });
          break;
        case "react": {
          const eventId = context.event_id as string;
          if (eventId) {
            await matrixApi("PUT", `/_matrix/client/v3/rooms/${encodeURIComponent(roomId)}/send/m.reaction/${Date.now()}`, {
              "m.relates_to": { rel_type: "m.annotation", event_id: eventId, key: params.emoji as string },
            });
          }
          break;
        }
      }
    },
  };
}
