/**
 * Discord channel driver using discord.js.
 * Port of crates/opex-channel/src/channels/discord.rs
 */

import {
  Client,
  GatewayIntentBits,
  MessageFlags,
  type ApplicationCommandDataResolvable,
  type Message as DMessage,
} from "discord.js";
import type { BridgeHandle, OutboundAction } from "../bridge";
import type { ChannelDriver } from "../session";
import type { IncomingMessageDto, MediaAttachment } from "../types";
import { getStrings, type Strings } from "../localization";
import { splitText, toolEmoji, parseDirectives, parseUserCommand, classifyMediaType, reUploadAttachments, commonMarkToDiscord } from "./common";
import { commandsToDiscord, reconstructCommandText, type ApiCommand } from "./discord-commands";

const STREAM_EDIT_INTERVAL_MS = 1000;
const MAX_MESSAGE_LEN = 1900; // Discord limit ~2000, leave margin

export function createDiscordDriver(
  bridge: BridgeHandle,
  credential: string,
  _channelConfig: Record<string, unknown> | undefined,
  language: string,
  _typingMode: string,
): ChannelDriver {
  const strings = getStrings(language);

  const client = new Client({
    intents: [
      GatewayIntentBits.Guilds,
      GatewayIntentBits.GuildMessages,
      GatewayIntentBits.MessageContent,
      GatewayIntentBits.DirectMessages,
      GatewayIntentBits.GuildMessageReactions,
    ],
  });

  const activeRequests = new Map<string, string>();
  const thinkState = new Set<string>();

  client.on("messageCreate", async (msg: DMessage) => {
    if (msg.author.bot) return;

    const userId = msg.author.id;
    const channelId = msg.channelId;
    const key = `${userId}:${channelId}`;
    const displayName = msg.author.globalName ?? msg.author.username;

    // Access control
    const { allowed, isOwner } = await bridge.checkAccess(userId);

    if (!allowed && !isOwner) {
      const code = await bridge.createPairingCode(userId, displayName);
      await msg.reply(strings.accessRestricted(code)).catch(() => {});
      return;
    }

    // Owner commands
    if (isOwner) {
      const text = msg.content;
      if (text.startsWith("/approve ") || text.startsWith("/reject ") ||
          text === "/users" || text.startsWith("/revoke ")) {
        await handleOwnerCommand(text, bridge, strings, msg);
        return;
      }
    }

    // User commands
    const cmd = parseUserCommand(msg.content);
    if (cmd === "stop") {
      const reqId = activeRequests.get(key);
      if (reqId) {
        bridge.cancelRequest(reqId);
        activeRequests.delete(key);
        await msg.react("🛑").catch(() => {});
      } else {
        await msg.reply(strings.noActiveRequest).catch(() => {});
      }
      return;
    }
    if (cmd === "think") {
      if (thinkState.has(key)) {
        thinkState.delete(key);
        await msg.reply(strings.thinkModeOff).catch(() => {});
      } else {
        thinkState.add(key);
        await msg.reply(strings.thinkModeOn).catch(() => {});
      }
      return;
    }

    // Media attachments
    const attachments: MediaAttachment[] = [];
    for (const att of msg.attachments.values()) {
      attachments.push({
        url: att.url,
        media_type: classifyMediaType(att.contentType ?? undefined),
        file_name: att.name ?? undefined,
        mime_type: att.contentType ?? undefined,
        file_size: att.size,
      });
    }

    const text = msg.content;
    if (!text && attachments.length === 0) return;

    // Parse directives
    const { text: cleanText, directives } = parseDirectives(text);
    const finalText = cleanText || text;

    if (thinkState.has(key)) {
      thinkState.delete(key);
      directives.think = true;
    }

    // Re-upload media. A failed re-upload must not silently drop the whole
    // message — react ❌ and tell the user so they can retry (F082).
    let stableAttachments: typeof attachments;
    try {
      stableAttachments = await reUploadAttachments(bridge, attachments);
    } catch (err: any) {
      console.error("[discord] reUploadAttachments failed:", err?.message ?? err);
      await msg.react("❌").catch(() => {});
      await msg.reply(strings.errorMessage(err?.message ?? "media upload failed")).catch(() => {});
      return;
    }

    const dto: IncomingMessageDto = {
      user_id: userId,
      display_name: displayName,
      text: finalText,
      attachments: stableAttachments,
      context: {
        guild_id: msg.guildId,
        channel_id: channelId,
        message_id: msg.id,
        thread_id: msg.thread?.id,
        directives,
      },
      timestamp: new Date().toISOString(),
    };

    const { requestId, onChunk, onPhase, result } = bridge.sendMessage(dto);
    activeRequests.set(key, requestId);

    // Phase reactions
    onPhase(async (phase, toolName) => {
      try {
        if (phase === "thinking") await msg.react("🤔").catch(() => {});
        else if (phase === "calling_tool") {
          const emoji = toolEmoji(toolName);
          await msg.react(emoji).catch(() => {});
        }
        else if (phase === "composing") await msg.react("⚡").catch(() => {});
      } catch {
        // cosmetic, ok to fail silently
      }
    });

    // Streaming display.
    // streamMsg is assigned inside the setInterval closure below; TS flow
    // analysis can't see assignments across closure boundaries, so we
    // capture into a local at the use-site (see "captured" below).
    let streamMsg: DMessage | null = null;
    let fullText = "";
    let dirty = false;

    onChunk((chunkText) => {
      fullText += chunkText;
      dirty = true;
    });

    const streamTimer = setInterval(async () => {
      if (!dirty || fullText.length > MAX_MESSAGE_LEN) return;
      dirty = false;
      const display = `${fullText}\u258C`;
      try {
        if (!streamMsg) {
          streamMsg = await msg.reply(display);
        } else {
          await streamMsg.edit(display).catch(() => {});
        }
      } catch (e) {
        console.warn("[discord] streaming edit failed:", (e as Error).message?.slice(0, 100));
      }
    }, STREAM_EDIT_INTERVAL_MS);

    try {
      const response = await result;
      clearInterval(streamTimer);

      if (response) {
        // streamMsg is assigned inside a setInterval closure; TS flow analysis
        // can't see that across the closure boundary, so we capture into a
        // non-null local once and use that for editing.
        const captured = streamMsg as DMessage | null;
        if (captured) {
          if (response.length <= MAX_MESSAGE_LEN) {
            await captured.edit(response).catch(() => {});
          } else {
            const parts = splitText(response, MAX_MESSAGE_LEN, true);
            await captured.edit(parts[0]).catch(() => {});
            for (let i = 1; i < parts.length; i++) {
              if (msg.channel.isSendable()) {
                await msg.channel.send(parts[i]).catch(() => {});
              }
            }
          }
        } else {
          const parts = splitText(response, MAX_MESSAGE_LEN, true);
          for (const part of parts) {
            await msg.reply(part).catch(() => {});
          }
        }
        await msg.react("👍").catch(() => {});
        setTimeout(async () => {
          await msg.reactions.removeAll().catch(() => {});
        }, 3000);
      }
    } catch (err: any) {
      clearInterval(streamTimer);
      if (err.message === "cancelled") {
        await msg.react("🛑").catch(() => {});
      } else {
        await msg.react("❌").catch(() => {});
        await msg.reply(strings.errorMessage(err.message)).catch(() => {});
      }
    }

    activeRequests.delete(key);
  });

  // Register the registry's native slash commands with Discord once the
  // client is ready. Fail-soft: registration errors must never crash the
  // driver — Discord already has whatever command menu it had before.
  client.once("ready", async () => {
    try {
      const coreUrl = (process.env.OPEX_CORE_WS || "ws://localhost:18789").replace("ws://", "http://");
      const authToken = process.env.OPEX_AUTH_TOKEN || "";
      const resp = await fetch(
        `${coreUrl}/api/commands?scope=native&lang=${encodeURIComponent(language)}`,
        { headers: { Authorization: `Bearer ${authToken}` }, signal: AbortSignal.timeout(5000) },
      );
      if (resp.ok) {
        const body = (await resp.json()) as { commands?: ApiCommand[] };
        const cmds = commandsToDiscord(body.commands ?? []);
        if (cmds.length && client.application) {
          await client.application.commands
            .set(cmds as unknown as ApplicationCommandDataResolvable[])
            .catch((e) => console.error("[discord] command register failed:", e));
        }
      }
    } catch (e) {
      console.error("[discord] command fetch failed:", e);
    }
  });

  // Native slash command dispatch: reconstruct "/name <args>" text from the
  // interaction's options and route it through the same bridge.sendMessage
  // path messageCreate uses, streaming the response into the deferred reply.
  client.on("interactionCreate", async (interaction) => {
    if (!interaction.isChatInputCommand()) return;

    const userId = interaction.user.id;
    const displayName = interaction.user.globalName ?? interaction.user.username;

    const { allowed, isOwner } = await bridge.checkAccess(userId);
    if (!allowed && !isOwner) {
      const code = await bridge.createPairingCode(userId, displayName);
      await interaction
        .reply({ content: strings.accessRestricted(code), flags: MessageFlags.Ephemeral })
        .catch(() => {});
      return;
    }

    await interaction.deferReply().catch(() => {});

    // Reconstruct "/name <values>" from the provided options (declared order).
    const values: Record<string, string> = {};
    for (const opt of interaction.options.data) {
      if (opt.value != null) values[opt.name] = String(opt.value);
    }
    const text = reconstructCommandText(interaction.commandName, values);

    const dto: IncomingMessageDto = {
      user_id: userId,
      display_name: displayName,
      text,
      attachments: [],
      context: {
        guild_id: interaction.guildId,
        channel_id: interaction.channelId,
      },
      timestamp: new Date().toISOString(),
    };

    const { onChunk, result } = bridge.sendMessage(dto);

    let acc = "";
    onChunk((chunk: string) => {
      acc += chunk;
      interaction.editReply(acc.slice(0, MAX_MESSAGE_LEN) || "…").catch(() => {});
    });

    try {
      const final = await result;
      const out = (final && final.length ? final : acc) || "✓";
      await interaction.editReply(out.slice(0, MAX_MESSAGE_LEN)).catch(() => {});
    } catch (err) {
      await interaction.editReply(strings.errorMessage((err as Error)?.message ?? "error")).catch(() => {});
    }
  });

  return {
    start: async () => {
      await client.login(credential);
      console.log(`[discord] logged in as ${client.user?.tag}`);
    },
    stop: async () => {
      await client.destroy();
    },
    onAction: async (action: OutboundAction) => {
      const context = action.action.context as Record<string, unknown>;
      const params = action.action.params as Record<string, unknown>;
      const channelId = context.channel_id as string;
      const messageId = context.message_id as string | undefined;
      const channel = await client.channels.fetch(channelId);
      // isSendable() narrows to SendableChannels — text-based channels that
      // support .send/.messages/.edit (excludes PartialGroupDMChannel etc.).
      if (!channel?.isSendable()) return;
      const textChannel = channel;

      switch (action.action.action) {
        case "react":
          if (messageId) {
            const m = await textChannel.messages.fetch(messageId);
            await m.react(params.emoji as string);
          }
          break;
        case "send_message":
          await textChannel.send(commonMarkToDiscord(params.text as string));
          break;
        case "reply":
          if (messageId) {
            const m = await textChannel.messages.fetch(messageId);
            await m.reply(commonMarkToDiscord(params.text as string));
          }
          break;
        case "edit":
          if (messageId) {
            const m = await textChannel.messages.fetch(messageId);
            await m.edit(commonMarkToDiscord(params.text as string));
          }
          break;
        case "delete":
          if (messageId) {
            const m = await textChannel.messages.fetch(messageId);
            await m.delete();
          }
          break;
      }
    },
  };
}

async function handleOwnerCommand(
  text: string,
  bridge: BridgeHandle,
  strings: Strings,
  msg: DMessage,
): Promise<void> {
  // Audit 2026-05-08 (7th pass): owner-command replies always go to the
  // owner's DM (`msg.author.send(...)`), never via `msg.reply()` — running
  // `/users` in a public guild channel would otherwise leak the entire
  // approved-user list into that channel.
  const sendOwner = async (out: string) => {
    await msg.author.send(out).catch(() => {});
  };
  const trimmed = text.trim();

  if (trimmed.startsWith("/approve ")) {
    const code = trimmed.slice("/approve ".length).trim();
    const result = await bridge.approvePairing(code);
    await sendOwner(result.success ? strings.userApproved(code) : strings.codeNotFound);
    return;
  }
  if (trimmed.startsWith("/reject ")) {
    const code = trimmed.slice("/reject ".length).trim();
    bridge.rejectPairing(code);
    await sendOwner(strings.requestRejected);
    return;
  }
  if (trimmed === "/users") {
    const users = await bridge.listUsers();
    if (users.length === 0) {
      await sendOwner(strings.noApprovedUsers);
      return;
    }
    let out = strings.approvedUsersHeader;
    for (const u of users) {
      const uid = u.channel_user_id ?? "?";
      const label = u.display_name ?? uid;
      out += strings.userListItem(label, uid, u.approved_at ?? "?");
    }
    out += strings.revokeHint;
    await sendOwner(out);
    return;
  }
  if (trimmed.startsWith("/revoke ")) {
    const targetId = trimmed.slice("/revoke ".length).trim();
    const success = await bridge.revokeUser(targetId);
    await sendOwner(success ? strings.userRevoked(targetId) : strings.userNotFound);
  }
}
