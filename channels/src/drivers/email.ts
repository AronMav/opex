/**
 * Email (IMAP/SMTP) channel driver.
 * Allows the agent to monitor an inbox and reply to emails.
 */

import { ImapFlow } from "imapflow";
import nodemailer from "nodemailer";
import { simpleParser } from "mailparser";
import type { BridgeHandle } from "../bridge";
import type { IncomingMessageDto } from "../types";
import { getStrings } from "../localization";
import { parseDirectives } from "./common";

export function createEmailDriver(
  bridge: BridgeHandle,
  credential: string,
  channelConfig: Record<string, unknown> | undefined,
  language: string,
  _typingMode: string,
) {
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

  const client = new ImapFlow({
    host: imapConfig.host,
    port: imapConfig.port,
    secure: imapConfig.secure,
    auth: imapConfig.auth,
    logger: false,
  });

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
      try {
        await client.connect();
        let lock = await client.getMailboxLock("INBOX");
        try {
          // Search for UNSEEN messages, then fetch each
          const unseenUids = await client.search({ seen: false });
          if (unseenUids.length === 0) { lock.release(); await new Promise(r => setTimeout(r, 30_000)); continue; }
          for await (let msg of client.fetch(unseenUids, { envelope: true, source: true })) {
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
            await client.messageFlagsAdd(msg.uid, ["\\Seen"]);
          }
        } finally {
          lock.release();
        }
        await client.logout();
      } catch (err) {
        console.error("[email] poll error:", err);
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
      try { await client.logout(); } catch {}
    },
    onAction: async (action: any) => {
      const to = action.action.context.from as string;
      const subject = action.action.context.subject as string;
      const messageId = action.action.context.message_id as string;
      
      if (action.action.action === "send_message" || action.action.action === "reply") {
        await sendEmail(to, subject, action.action.params.text as string, messageId);
      }
    },
  };
}
