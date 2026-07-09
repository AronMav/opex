/**
 * Slack channel driver using @slack/bolt (Socket Mode).
 * Port of crates/opex-channel/src/channels/slack.rs
 */

import { App } from "@slack/bolt";
import type { BridgeHandle, OutboundAction } from "../bridge";
import type { ChannelDriver } from "../session";
import type { IncomingMessageDto, MediaAttachment } from "../types";
import { getStrings, type Strings } from "../localization";
import {
  splitText,
  toolEmoji,
  parseDirectives,
  parseUserCommand,
  emojiToSlackShortcode,
  classifyMediaType,
  reUploadAttachments,
  commonMarkToSlack,
} from "./common";
import { isOwnerCommand, runOwnerCommand } from "../owner-commands";

const STREAM_EDIT_INTERVAL_MS = 1000;
const MAX_MESSAGE_LEN = 3000;

export function createSlackDriver(
  bridge: BridgeHandle,
  credential: string,
  channelConfig: Record<string, unknown> | undefined,
  language: string,
  _typingMode: string,
): ChannelDriver {
  const strings = getStrings(language);
  const appToken = (channelConfig?.app_token as string) ?? "";
  const botToken = credential;

  const app = new App({
    token: botToken,
    appToken,
    socketMode: true,
  });

  const activeRequests = new Map<string, string>();
  const thinkState = new Set<string>();

  app.message(async ({ message, say, client }) => {
    if (!("text" in message) || message.subtype) return;
    const msg = message as any;
    const userId = msg.user;
    const channelId = msg.channel;
    const threadTs = msg.thread_ts ?? msg.ts;
    const key = `${userId}:${channelId}`;

    // Access control
    const { allowed, isOwner } = await bridge.checkAccess(userId);
    if (!allowed && !isOwner) {
      const code = await bridge.createPairingCode(userId, userId);
      await say({ text: strings.accessRestricted(code), thread_ts: threadTs });
      return;
    }

    const text = msg.text ?? "";

    // Owner commands (audit 2026-05-08, group DD): /approve, /reject, /users,
    // /revoke. Without this branch the owner could not bootstrap pairing
    // requests over Slack.
    //
    // 7th pass: owner-command replies always go via DM
    // (`client.chat.postMessage({channel: userId, ...})`), never `say()` —
    // running `/users` in a public channel would otherwise leak the entire
    // approved-user list into that channel's thread.
    if (isOwner && isOwnerCommand(text)) {
      const reply = await runOwnerCommand(text, bridge, strings);
      if (reply) {
        await client.chat.postMessage({ channel: userId, text: reply }).catch(() => {});
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
        await client.reactions.add({ channel: channelId, name: "octagonal_sign", timestamp: msg.ts }).catch(() => {});
      }
      return;
    }
    if (cmd === "think") {
      if (thinkState.has(key)) {
        thinkState.delete(key);
        await say({ text: strings.thinkModeOff, thread_ts: threadTs });
      } else {
        thinkState.add(key);
        await say({ text: strings.thinkModeOn, thread_ts: threadTs });
      }
      return;
    }

    // Media (file attachments)
    const attachments: MediaAttachment[] = [];
    if (msg.files) {
      for (const f of msg.files) {
        attachments.push({
          url: f.url_private,
          media_type: classifyMediaType(f.mimetype),
          file_name: f.name,
          mime_type: f.mimetype,
          file_size: f.size,
        });
      }
    }

    if (!text && attachments.length === 0) return;

    const { text: cleanText, directives } = parseDirectives(text);
    const finalText = cleanText || text;

    if (thinkState.has(key)) {
      thinkState.delete(key);
      directives.think = true;
    }

    // Re-upload media. A failed re-upload must not silently drop the whole
    // message — react x and tell the user so they can retry (F082).
    let stableAttachments: MediaAttachment[];
    try {
      stableAttachments = await reUploadAttachments(bridge, attachments, `Bearer ${botToken}`);
    } catch (err: any) {
      console.error("[slack] reUploadAttachments failed:", err?.message ?? err);
      await client.reactions.add({ channel: channelId, name: "x", timestamp: msg.ts }).catch(() => {});
      await say({ text: strings.errorMessage(err?.message ?? "media upload failed"), thread_ts: threadTs }).catch(() => {});
      return;
    }

    const dto: IncomingMessageDto = {
      user_id: userId,
      text: finalText,
      attachments: stableAttachments,
      context: {
        channel: channelId,
        ts: msg.ts,
        thread_ts: threadTs,
        directives,
      },
      timestamp: new Date().toISOString(),
    };

    const { requestId, onChunk, onPhase, result } = bridge.sendMessage(dto);
    activeRequests.set(key, requestId);

    // Phase reactions
    onPhase(async (phase, toolName) => {
      try {
        let emoji = "fire";
        if (phase === "thinking") emoji = "thinking_face";
        else if (phase === "calling_tool") emoji = emojiToSlackShortcode(toolEmoji(toolName));
        else if (phase === "composing") emoji = "zap";
        await client.reactions.add({ channel: channelId, name: emoji, timestamp: msg.ts }).catch(() => {});
      } catch {
        // cosmetic, ok to fail silently
      }
    });

    // Streaming
    let streamTs: string | null = null;
    let fullText = "";
    let dirty = false;

    onChunk((chunk) => { fullText += chunk; dirty = true; });

    const streamTimer = setInterval(async () => {
      if (!dirty || fullText.length > MAX_MESSAGE_LEN) return;
      dirty = false;
      const display = `${fullText}\u258C`;
      try {
        if (!streamTs) {
          const resp = await client.chat.postMessage({
            channel: channelId,
            text: display,
            thread_ts: threadTs,
          });
          streamTs = resp.ts ?? null;
        } else {
          await client.chat.update({
            channel: channelId,
            ts: streamTs,
            text: display,
          }).catch(() => {});
        }
      } catch (e) {
        console.warn("[slack] streaming edit failed:", (e as Error).message?.slice(0, 100));
      }
    }, STREAM_EDIT_INTERVAL_MS);

    try {
      const response = await result;
      clearInterval(streamTimer);

      if (response) {
        if (streamTs) {
          if (response.length <= MAX_MESSAGE_LEN) {
            await client.chat.update({ channel: channelId, ts: streamTs, text: response }).catch(() => {});
          } else {
            const parts = splitText(response, MAX_MESSAGE_LEN, true);
            await client.chat.update({ channel: channelId, ts: streamTs, text: parts[0] }).catch(() => {});
            for (let i = 1; i < parts.length; i++) {
              await client.chat.postMessage({ channel: channelId, text: parts[i], thread_ts: threadTs }).catch(() => {});
            }
          }
        } else {
          const parts = splitText(response, MAX_MESSAGE_LEN, true);
          for (const part of parts) {
            await say({ text: part, thread_ts: threadTs });
          }
        }
        await client.reactions.add({ channel: channelId, name: "thumbsup", timestamp: msg.ts }).catch(() => {});
      }
    } catch (err: any) {
      clearInterval(streamTimer);
      if (err.message !== "cancelled") {
        await client.reactions.add({ channel: channelId, name: "x", timestamp: msg.ts }).catch(() => {});
      }
    }

    activeRequests.delete(key);
  });

  return {
    start: async () => {
      await app.start();
      console.log("[slack] Socket Mode connected");
    },
    stop: async () => {
      await app.stop();
    },
    onAction: async (action: OutboundAction) => {
      const context = action.action.context as Record<string, unknown>;
      const params = action.action.params as Record<string, unknown>;
      const channelId = context.channel as string;
      const ts = context.ts as string | undefined;
      const threadTs = context.thread_ts as string | undefined;

      switch (action.action.action) {
        case "react":
          if (ts) {
            const shortcode = emojiToSlackShortcode(params.emoji as string);
            await app.client.reactions.add({ channel: channelId, name: shortcode, timestamp: ts });
          }
          break;
        case "send_message":
          await app.client.chat.postMessage({
            channel: channelId,
            text: commonMarkToSlack(params.text as string),
            thread_ts: threadTs,
          });
          break;
        case "edit":
          if (ts) {
            await app.client.chat.update({
              channel: channelId,
              ts,
              text: commonMarkToSlack(params.text as string),
            });
          }
          break;
        case "delete":
          if (ts) {
            await app.client.chat.delete({ channel: channelId, ts });
          }
          break;
      }
    },
  };
}
