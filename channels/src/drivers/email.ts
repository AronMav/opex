/**
 * Email (IMAP/SMTP) channel driver.
 * Allows the agent to monitor an inbox and reply to emails.
 */

import { ImapFlow } from "imapflow";
import nodemailer from "nodemailer";
import { simpleParser } from "mailparser";
import type { BridgeHandle, OutboundAction } from "../bridge";
import type { ChannelDriver } from "../session";
import type { IncomingMessageDto } from "../types";
import { getStrings } from "../localization";
import { parseDirectives } from "./common";

export function createEmailDriver(
  bridge: BridgeHandle,
  credential: string,
  channelConfig: Record<string, unknown> | undefined,
  language: string,
  _typingMode: string,
): ChannelDriver {
  const strings = getStrings(language);
  const password = credential;
  
  const imapConfig = {
    host: (channelConfig?.imap_host as string) || "imap.gmail.com",
    port: (channelConfig?.imap_port as number) || 993,
    secure: true,
    auth: {
      user: (channelConfig?.imap_user as string) || "",
      pass: password,
    },
  };

  const smtpConfig = {
    host: (channelConfig?.smtp_host as string) || "smtp.gmail.com",
    port: (channelConfig?.smtp_port as number) || 465,
    secure: true,
    auth: {
      user: (channelConfig?.smtp_user as string) || imapConfig.auth.user,
      pass: password,
    },
  };

  // F021: build a FRESH ImapFlow per connection attempt (below) rather than
  // reusing one module-scoped client. ImapFlow cannot reconnect an already-open
  // client, so a single shared instance left half-open by an error would make
  // every future connect() fail and silently kill the channel until restart.
  const makeClient = () =>
    new ImapFlow({
      host: imapConfig.host,
      port: imapConfig.port,
      secure: imapConfig.secure,
      auth: imapConfig.auth,
      logger: false,
    });
  let client: ImapFlow | null = null;

  const transporter = nodemailer.createTransport(smtpConfig);
  let stopPolling = false;

  async function sendEmail(to: string, subject: string, text: string, inReplyTo?: string) {
    await transporter.sendMail({
      from: imapConfig.auth.user,
      to,
      subject: subject.startsWith("Re:") ? subject : `Re: ${subject}`,
      text,
      inReplyTo,
      references: inReplyTo,
    });
  }

  async function pollInbox() {
    while (!stopPolling) {
      client = makeClient();
      const c = client;
      try {
        await c.connect();
        let lock = await c.getMailboxLock("INBOX");
        try {
          // Search for UNSEEN messages, then fetch each. ImapFlow returns
          // `false` when the search yields no results — narrow that out.
          const unseenUids = await c.search({ seen: false });
          if (Array.isArray(unseenUids) && unseenUids.length > 0) {
            for await (let msg of c.fetch(unseenUids, { envelope: true, source: true })) {
              // F020: isolate each message. Without this, ANY failure (agent
              // error, transient SMTP send failure) escaped before the \Seen
              // flag was set, so the same message was re-fetched every 30s
              // forever — a poison pill that also starved every message behind
              // it. Mark \Seen even on failure so a bad message can't loop.
              try {
                if (!msg.source || !msg.envelope) continue;
                const parsed = await simpleParser(msg.source);
                const from = parsed.from?.value[0]?.address || "";
                const subject = parsed.subject || "No Subject";
                const text = parsed.text || "";
                const messageId = msg.envelope.messageId;

                if (!from || !text) continue;

                // Access control
                const { allowed, isOwner } = await bridge.checkAccess(from);
                if (!allowed && !isOwner) {
                  const code = await bridge.createPairingCode(from, from);
                  await sendEmail(from, subject, strings.accessRestricted(code), messageId);
                  await c.messageFlagsAdd(msg.uid, ["\\Seen"]);
                  continue;
                }

                const { text: cleanText, directives } = parseDirectives(text);
                const dto: IncomingMessageDto = {
                  user_id: from,
                  display_name: parsed.from?.value[0]?.name || from,
                  text: cleanText || text,
                  attachments: [],
                  context: {
                    subject,
                    message_id: messageId,
                    directives,
                  },
                  timestamp: new Date().toISOString(),
                };

                const { result } = bridge.sendMessage(dto);
                const response = await result;
                if (response) {
                  await sendEmail(from, subject, response, messageId);
                }

                // Mark as read
                await c.messageFlagsAdd(msg.uid, ["\\Seen"]);
              } catch (msgErr) {
                console.error("[email] message processing failed:", msgErr);
                // Best-effort: flag \Seen so a poison message isn't reprocessed
                // every poll. Errors here are swallowed (already in a failure path).
                try { await c.messageFlagsAdd(msg.uid, ["\\Seen"]); } catch {}
              }
            }
          }
        } finally {
          lock.release();
        }
      } catch (err) {
        console.error("[email] poll error:", err);
      } finally {
        // F021: ALWAYS tear the connection down, even on error, so a broken /
        // half-open client can't wedge every future poll.
        try {
          await c.logout();
        } catch {
          try { c.close(); } catch {}
        }
        if (client === c) client = null;
      }
      // Wait 30s before next poll
      await new Promise(r => setTimeout(r, 30000));
    }
  }

  return {
    start: async () => {
      pollInbox().catch(err => console.error("[email] background loop error:", err));
      console.log(`[email] monitoring ${imapConfig.auth.user}`);
    },
    stop: async () => {
      stopPolling = true;
      try { if (client) await client.logout(); } catch {}
    },
    onAction: async (action: OutboundAction) => {
      const context = action.action.context as Record<string, unknown>;
      const params = action.action.params as Record<string, unknown>;
      const to = context.from as string;
      const subject = context.subject as string;
      const messageId = context.message_id as string;

      if (action.action.action === "send_message" || action.action.action === "reply") {
        await sendEmail(to, subject, params.text as string, messageId);
      }
    },
  };
}
