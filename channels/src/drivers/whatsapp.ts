/**
 * WhatsApp Cloud API channel driver.
 * Port of crates/opex-channel/src/channels/whatsapp.rs
 *
 * Uses Meta Graph API v21.0:
 * - Incoming: Bun.serve() webhook
 * - Outgoing: REST API
 */

import { createHmac, timingSafeEqual } from "crypto";
import type { BridgeHandle } from "../bridge";
import type { ChannelDriver } from "../session";
import type { IncomingMessageDto, MediaAttachment } from "../types";
import { getStrings } from "../localization";
import { splitText, parseDirectives, parseUserCommand, commonMarkToWhatsApp } from "./common";

const GRAPH_API_BASE = "https://graph.facebook.com/v21.0";
const MAX_TEXT_LEN = 4096;

export function createWhatsAppDriver(
  bridge: BridgeHandle,
  credential: string,
  channelConfig: Record<string, unknown> | undefined,
  language: string,
  _typingMode: string,
): ChannelDriver {
  const strings = getStrings(language);
  const accessToken = credential;
  const phoneNumberId = (channelConfig?.phone_number_id as string) ?? "";
  const verifyToken = (channelConfig?.verify_token as string) ?? "";
  const webhookPort = (channelConfig?.webhook_port as number) ?? 8443;
  // F017: Meta app secret for X-Hub-Signature-256 verification of inbound
  // webhooks. When set, POSTs with a missing/invalid signature are rejected so
  // an attacker can't spoof msg.from and bypass access control.
  const appSecret = (channelConfig?.app_secret as string) ?? "";

  let server: ReturnType<typeof Bun.serve> | null = null;
  const activeRequests = new Map<string, string>();

  async function graphApi(
    method: string,
    path: string,
    body?: unknown,
  ): Promise<any> {
    const resp = await fetch(`${GRAPH_API_BASE}${path}`, {
      method,
      headers: {
        Authorization: `Bearer ${accessToken}`,
        "Content-Type": "application/json",
      },
      body: body ? JSON.stringify(body) : undefined,
    });
    if (!resp.ok) {
      const text = await resp.text().catch(() => "");
      throw new Error(`WhatsApp API ${resp.status}: ${text}`);
    }
    return resp.json();
  }

  async function sendTextMessage(to: string, text: string): Promise<void> {
    const parts = splitText(text, MAX_TEXT_LEN, false);
    for (const part of parts) {
      await graphApi("POST", `/${phoneNumberId}/messages`, {
        messaging_product: "whatsapp",
        to,
        type: "text",
        text: { body: part },
      });
    }
  }

  async function sendReaction(
    to: string,
    messageId: string,
    emoji: string,
  ): Promise<void> {
    await graphApi("POST", `/${phoneNumberId}/messages`, {
      messaging_product: "whatsapp",
      to,
      type: "reaction",
      reaction: { message_id: messageId, emoji },
    }).catch(() => {});
  }

  async function handleIncomingMessage(
    waId: string,
    messageId: string,
    text: string,
    mediaAttachments: MediaAttachment[],
  ): Promise<void> {
    // Access control
    const { allowed, isOwner } = await bridge.checkAccess(waId);
    if (!allowed && !isOwner) {
      const code = await bridge.createPairingCode(waId, waId);
      await sendTextMessage(waId, strings.accessRestricted(code));
      return;
    }

    // User commands
    const cmd = parseUserCommand(text);
    if (cmd === "stop") {
      const reqId = activeRequests.get(waId);
      if (reqId) {
        bridge.cancelRequest(reqId);
        activeRequests.delete(waId);
        await sendReaction(waId, messageId, "🛑");
      }
      return;
    }

    if (!text && mediaAttachments.length === 0) return;

    const { text: cleanText, directives } = parseDirectives(text);
    const finalText = cleanText || text;

    const dto: IncomingMessageDto = {
      user_id: waId,
      text: finalText,
      attachments: mediaAttachments,
      context: {
        phone_number_id: phoneNumberId,
        wa_id: waId,
        message_id: messageId,
        directives,
      },
      timestamp: new Date().toISOString(),
    };

    const { requestId, result } = bridge.sendMessage(dto);
    activeRequests.set(waId, requestId);

    // No streaming for WhatsApp
    try {
      const response = await result;
      if (response) {
        await sendTextMessage(waId, response);
        await sendReaction(waId, messageId, "👍");
      }
    } catch (err: any) {
      if (err.message !== "cancelled") {
        await sendReaction(waId, messageId, "❌");
        await sendTextMessage(waId, strings.errorMessage(err.message));
      }
    }

    activeRequests.delete(waId);
  }

  return {
    start: async () => {
      server = Bun.serve({
        port: webhookPort,
        async fetch(req) {
          const url = new URL(req.url);

          // Webhook verification (GET)
          if (req.method === "GET" && url.pathname === "/webhook") {
            const mode = url.searchParams.get("hub.mode");
            const token = url.searchParams.get("hub.verify_token");
            const challenge = url.searchParams.get("hub.challenge");
            if (mode === "subscribe" && token === verifyToken) {
              return new Response(challenge ?? "", { status: 200 });
            }
            return new Response("Forbidden", { status: 403 });
          }

          // Webhook events (POST)
          if (req.method === "POST" && url.pathname === "/webhook") {
            // F017: verify the Meta X-Hub-Signature-256 HMAC over the RAW body
            // before trusting any of its contents (msg.from drives access
            // control). Read the raw text so the HMAC matches byte-for-byte.
            const raw = await req.text();
            if (appSecret) {
              const sigHeader = req.headers.get("x-hub-signature-256") ?? "";
              const expected =
                "sha256=" + createHmac("sha256", appSecret).update(raw).digest("hex");
              const a = Buffer.from(sigHeader);
              const b = Buffer.from(expected);
              if (a.length !== b.length || !timingSafeEqual(a, b)) {
                console.warn("[whatsapp] rejected webhook POST: bad X-Hub-Signature-256");
                return new Response("Forbidden", { status: 403 });
              }
            } else {
              console.warn(
                "[whatsapp] app_secret not configured — webhook signature verification DISABLED (spoofable); set channelConfig.app_secret",
              );
            }
            try {
              const body = JSON.parse(raw);
              const entries = body?.entry ?? [];
              for (const entry of entries) {
                const changes = entry?.changes ?? [];
                for (const change of changes) {
                  const messages = change?.value?.messages ?? [];
                  for (const msg of messages) {
                    const waId = msg.from;
                    const messageId = msg.id;

                    if (msg.type === "text") {
                      handleIncomingMessage(waId, messageId, msg.text?.body ?? "", []).catch(
                        (err) => console.error("[whatsapp] message error:", err),
                      );
                    } else if (msg.type === "image" || msg.type === "audio" || msg.type === "video" || msg.type === "document") {
                      const mediaObj = msg[msg.type];
                      const mediaType = msg.type === "image" ? "image"
                        : msg.type === "audio" ? "audio"
                        : msg.type === "video" ? "video"
                        : "document";

                      // Download media from Graph API
                      let mediaUrl = "";
                      try {
                        const mediaInfo = await graphApi("GET", `/${mediaObj.id}`);
                        mediaUrl = mediaInfo.url;
                      } catch {}

                      const att: MediaAttachment = {
                        url: mediaUrl,
                        media_type: mediaType,
                        mime_type: mediaObj.mime_type,
                        file_name: mediaObj.filename,
                      };

                      const caption = msg.caption ?? "";
                      handleIncomingMessage(waId, messageId, caption, [att]).catch(
                        (err) => console.error("[whatsapp] media error:", err),
                      );
                    }
                  }
                }
              }
            } catch (err) {
              console.error("[whatsapp] webhook parse error:", err);
            }
            return new Response("OK", { status: 200 });
          }

          return new Response("Not Found", { status: 404 });
        },
      });
      console.log(`[whatsapp] webhook server on :${webhookPort}`);
    },
    stop: async () => {
      server?.stop();
    },
    onAction: async (action: import("../bridge").OutboundAction) => {
      const context = action.action.context as Record<string, unknown>;
      const params = action.action.params as Record<string, unknown>;
      const waId = context.wa_id as string;
      if (action.action.action === "send_message" || action.action.action === "reply") {
        await sendTextMessage(waId, commonMarkToWhatsApp(params.text as string));
      } else if (action.action.action === "react") {
        const messageId = context.message_id as string;
        if (messageId) await sendReaction(waId, messageId, params.emoji as string);
      }
    },
  };
}
