/**
 * Telegram channel driver using grammy.
 * Port of crates/hydeclaw-channel/src/channels/telegram.rs
 */

import { Bot, Context, InlineKeyboard } from "grammy";
import type { Message } from "grammy/types";
import type { BridgeHandle, OutboundAction, UserEntry } from "../bridge";
import type { IncomingMessageDto, MediaAttachment } from "../types";
import { getStrings, type Strings } from "../localization";
import { splitText, toolEmoji, parseDirectives, parseUserCommand, reUploadAttachments, commonMarkToMarkdownV2, isTgPermanentError, extractTgErrorCode, extractTgRetryAfter, exponentialDelay, chatCooldownKey } from "./common";
import * as fs from "fs";
import * as path from "path";

/** Builds reply_parameters with allow_sending_without_reply for resilient replies */
function safeReplyParams(messageId: number | undefined): { message_id: number; allow_sending_without_reply: true } | undefined {
  return messageId ? { message_id: messageId, allow_sending_without_reply: true } : undefined;
}

const QUEUE_FILE = path.join(process.cwd(), ".pending-queue.json");

interface PersistedQueueItem {
  userId: string;
  chatId: number;
  text: string;
  attachments: MediaAttachment[];
}

function persistQueue(pendingQueue: Map<string, QueuedMessage[]>) {
  try {
    const data: Record<string, PersistedQueueItem[]> = {};
    for (const [key, items] of pendingQueue) {
      data[key] = items.map((q) => ({
        userId: q.userId, chatId: q.chatId, text: q.text, attachments: q.attachments,
      }));
    }
    fs.writeFileSync(QUEUE_FILE, JSON.stringify(data));
  } catch (e) {
    console.error("[queue] failed to persist queue:", e);
  }
}

function restoreQueue(): Map<string, PersistedQueueItem[]> {
  try {
    const raw = fs.readFileSync(QUEUE_FILE, "utf-8");
    fs.unlinkSync(QUEUE_FILE);
    return new Map(Object.entries(JSON.parse(raw)));
  } catch { return new Map(); }
}

// ── Per-chat cooldown map ──────────────────────────────────────────────

/** Per-chat/topic delivery error cooldown. Key: "${chatId}:${threadId??''}". */
interface CooldownEntry {
  errorCode: number;
  cooldownUntil: number; // Date.now() + ms
}
const chatCooldowns = new Map<string, CooldownEntry>();

function isChatOnCooldown(chatId: number, threadId: number | undefined, errorCode: number): boolean {
  const key = chatCooldownKey(chatId, threadId);
  const entry = chatCooldowns.get(key);
  if (!entry) return false;
  if (Date.now() > entry.cooldownUntil) {
    chatCooldowns.delete(key);
    return false;
  }
  return entry.errorCode === errorCode;
}

function setChatCooldown(chatId: number, threadId: number | undefined, errorCode: number, ms: number): void {
  chatCooldowns.set(chatCooldownKey(chatId, threadId), {
    errorCode,
    cooldownUntil: Date.now() + ms,
  });
}

async function retryTg<T>(
  fn: () => Promise<T>,
  attempts = 3,
  label = "",
  chatId?: number,
  threadId?: number,
  errorCooldownMs = 60_000,
): Promise<T | undefined> {
  for (let i = 0; i < attempts; i++) {
    try {
      return await fn();
    } catch (e) {
      const errorCode = extractTgErrorCode(e) ?? 0;

      if (isTgPermanentError(e)) {
        console.warn(`[tg] ${label} permanent error (${errorCode}), not retrying:`, e);
        if (chatId !== undefined) {
          setChatCooldown(chatId, threadId, errorCode, errorCooldownMs);
        }
        return undefined;
      }

      // 429: respect retry_after
      if (errorCode === 429) {
        const retryAfter = extractTgRetryAfter(e);
        const delay = retryAfter ? retryAfter * 1000 : errorCooldownMs;
        console.warn(`[tg] ${label} rate limited, waiting ${delay}ms`);
        if (chatId !== undefined) {
          setChatCooldown(chatId, threadId, 429, delay);
        }
        if (i < attempts - 1) {
          await Bun.sleep(delay);
          continue;
        }
        return undefined;
      }

      if (i === attempts - 1) {
        console.warn(`[tg] ${label} failed after ${attempts} attempts:`, e);
        return undefined;
      }

      await Bun.sleep(exponentialDelay(i));
    }
  }
  return undefined;
}

const DEBOUNCE_DELAY_MS = 1500;
const STREAM_EDIT_INTERVAL_MS = 1000;
const MAX_MESSAGE_LEN = 3600;

interface DebounceEntry {
  texts: string[];
  attachments: MediaAttachment[];
  msg: Message;
  userId: string;
  chatId: number;
  timer: ReturnType<typeof setTimeout>;
}

interface QueuedMessage {
  msg: Message;
  userId: string;
  chatId: number;
  text: string;
  attachments: MediaAttachment[];
}

interface ActiveState {
  activeRequests: Map<string, string>; // `${userId}:${chatId}` → requestId
  thinkState: Set<string>; // `${userId}:${chatId}`
  debounce: Map<string, DebounceEntry>; // `${userId}:${chatId}`
  pendingQueue: Map<string, QueuedMessage[]>; // queue per user:chat key
}

export function createTelegramDriver(
  bridge: BridgeHandle,
  credential: string,
  channelConfig: Record<string, unknown> | undefined,
  language: string,
  typingMode: string,
): { start: () => Promise<void>; stop: () => Promise<void> } {
  const strings = getStrings(language);
  const groupMode = (channelConfig?.group_mode as string) ?? "mention";
  const apiUrl = channelConfig?.api_url as string | undefined;
  const errorCooldownMs = (channelConfig?.error_cooldown_ms as number) ?? 60_000;
  const errorPolicy = (channelConfig?.error_policy as string) ?? "suppress_repeated";

  const bot = new Bot(credential, apiUrl ? { client: { apiRoot: apiUrl } } : undefined);

  // Track forum topics that have already been renamed (resets on restart — fine, Telegram keeps the name)
  const renamedTopics = new Set<string>();

  const state: ActiveState = {
    activeRequests: new Map(),
    thinkState: new Set(),
    debounce: new Map(),
    pendingQueue: new Map(),
  };

  let botUsername = "";

  // Set bot commands
  bot.api.setMyCommands([
    { command: "help", description: strings.cmdHelp },
    { command: "status", description: strings.cmdStatus },
    { command: "memory", description: strings.cmdMemory },
    { command: "new", description: strings.cmdNew },
    { command: "compact", description: strings.cmdCompact },
    { command: "stop", description: strings.cmdStop },
    { command: "think", description: strings.cmdThink },
  ]).catch(() => {});

  // Message handler
  bot.on("message", async (ctx) => {
    const msg = ctx.message;
    const userId = msg.from?.id?.toString() ?? "";
    const chatId = msg.chat.id;
    const key = `${userId}:${chatId}`;

    // Self-chat dedupe: skip messages from the bot itself (prevents infinite loops in groups)
    if (msg.from?.is_bot && msg.from?.id === bot.botInfo.id) return;
    if ((msg as any).via_bot?.id === bot.botInfo.id) return;

    // Auto-rename forum topic on first message in topic
    if (msg.message_thread_id && !msg.reply_to_message) {
      const topicKey = `${chatId}:${msg.message_thread_id}`;
      if (!renamedTopics.has(topicKey)) {
        renamedTopics.add(topicKey);
        const topicName = (msg.text || msg.caption || "").slice(0, 50).trim();
        if (topicName) {
          bot.api.editForumTopic(chatId, msg.message_thread_id, { name: topicName })
            .catch(() => {}); // Best-effort, don't block
        }
      }
    }

    // Group filtering
    const isGroup = msg.chat.type === "group" || msg.chat.type === "supergroup";
    if (isGroup) {
      if (groupMode === "off") return;
      if (groupMode !== "always") {
        // "mention" mode: only respond to mentions or replies to bot
        const text = msg.text ?? msg.caption ?? "";
        const mentioned = botUsername && text.toLowerCase().includes(`@${botUsername}`);
        const isReplyToBot = msg.reply_to_message?.from?.username?.toLowerCase() === botUsername;
        if (!mentioned && !isReplyToBot) return;
      }
    }

    // Access control
    const { allowed, isOwner } = await bridge.checkAccess(userId);

    if (!allowed && !isOwner) {
      const displayName = [msg.from?.first_name, msg.from?.last_name].filter(Boolean).join(" ") || userId;
      const code = await bridge.createPairingCode(userId, displayName);

      await ctx.reply(strings.accessRestricted(code), {
        parse_mode: "MarkdownV2",
        reply_parameters: safeReplyParams(msg.message_id),
      }).catch(() => {});

      // Notify owner
      if (bridge.ownerId) {
        const ownerChatId = Number(bridge.ownerId);
        if (!isNaN(ownerChatId)) {
          await bot.api.sendMessage(
            ownerChatId,
            strings.accessRequest(displayName, userId, code),
          ).catch(() => {});
        }
      }
      return;
    }

    // Owner commands
    if (isOwner) {
      const text = msg.text ?? msg.caption ?? "";
      const ownerResult = await handleOwnerCommand(text, bridge, strings, ctx);
      if (ownerResult) return;
    }

    // User slash commands (local)
    if (msg.text) {
      const cmd = parseUserCommand(msg.text);
      if (cmd === "stop") {
        const reqId = state.activeRequests.get(key);
        if (reqId) {
          bridge.cancelRequest(reqId);
          state.activeRequests.delete(key);
          await setReaction(ctx, "🛑");
        } else {
          await ctx.reply(strings.noActiveRequest, {
            reply_parameters: safeReplyParams(msg.message_id),
          }).catch(() => {});
        }
        return;
      }
      if (cmd === "think") {
        if (state.thinkState.has(key)) {
          state.thinkState.delete(key);
          await ctx.reply(strings.thinkModeOff, {
            reply_parameters: safeReplyParams(msg.message_id),
          }).catch(() => {});
        } else {
          state.thinkState.add(key);
          await ctx.reply(strings.thinkModeOn, {
            reply_parameters: safeReplyParams(msg.message_id),
          }).catch(() => {});
        }
        return;
      }
      // "help" falls through to core
    }

    // Media attachments
    const attachments: MediaAttachment[] = [];
    const fileId = getFileId(msg);
    if (fileId) {
      try {
        const file = await bot.api.getFile(fileId);
        const url = `https://api.telegram.org/file/bot${credential}/${file.file_path}`;
        attachments.push({
          url,
          media_type: getMediaType(msg),
          file_name: getFileName(msg),
          mime_type: getMimeType(msg),
          file_size: file.file_size,
        });
      } catch (err) {
        console.error("Failed to get file URL:", err);
        await setReaction(ctx, "❌");
        return;
      }
    }

    let text = msg.text ?? msg.caption ?? "";
    // Strip @botname from group messages so LLM sees clean text
    if (isGroup && botUsername) {
      text = text.replace(new RegExp(`@${botUsername}\\b`, "gi"), "").trim();
    }
    if (!text && attachments.length === 0) return;

    // Don't debounce commands (messages starting with /)
    const isCommand = text.startsWith("/");

    // Debounce rapid messages (merge texts + attachments, use last msg for context)
    if (!isCommand) {
      const entry = state.debounce.get(key);
      if (entry) {
        clearTimeout(entry.timer);
        if (text) entry.texts.push(text);
        entry.attachments.push(...attachments);
        entry.msg = msg; // Always use the LAST message for reply context
        entry.timer = setTimeout(() => {
          const e = state.debounce.get(key);
          if (!e) return;
          state.debounce.delete(key);
          const mergedText = e.texts.join("\n");
          dispatchOrQueue(bot, e.msg, e.userId, e.chatId, mergedText, e.attachments, bridge, strings, state, errorCooldownMs, errorPolicy);
        }, DEBOUNCE_DELAY_MS);
        return;
      }

      const timer = setTimeout(() => {
        const e = state.debounce.get(key);
        if (!e) return;
        state.debounce.delete(key);
        const mergedText = e.texts.join("\n");
        dispatchOrQueue(bot, e.msg, e.userId, e.chatId, mergedText, e.attachments, bridge, strings, state, errorCooldownMs, errorPolicy);
      }, DEBOUNCE_DELAY_MS);

      state.debounce.set(key, { texts: text ? [text] : [], attachments: [...attachments], msg, userId, chatId, timer });
      return;
    }

    await dispatchOrQueue(bot, msg, userId, chatId, text, attachments, bridge, strings, state, errorCooldownMs, errorPolicy);
  });

  // Callback query handler (inline buttons)
  bot.on("callback_query:data", async (ctx) => {
    const data = ctx.callbackQuery.data;
    const userId = ctx.callbackQuery.from.id.toString();
    const chatId = ctx.callbackQuery.message?.chat.id;
    const msgId = ctx.callbackQuery.message?.message_id;

    // Handle approval callbacks (approve:UUID / reject:UUID) via HTTP to Core API
    const approveMatch = data.match(/^(approve|reject):(.+)$/);
    if (approveMatch) {
      const [, action, approvalId] = approveMatch;
      const status = action === "approve" ? "approved" : "rejected";
      const label = status === "approved" ? strings.approvalApproved : strings.approvalRejected;

      // Answer callback immediately to stop Telegram spinner
      await ctx.answerCallbackQuery({ text: label }).catch(() => {});

      // Call Core API to resolve approval
      const coreUrl = (process.env.HYDECLAW_CORE_WS || "ws://localhost:18789").replace("ws://", "http://");
      const authToken = process.env.HYDECLAW_AUTH_TOKEN || "";
      try {
        const resp = await fetch(`${coreUrl}/api/approvals/${approvalId}/resolve`, {
          method: "POST",
          headers: { "Content-Type": "application/json", "Authorization": `Bearer ${authToken}` },
          body: JSON.stringify({ status, resolved_by: userId }),
          signal: AbortSignal.timeout(5000),
        });
        if (!resp.ok) {
          const err = await resp.text().catch(() => "unknown error");
          console.error(`[tg] approval resolve failed: ${resp.status} ${err}`);
        }
      } catch (err) {
        console.error("[tg] approval HTTP error:", err);
      }

      // Update message: replace buttons text with result, remove keyboard
      if (chatId && msgId) {
        const origText = ctx.callbackQuery.message?.text || "";
        // Replace the 🔐 header with result
        const resultText = origText.replace(/^🔐[^\n]*/, label);
        await retryTg(() => bot.api.editMessageText(chatId, msgId, resultText)).catch(() => {});
      }
      return;
    }

    // Non-approval callbacks — access check required before forwarding.
    const { allowed, isOwner } = await bridge.checkAccess(userId);
    if (!allowed && !isOwner) {
      const displayName = ctx.callbackQuery.from.first_name || userId;
      const code = await bridge.createPairingCode(userId, displayName);
      await ctx.answerCallbackQuery().catch(() => {});
      await ctx.reply(strings.accessRestricted(code), { parse_mode: "MarkdownV2" }).catch(() => {});
      return;
    }

    await ctx.answerCallbackQuery().catch(() => {});
    bridge.sendMessage({
      user_id: userId,
      text: data,
      attachments: [],
      context: { chat_id: chatId, message_id: msgId, callback_query_id: ctx.callbackQuery.id, is_callback: true },
      timestamp: new Date().toISOString(),
    });
  });

  // Inline mode: @bot query from any chat
  bot.on("inline_query", async (ctx) => {
    const query = ctx.inlineQuery.query.trim();
    if (!query || query.length < 2) {
      await ctx.answerInlineQuery([]);
      return;
    }

    const userId = ctx.inlineQuery.from.id.toString();
    const access = await bridge.checkAccess(userId);
    if (!access.allowed) {
      await ctx.answerInlineQuery([{
        type: "article",
        id: "denied",
        title: "Access denied",
        input_message_content: { message_text: "Access denied" },
      }]);
      return;
    }

    try {
      const { result } = bridge.sendMessage({
        user_id: userId,
        text: query,
        attachments: [],
        context: { inline: true },
        timestamp: new Date().toISOString(),
      });

      const timeout = new Promise<string>((_, reject) =>
        setTimeout(() => reject(new Error("timeout")), 15000)
      );
      const response = await Promise.race([result, timeout]);
      const text = (response || "No response").slice(0, 4096);

      await ctx.answerInlineQuery([{
        type: "article",
        id: `inline-${Date.now()}`,
        title: query.slice(0, 64),
        description: text.slice(0, 128),
        input_message_content: { message_text: text },
      }], { cache_time: 60 });
    } catch {
      await ctx.answerInlineQuery([{
        type: "article",
        id: "error",
        title: "Error",
        input_message_content: { message_text: "Failed to get response. Try again." },
      }]);
    }
  });

  return {
    start: async () => {
      const me = await bot.api.getMe();
      botUsername = (me.username ?? "").toLowerCase();
      console.log(`[telegram] bot @${botUsername} starting...`);
      bot.start({ drop_pending_updates: true });
    },
    stop: async () => {
      await bot.stop();
    },
    onAction: async (action: OutboundAction) => {
      await executeAction(bot, action.actionId, action.action, strings);
    },
  };
}

// ── Dispatch or queue ───────────────────────────────────────────────────

/** Dispatch to processMessage, or queue if agent is busy with this key. */
async function dispatchOrQueue(
  bot: Bot,
  msg: Message,
  userId: string,
  chatId: number,
  text: string,
  attachments: MediaAttachment[],
  bridge: BridgeHandle,
  strings: Strings,
  state: ActiveState,
  errorCooldownMs = 60_000,
  errorPolicy = "suppress_repeated",
): Promise<void> {
  const key = `${userId}:${chatId}`;

  if (state.activeRequests.has(key)) {
    // Agent busy — accumulate all queued messages (merged on drain)
    const existing = state.pendingQueue.get(key) ?? [];
    existing.push({ msg, userId, chatId, text, attachments });
    state.pendingQueue.set(key, existing);
    persistQueue(state.pendingQueue);
    await bot.api.setMessageReaction(
      chatId, msg.message_id,
      [{ type: "emoji", emoji: "⏳" as any }],
    ).catch(() => {});
    return;
  }

  await processMessage(bot, msg, userId, chatId, text, attachments, bridge, strings, state, errorCooldownMs, errorPolicy);
}

// ── Process message ─────────────────────────────────────────────────────

async function processMessage(
  bot: Bot,
  msg: Message,
  userId: string,
  chatId: number,
  text: string,
  attachments: MediaAttachment[],
  bridge: BridgeHandle,
  strings: Strings,
  state: ActiveState,
  errorCooldownMs = 60_000,
  errorPolicy = "suppress_repeated",
): Promise<void> {
  const key = `${userId}:${chatId}`;
  const threadId = msg.message_thread_id;

  // Check if chat has an active error cooldown (suppress_repeated policy)
  if (errorPolicy !== "always_retry") {
    const cooldownKey = chatCooldownKey(chatId, threadId);
    const entry = chatCooldowns.get(cooldownKey);
    if (entry && Date.now() < entry.cooldownUntil) {
      console.warn(`[tg] chat ${chatId} on cooldown until ${new Date(entry.cooldownUntil).toISOString()}, skipping`);
      return;
    }
  }

  // Send typing indicator
  await retryTg(() => bot.api.sendChatAction(chatId, "typing"), 3, "sendChatAction", chatId, threadId, errorCooldownMs);

  // Immediate ack — user sees we received their message.
  // Deliberately placed BEFORE reUploadAttachments so the ack is instant,
  // even if media re-upload is slow.
  await bot.api.setMessageReaction(chatId, msg.message_id, [{ type: "emoji", emoji: "👀" as any }]).catch(() => {});

  // Re-upload media for stable URLs
  const stableAttachments = await reUploadAttachments(bridge, attachments);

  // Parse directives
  const { text: cleanText, directives } = parseDirectives(text);
  const finalText = cleanText || text;

  // Check think-state toggle
  if (state.thinkState.has(key)) {
    state.thinkState.delete(key);
    directives.think = true;
  }

  const dto: IncomingMessageDto = {
    user_id: userId,
    display_name: [msg.from?.first_name, msg.from?.last_name].filter(Boolean).join(" ") || undefined,
    text: finalText,
    attachments: stableAttachments,
    context: {
      chat_id: chatId,
      message_id: msg.message_id,
      reply_to_message_id: msg.reply_to_message?.message_id,
      thread_id: msg.message_thread_id,
      is_group: msg.chat.type === "group" || msg.chat.type === "supergroup",
      directives,
    },
    timestamp: new Date().toISOString(),
  };

  const { requestId, onChunk, onPhase, result } = bridge.sendMessage(dto);
  state.activeRequests.set(key, requestId);

  // Phase reactor (reactions + stall detection)
  let phaseTimer: ReturnType<typeof setInterval> | null = null;
  let lastPhaseTime = Date.now();
  let stallLevel = 0;

  onPhase(async (phase, toolName) => {
    lastPhaseTime = Date.now();
    stallLevel = 0;
    try {
      if (phase === "thinking") {
        await bot.api.setMessageReaction(chatId, msg.message_id, [{ type: "emoji", emoji: "🤔" }]).catch(() => {});
      } else if (phase === "calling_tool") {
        const emoji = toolEmoji(toolName);
        await bot.api.setMessageReaction(chatId, msg.message_id, [{ type: "emoji", emoji: emoji as any }]).catch(() => {});
        await retryTg(() => bot.api.sendChatAction(chatId, "typing"), 3, "sendChatAction");
      } else if (phase === "composing") {
        await bot.api.setMessageReaction(chatId, msg.message_id, [{ type: "emoji", emoji: "⚡" }]).catch(() => {});
      }
    } catch {
      // cosmetic, ok to fail silently
    }
  });

  phaseTimer = setInterval(async () => {
    const elapsed = Date.now() - lastPhaseTime;
    if (elapsed >= 30_000 && stallLevel < 2) {
      stallLevel = 2;
      await bot.api.setMessageReaction(chatId, msg.message_id, [{ type: "emoji", emoji: "😨" }]).catch(() => {});
    } else if (elapsed >= 10_000 && stallLevel < 1) {
      stallLevel = 1;
      await bot.api.setMessageReaction(chatId, msg.message_id, [{ type: "emoji", emoji: "🥱" }]).catch(() => {});
    }
  }, 5000);

  // Streaming display
  let streamMsgId: number | null = null;
  let fullText = "";
  let dirty = false;
  let lastStreamEdit: Promise<unknown> = Promise.resolve();
  let streamStopped = false;

  let lastChunkAt = Date.now();
  onChunk((chunkText) => {
    fullText += chunkText;
    dirty = true;
    lastChunkAt = Date.now();
  });

  const streamTimer = setInterval(() => {
    if (streamStopped || !dirty || fullText.length > MAX_MESSAGE_LEN) return;
    // Don't create a streaming message for empty/whitespace-only text (avoids blank placeholder during tool calls)
    if (streamMsgId === null && fullText.trim().length === 0) return;
    dirty = false;
    const display = `${fullText}\u258C`;
    lastStreamEdit = (async () => {
      try {
        if (streamMsgId === null) {
          const sent = await bot.api.sendMessage(chatId, display, {
            reply_parameters: safeReplyParams(msg.message_id),
          });
          streamMsgId = sent.message_id;
        } else {
          await retryTg(() => bot.api.editMessageText(chatId, streamMsgId!, display), 3, "editMessageText");
        }
      } catch (e) {
        console.warn("[telegram] streaming edit failed:", (e as Error).message?.slice(0, 100));
      }
    })();
  }, STREAM_EDIT_INTERVAL_MS);

  const stallCheck = setInterval(() => {
    if (Date.now() - lastChunkAt > 60_000 && !streamStopped) {
      streamStopped = true;
      clearInterval(stallCheck);
      console.warn("[tg] stream stalled for 60s, aborting");
      const notice = fullText ? fullText + "\n\n\u26A0\uFE0F Stream stalled" : "\u26A0\uFE0F Response timed out";
      if (streamMsgId) editWithMarkdown(bot, chatId, streamMsgId, notice).catch(() => {});
    }
  }, 10_000);

  // Wait for final result
  try {
    const response = await result;
    streamStopped = true;
    clearInterval(streamTimer);
    clearInterval(stallCheck);
    if (phaseTimer) clearInterval(phaseTimer);
    // Wait for last streaming edit to finish before sending formatted version
    await lastStreamEdit.catch(() => {});

    if (response) {
      if (streamMsgId) {
        // Edit final text with markdown
        await editWithMarkdown(bot, chatId, streamMsgId, response);
      } else {
        await sendMarkdownReply(bot, chatId, msg.message_id, response);
      }

      // Thumbs up → clear after 3s
      await bot.api.setMessageReaction(chatId, msg.message_id, [{ type: "emoji", emoji: "👍" }]).catch(() => {});
      setTimeout(async () => {
        await bot.api.setMessageReaction(chatId, msg.message_id, []).catch(() => {});
      }, 3000);
    }
  } catch (err: any) {
    clearInterval(streamTimer);
    clearInterval(stallCheck);
    if (phaseTimer) clearInterval(phaseTimer);

    if (err.message === "cancelled") {
      await bot.api.setMessageReaction(chatId, msg.message_id, [{ type: "emoji", emoji: "🛑" as any }]).catch(() => {});
    } else {
      await bot.api.setMessageReaction(chatId, msg.message_id, [{ type: "emoji", emoji: "❌" }]).catch(() => {});
      const isGroup = chatId < 0;
      await bot.api.sendMessage(chatId, strings.errorMessage(err.message), {
        reply_parameters: safeReplyParams(msg.message_id),
        disable_notification: isGroup,
      }).catch(() => {});
    }
  }

  state.activeRequests.delete(key);

  // Drain: merge ALL queued messages and process
  const queued = state.pendingQueue.get(key);
  if (queued && queued.length > 0) {
    state.pendingQueue.delete(key);
    persistQueue(state.pendingQueue);
    // Merge all queued messages
    const merged = queued.map((q) => q.text).filter(Boolean).join("\n\n---\n\n");
    const last = queued[queued.length - 1];
    const allAttachments = queued.flatMap((q) => q.attachments);
    await bot.api.setMessageReaction(last.chatId, last.msg.message_id, []).catch(() => {});
    setImmediate(() => {
      processMessage(bot, last.msg, last.userId, last.chatId, merged, allAttachments, bridge, strings, state, errorCooldownMs, errorPolicy).catch(() => {});
    });
  }
}

// ── Markdown helpers ────────────────────────────────────────────────────

async function sendMarkdownReply(
  bot: Bot,
  chatId: number,
  replyTo: number,
  text: string,
): Promise<void> {
  const parts = splitText(text, MAX_MESSAGE_LEN, true);
  for (let i = 0; i < parts.length; i++) {
    const replyParams = i === 0 ? safeReplyParams(replyTo) : undefined;
    // Try MarkdownV2 first, fallback to plain text
    const md2 = commonMarkToMarkdownV2(parts[i]);
    try {
      await bot.api.sendMessage(chatId, md2, {
        parse_mode: "MarkdownV2",
        reply_parameters: replyParams,
      });
    } catch (e: any) {
      console.warn(`[telegram] MarkdownV2 send failed: ${e.message?.slice(0, 200)}`);
      await bot.api.sendMessage(chatId, parts[i], {
        reply_parameters: replyParams,
      }).catch(() => {});
    }

    if (i < parts.length - 1) {
      await Bun.sleep(100);
    }
  }
}

async function editWithMarkdown(
  bot: Bot,
  chatId: number,
  messageId: number,
  text: string,
): Promise<void> {
  if (text.length <= MAX_MESSAGE_LEN) {
    const md2 = commonMarkToMarkdownV2(text);
    try {
      await bot.api.editMessageText(chatId, messageId, md2, { parse_mode: "MarkdownV2" });
    } catch (e: any) {
      console.warn(`[telegram] MarkdownV2 edit failed: ${e.message?.slice(0, 200)}`);
      await bot.api.editMessageText(chatId, messageId, text).catch(() => {});
    }
    return;
  }

  // Split and edit first, send rest
  const parts = splitText(text, MAX_MESSAGE_LEN, true);
  const firstMd2 = commonMarkToMarkdownV2(parts[0]);
  try {
    await bot.api.editMessageText(chatId, messageId, firstMd2, { parse_mode: "MarkdownV2" });
  } catch (e: any) {
    console.warn(`[telegram] MarkdownV2 edit (split) failed: ${e.message?.slice(0, 200)}`);
    await bot.api.editMessageText(chatId, messageId, parts[0]).catch(() => {});
  }
  for (let i = 1; i < parts.length; i++) {
    const md2 = commonMarkToMarkdownV2(parts[i]);
    try {
      await bot.api.sendMessage(chatId, md2, { parse_mode: "MarkdownV2" });
    } catch (e: any) {
      console.warn(`[telegram] MarkdownV2 send (split) failed: ${e.message?.slice(0, 200)}`);
      await bot.api.sendMessage(chatId, parts[i]).catch(() => {});
    }
    await Bun.sleep(100);
  }
}

// ── Owner commands ──────────────────────────────────────────────────────

async function handleOwnerCommand(
  text: string,
  bridge: BridgeHandle,
  strings: Strings,
  ctx: Context,
): Promise<boolean> {
  const trimmed = text.trim();

  if (trimmed.startsWith("/approve ")) {
    const code = trimmed.slice("/approve ".length).trim();
    const result = await bridge.approvePairing(code);
    const reply = result.success
      ? strings.userApproved(result.error ?? code)
      : (result.error === "expired" ? strings.codeExpired : strings.codeNotFound);
    await ctx.reply(reply, {
      reply_parameters: safeReplyParams(ctx.message?.message_id),
    }).catch(() => {});
    return true;
  }

  if (trimmed.startsWith("/reject ")) {
    const code = trimmed.slice("/reject ".length).trim();
    bridge.rejectPairing(code);
    await ctx.reply(strings.requestRejected, {
      reply_parameters: safeReplyParams(ctx.message?.message_id),
    }).catch(() => {});
    return true;
  }

  if (trimmed === "/users") {
    const users = await bridge.listUsers();
    if (users.length === 0) {
      await ctx.reply(strings.noApprovedUsers, {
        reply_parameters: safeReplyParams(ctx.message?.message_id),
      }).catch(() => {});
      return true;
    }
    let out = strings.approvedUsersHeader;
    for (const u of users) {
      const uid = u.channel_user_id ?? "?";
      const label = u.display_name ?? uid;
      const date = u.approved_at ?? "?";
      out += strings.userListItem(label, uid, date);
    }
    out += strings.revokeHint;
    await ctx.reply(out, {
      reply_parameters: safeReplyParams(ctx.message?.message_id),
    }).catch(() => {});
    return true;
  }

  if (trimmed.startsWith("/revoke ")) {
    const targetId = trimmed.slice("/revoke ".length).trim();
    const success = await bridge.revokeUser(targetId);
    const reply = success ? strings.userRevoked(targetId) : strings.userNotFound;
    await ctx.reply(reply, {
      reply_parameters: safeReplyParams(ctx.message?.message_id),
    }).catch(() => {});
    return true;
  }

  return false;
}

// ── Telegram helpers ────────────────────────────────────────────────────

async function setReaction(ctx: Context, emoji: string): Promise<void> {
  try {
    await ctx.api.setMessageReaction(
      ctx.message!.chat.id,
      ctx.message!.message_id,
      [{ type: "emoji", emoji: emoji as any }],
    );
  } catch {
    // cosmetic, ok to fail silently
  }
}

function getFileId(msg: Message): string | undefined {
  if (msg.voice) return msg.voice.file_id;
  if (msg.audio) return msg.audio.file_id;
  if (msg.photo) return msg.photo[msg.photo.length - 1]?.file_id;
  if (msg.video) return msg.video.file_id;
  if (msg.video_note) return msg.video_note.file_id;
  if (msg.document) return msg.document.file_id;
  if (msg.sticker) return msg.sticker.file_id;
  return undefined;
}

function getMediaType(msg: Message): import("../types").MediaType {
  if (msg.voice || msg.audio) return "audio";
  if (msg.photo) return "image";
  if (msg.video || msg.video_note) return "video";
  if (msg.document) return "document";
  if (msg.sticker) return "image";
  return "document";
}

function getFileName(msg: Message): string | undefined {
  if (msg.audio?.file_name) return msg.audio.file_name;
  if (msg.video?.file_name) return msg.video.file_name;
  if (msg.document?.file_name) return msg.document.file_name;
  return undefined;
}

function getMimeType(msg: Message): string | undefined {
  if (msg.voice) return "audio/ogg";
  if (msg.audio?.mime_type) return msg.audio.mime_type;
  if (msg.photo) return "image/jpeg";
  if (msg.video?.mime_type) return msg.video.mime_type;
  if (msg.video_note) return "video/mp4";
  if (msg.document?.mime_type) return msg.document.mime_type;
  if (msg.sticker) return "image/webp";
  return undefined;
}

// ── Actions ─────────────────────────────────────────────────────────────

async function executeAction(
  bot: Bot,
  actionId: string,
  action: { action: string; params: Record<string, unknown>; context: Record<string, unknown> },
  strings?: Strings,
): Promise<void> {
  const chatId = action.context?.chat_id as number | undefined;
  const messageId = action.context?.message_id as number | undefined;

  if (!chatId) {
    console.error(`[tg] executeAction: no chat_id in context for action=${action.action}`, JSON.stringify(action.context).slice(0, 200));
    return;
  }

  switch (action.action) {
      case "react":
        if (messageId) {
          await bot.api.setMessageReaction(chatId, messageId, [
            { type: "emoji", emoji: (action.params.emoji as string) as any },
          ]);
        }
        break;

      case "pin":
        if (messageId) {
          await bot.api.pinChatMessage(chatId, messageId);
        }
        break;

      case "edit":
        if (messageId) {
          const editText = action.params.text as string;
          const editMd2 = commonMarkToMarkdownV2(editText);
          try {
            await bot.api.editMessageText(chatId, messageId, editMd2, { parse_mode: "MarkdownV2" });
          } catch (e) {
            console.warn("[telegram] action edit MarkdownV2 failed:", (e as Error).message?.slice(0, 100));
            await bot.api.editMessageText(chatId, messageId, editText).catch(() => {});
          }
        }
        break;

      case "delete":
        if (messageId) {
          await bot.api.deleteMessage(chatId, messageId);
        }
        break;

      case "reply": {
        const replyText = action.params.text as string;
        const replyMd2 = commonMarkToMarkdownV2(replyText);
        try {
          await bot.api.sendMessage(chatId, replyMd2, {
            parse_mode: "MarkdownV2",
            reply_parameters: safeReplyParams(messageId),
          });
        } catch (e) {
          console.warn("[telegram] action reply MarkdownV2 failed:", (e as Error).message?.slice(0, 100));
          await bot.api.sendMessage(chatId, replyText, {
            reply_parameters: safeReplyParams(messageId),
          }).catch(() => {});
        }
        break;
      }

      case "send_message": {
        const text = action.params.text as string;
        const parts = splitText(text, MAX_MESSAGE_LEN, true);
        for (const part of parts) {
          const md2 = commonMarkToMarkdownV2(part);
          try {
            await bot.api.sendMessage(chatId, md2, { parse_mode: "MarkdownV2" });
          } catch (e) {
            console.warn("[telegram] action send_message MarkdownV2 failed:", (e as Error).message?.slice(0, 100));
            await bot.api.sendMessage(chatId, part).catch(() => {});
          }
        }
        break;
      }

      case "send_voice": {
        const audioData = action.params.audio_data as string;
        if (audioData) {
          const buffer = Buffer.from(audioData, "base64");
          const blob = new Blob([buffer], { type: "audio/ogg" });
          const file = new File([blob], "voice.ogg", { type: "audio/ogg" });
          await bot.api.sendVoice(chatId, file, {
            reply_parameters: safeReplyParams(messageId),
          });
        }
        break;
      }

      case "send_photo": {
        const url = action.params.url as string;
        if (url) {
          await bot.api.sendPhoto(chatId, url, {
            caption: action.params.caption as string | undefined,
            reply_parameters: safeReplyParams(messageId),
          });
        }
        break;
      }

      case "approval_request": {
        const toolName = action.params.tool_name as string;
        const args = action.params.args as Record<string, unknown> | undefined;
        const approvalId = action.params.approval_id as string;

        // Format args as readable lines
        let argsText = "";
        if (args && typeof args === "object") {
          const lines = Object.entries(args)
            .map(([k, v]) => {
              const val = typeof v === "string"
                ? (v.length > 80 ? v.slice(0, 77) + "…" : v)
                : JSON.stringify(v);
              return `  • ${k}: ${val}`;
            });
          if (lines.length > 0) argsText = "\n" + lines.join("\n");
        }

        if (!strings) { console.error("[tg] approval_request requires strings"); break; }
        const s = strings;
        const text = `${s.approvalHeader(toolName)}${argsText}`;
        const keyboard = new InlineKeyboard()
          .text(s.approvalApprove, `approve:${approvalId}`).row()
          .text(s.approvalReject, `reject:${approvalId}`);

        await bot.api.sendMessage(chatId, text, {
          reply_markup: keyboard,
          reply_parameters: safeReplyParams(messageId),
        });
        break;
      }

      case "send_buttons": {
        const buttons = action.params.buttons as Array<{ text: string; data: string }>;
        if (buttons) {
          const keyboard = new InlineKeyboard();
          for (const btn of buttons) {
            keyboard.text(btn.text, btn.data);
          }
          await bot.api.sendMessage(chatId, action.params.text as string ?? strings.choose, {
            reply_markup: keyboard,
            reply_parameters: safeReplyParams(messageId),
          });
        }
        break;
      }
  }
}
